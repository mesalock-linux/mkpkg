use failure::Error;
use git2::build::{CheckoutBuilder, RepoBuilder};
use git2::{self, FetchOptions, RemoteCallbacks, Repository};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::header::{AcceptRanges, ByteRangeSpec, ContentLength, ContentRange, ContentRangeSpec,
                      Headers, Range, RangeUnit};
use reqwest::{self, Client};
use url::Url;

use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::time::{Duration, Instant};

use package::{BuildFile, PackageError};
use progress::{InitFn, IterFn};
use util::{self, path_to_string, UtilError};

use super::Config;

#[derive(Debug, Fail)]
pub enum NetworkError {
    #[fail(display = "could not create directory '{}': {}", _0, _1)]
    CreateDir(String, #[cause] io::Error),

    #[fail(display = "could not remove directory '{}': {}", _0, _1)]
    RemoveDir(String, #[cause] io::Error),

    #[fail(display = "could not get real path for file '{}': {}", _0, _1)]
    Canonicalize(String, #[cause] io::Error),

    #[fail(display = "could not create file '{}': {}", _0, _1)]
    TargetFile(String, #[cause] io::Error),

    #[fail(display = "could not read metadata for file '{}': {}", _0, _1)]
    Metadata(String, #[cause] io::Error),

    #[fail(display = "failed to download data from '{}': {}", _0, _1)]
    Download(Url, #[cause] io::Error),

    #[fail(display = "failed to write to '{}': {}", _0, _1)]
    Write(String, #[cause] io::Error),

    #[fail(display = "'{}' is an invalid source file path", _0)]
    InvalidSource(String),

    #[fail(display = "{}", _0)]
    Util(#[cause] UtilError),

    #[fail(display = "{}", _0)]
    Package(#[cause] PackageError),

    #[fail(display = "invalid scheme for the URL '{}'", _0)]
    UnknownScheme(Url),

    #[fail(display = "unknown fragment type for the URL '{}'", _0)]
    UnknownFragment(Url),

    #[fail(display = "failed to download '{}': {}", _0, _1)]
    Git(String, #[cause] git2::Error),

    #[fail(display = "failed to download '{}': {}", _0, _1)]
    Reqwest(String, #[cause] reqwest::Error),
}

pub(crate) struct Downloader {
    client: Client,
}

impl Downloader {
    const WAIT_TIME_MILLIS: u64 = 250;
    const WAIT_TIME_NANOS: u32 = Self::WAIT_TIME_MILLIS as u32 * 1_000_000;

    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    pub fn download_setup<'a>(
        &'a self,
        _config: &Config,
        pkgs: &[BuildFile],
    ) -> (Box<InitFn<'a>>, Box<IterFn<'a>>) {
        let pkgslen = pkgs.len();

        let init_fn = move |total_bar: &ProgressBar, bar: &ProgressBar| {
            bar.set_style(self.bar_style());

            total_bar.set_length(pkgslen as u64);
        };

        let iter_fn = move |config: &Config,
                            pkg: &BuildFile,
                            progbar: &ProgressBar,
                            _total_bar: &ProgressBar,
                            add_error: &Fn(Error)| {
            let inner = || -> Result<(), NetworkError> {
                let download_dir = pkg.download_dir(config);
                fs::create_dir_all(&download_dir)
                    .map_err(|e| NetworkError::CreateDir(path_to_string(&download_dir), e))?;

                for (i, url) in pkg.source().iter().enumerate() {
                    progbar.set_prefix(&format!("{}/{}", pkg.name(), i + 1));
                    progbar.set_position(0);

                    if let Err(f) = self.download(&progbar, pkg, config, url) {
                        add_error(f.into());
                    }
                }

                Ok(())
            };
            inner().map_err(|e| e.into())
        };

        (Box::new(init_fn), Box::new(iter_fn))
    }

    // TODO: check if download of correct file has already occurred and ensure the downloaded file
    //       is complete/not corrupted (to do so we would need some sort of checksums).  if so, we
    //       can skip that download
    fn download(
        &self,
        progbar: &ProgressBar,
        pkg: &BuildFile,
        config: &Config,
        src_item: &str,
    ) -> Result<(), NetworkError> {
        if let Ok(mut url) = Url::parse(src_item) {
            let filename = BuildFile::file_path(src_item).map_err(|e| NetworkError::Package(e))?;

            match url.scheme() {
                "http" | "https" => {
                    // as we require git URLs to be prefixed with "git+", this should be fine
                    self.download_http(progbar, pkg, config, &url, &filename)
                }
                "git+http" | "git+https" | "git" | "git+ssh" => {
                    // can only be git (if it's a valid source URL)
                    if url.scheme() != "git" {
                        let real_scheme = url.scheme()[4..].to_owned();
                        url.set_scheme(&real_scheme)
                            .map_err(|_| NetworkError::UnknownScheme(url.clone()))?;
                    }
                    self.download_git(progbar, pkg, config, &mut url, &filename)
                }
                _ => Err(NetworkError::UnknownScheme(url.clone())),
            }
        } else {
            // assuming that the item is just a file
            let pkgbuild_dir = pkg.pkgbuild_dir(config);
            let pkgbuild_dir = pkgbuild_dir
                .canonicalize()
                .map_err(|e| NetworkError::Canonicalize(path_to_string(pkgbuild_dir), e))?;

            let filepath = pkgbuild_dir.join(pkg.parent_dir()).join(src_item);
            let filepath = filepath
                .canonicalize()
                .map_err(|e| NetworkError::Canonicalize(path_to_string(&filepath), e))?;

            // ensure that the build file isn't trying to use system files as "sources"
            if filepath.starts_with(pkgbuild_dir) {
                // TODO: probably check for config.clobber
                let target_path = pkg.download_dir(config);
                util::copy_dir(&filepath, &target_path).map_err(|e| NetworkError::Util(e))
            } else {
                Err(NetworkError::InvalidSource(path_to_string(&filepath)))
            }
        }
    }

    // NOTE: unfortunately, libgit2 does not seem to support shallow clones, so projects with huge
    //       histories (e.g. glibc) will download very, very slowly
    // XXX: resolving deltas is very slow for some reason.  not sure if it's just libgit2 or due to
    //      the progress bar setup (it's libgit2)
    fn download_git(
        &self,
        progbar: &ProgressBar,
        pkg: &BuildFile,
        config: &Config,
        url: &mut Url,
        filename: &str,
    ) -> Result<(), NetworkError> {
        progbar.set_style(self.git_counting_style());

        let mut progress_check = Instant::now() - Duration::from_millis(Self::WAIT_TIME_MILLIS);
        let mut sideband_check = progress_check.clone();
        let mut deltas = false;
        let mut objects = false;
        let progress_cb = |progress: git2::Progress| {
            self.perform_gradually(&mut progress_check, || {
                if progress.total_objects() == progress.received_objects() {
                    if !deltas {
                        progbar.set_style(self.git_delta_style());
                        progbar.set_length(progress.total_deltas() as u64);
                        deltas = true;
                    }

                    progbar.set_position(progress.indexed_deltas() as u64);
                } else {
                    if !objects {
                        progbar.set_style(self.git_object_style());
                        progbar.set_length(progress.total_objects() as u64);
                        objects = true;
                    }

                    progbar.set_position(progress.received_objects() as u64);
                }
            });
            true
        };

        let sideband_cb = |data: &[u8]| {
            self.perform_gradually(&mut sideband_check, || {
                progbar.set_message(String::from_utf8_lossy(data).trim());
            });
            true
        };

        let mut callbacks = RemoteCallbacks::new();
        callbacks.transfer_progress(progress_cb);
        callbacks.sideband_progress(sideband_cb);

        let mut options = FetchOptions::new();
        options.remote_callbacks(callbacks);

        let fragment = url.fragment().map(|v| v.to_owned());
        url.set_fragment(None);

        let download_path = pkg.download_dir(config).join(filename);
        if download_path.exists() {
            if !config.clobber {
                if let Ok(repo) = Repository::open(&download_path) {
                    let head = repo.head().ok();
                    if let Some(name) = head.as_ref().and_then(|head| {
                        if head.is_branch() {
                            head.name()
                        } else {
                            None
                        }
                    }) {
                        repo.find_remote("origin")
                            .and_then(|mut remote| remote.fetch(&[&name], Some(&mut options), None))
                            .map_err(|e| NetworkError::Git(pkg.name().to_string(), e))?;

                        return self.checkout_fragment(pkg, url, &repo, fragment);
                    }
                }
            }
            fs::remove_dir_all(&download_path)
                .map_err(|e| NetworkError::RemoveDir(path_to_string(&download_path), e))?;
        }

        let repo = RepoBuilder::new()
            .fetch_options(options)
            .clone(url.as_str(), &download_path)
            .map_err(|e| NetworkError::Git(pkg.name().to_string(), e))?;

        self.checkout_fragment(pkg, url, &repo, fragment)
    }

    fn checkout_fragment(
        &self,
        pkg: &BuildFile,
        url: &Url,
        repo: &Repository,
        fragment: Option<String>,
    ) -> Result<(), NetworkError> {
        if let Some(fragment) = fragment {
            let res = if fragment.starts_with("branch=") {
                repo.set_head(&format!(
                    "refs/remotes/origin/{}",
                    fragment.trim_left_matches("branch=")
                ))
            } else if fragment.starts_with("tag=") {
                repo.revparse_single(fragment.trim_left_matches("tag="))
                    .and_then(|reference| reference.peel_to_tag())
                    .and_then(|tag| tag.peel())
                    .and_then(|object| repo.set_head_detached(object.id()))
            } else if fragment.starts_with("commit=") {
                repo.revparse_single(fragment.trim_left_matches("commit="))
                    .and_then(|reference| reference.peel_to_commit())
                    .and_then(|commit| repo.set_head_detached(commit.id()))
            } else {
                Err(NetworkError::UnknownFragment(url.to_owned()))?
            };
            res.and_then(|_| repo.checkout_head(Some(&mut CheckoutBuilder::new().force())))
                .map_err(|e| NetworkError::Git(pkg.name().to_string(), e))?;
        }

        Ok(())
    }

    fn download_http(
        &self,
        progbar: &ProgressBar,
        pkg: &BuildFile,
        config: &Config,
        url: &Url,
        filename: &str,
    ) -> Result<(), NetworkError> {
        const BUF_SIZE: usize = 32 * 1024;

        let filepath = pkg.download_dir(config).join(filename);
        let mut open_opts = OpenOptions::new();
        let mut headers = Headers::new();

        if filepath.exists() && !config.clobber {
            if let Some(length) = self.supports_range(url) {
                // get metadata for file so we can 1. get the size of the file for Range and 2. see if
                // the server has a newer version of the file (which would mean we need to download
                // from scratch)
                let metadata = fs::metadata(&filepath)
                    .map_err(|e| NetworkError::Metadata(path_to_string(&filepath), e))?;

                let filelen = metadata.len();
                if length == filelen {
                    // we (most likely) have the correct file, so we are done
                    return Ok(());
                } else if filelen < length {
                    // TODO: handle error (basically if anything fails here we should just download from
                    //       scratch)
                    //let create_time = metadata.created().or_else(|_| metadata.modified()).unwrap();
                    // subtract 60 seconds to satisfy If-Range's date validator
                    //let range_date = create_time - Duration::from_secs(60 * 60 * 24);

                    // FIXME: not sure how to get If-Range to work correctly
                    //headers.set(LastModified(range_date.into()));
                    //headers.set(IfRange::Date(create_time.into()));
                    headers.set(Range::Bytes(vec![ByteRangeSpec::AllFrom(filelen)]));

                    open_opts.append(true);
                }
            }
        }

        // we don't set any headers unless Content-Range is supported, so this is fine
        if headers.len() == 0 {
            // either the file doesn't exist or ranges aren't supported by the server, so just
            // trash any file that already exists
            open_opts.create(true).truncate(true).write(true);
        }

        let mut resp = self.client
            .get(url.as_str())
            .headers(headers)
            .send()
            .and_then(|res| res.error_for_status())
            .map_err(|e| NetworkError::Reqwest(pkg.name().to_string(), e))?;

        // XXX: will range ever be None?
        if let Some(&ContentRange(ContentRangeSpec::Bytes {
            range: Some((from, to)),
            instance_length: _,
        })) = resp.headers().get::<ContentRange>()
        {
            progbar.set_style(self.bar_style());

            progbar.set_length(to);
            progbar.set_position(from);
        } else if let Some(&ContentLength(length)) = resp.headers().get::<ContentLength>() {
            progbar.set_style(self.bar_style());

            progbar.set_length(length);
        } else {
            progbar.set_style(self.spinner_style());
        }

        let file = open_opts
            .open(&filepath)
            .map_err(|e| NetworkError::TargetFile(path_to_string(&filepath), e))?;

        let mut writer = BufWriter::new(file);

        let mut last_check = Instant::now() - Duration::from_millis(Self::WAIT_TIME_MILLIS);
        let mut byte_count = 0;
        let mut buffer = [0; BUF_SIZE];
        loop {
            let n = resp.read(&mut buffer)
                .map_err(|e| NetworkError::Download(url.clone(), e))?;
            if n == 0 {
                break;
            }

            byte_count += n;

            self.perform_gradually(&mut last_check, || {
                progbar.inc(byte_count as u64);
                byte_count = 0;
            });

            writer
                .write_all(&buffer[..n])
                .map_err(|e| NetworkError::Write(path_to_string(&filepath), e))?;
        }

        progbar.inc(byte_count as u64);

        Ok(())
    }

    fn supports_range(&self, url: &Url) -> Option<u64> {
        self.client.head(url.as_str()).send().ok().and_then(|res| {
            res.headers()
                .get::<AcceptRanges>()
                .map(|h| h.contains(&RangeUnit::Bytes) as u64)
                .and(res.headers().get::<ContentLength>().map(|h| h.0))
        })
    }

    fn perform_gradually<F>(&self, last_check: &mut Instant, mut action: F)
    where
        F: FnMut(),
    {
        // only display progress every 250ms to avoid the slowdown caused by the progress bar's
        // internal state constantly locking and unlocking
        let duration = Instant::now().duration_since(*last_check);
        if duration.as_secs() > 0 || duration.subsec_nanos() >= Self::WAIT_TIME_NANOS {
            action();

            *last_check = Instant::now();
        }
    }

    fn bar_style(&self) -> ProgressStyle {
        ProgressStyle::default_bar()
            .template("{prefix:.bold.dim}: {wide_bar} {bytes}/{total_bytes} {percent}% {eta}")
    }

    fn spinner_style(&self) -> ProgressStyle {
        ProgressStyle::default_spinner().template("{prefix:.bold.dim}: {spinner} {bytes}/?")
    }

    fn git_object_style(&self) -> ProgressStyle {
        ProgressStyle::default_bar()
            .template("{prefix:.bold.dim}: {wide_bar} {pos}/{len} objects {percent}%")
    }

    fn git_delta_style(&self) -> ProgressStyle {
        ProgressStyle::default_bar()
            .template("{prefix:.bold.dim}: {wide_bar} {pos}/{len} deltas {percent}%")
    }

    fn git_counting_style(&self) -> ProgressStyle {
        ProgressStyle::default_bar().template("{prefix:.bold.dim}: {msg}")
    }
}
