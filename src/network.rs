use git2::build::RepoBuilder;
use git2::{self, FetchOptions, RemoteCallbacks};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use reqwest::{self, Client};
use reqwest::header::ContentLength;
use url::Url;

use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Read, Write};

use package::{BuildFile, PackageError};
use progress::{AggregateError, ProgressError, Progress};
use util::path_to_string;

use super::Config;

#[derive(Debug, Fail)]
pub enum NetworkError {
    #[fail(display = "could not remove directory '{}': {}", _0, _1)]
    RemoveDir(String, #[cause] io::Error),

    #[fail(display = "could not create file '{}': {}", _0, _1)]
    TargetFile(String, #[cause] io::Error),

    #[fail(display = "failed to download data from '{}': {}", _0, _1)]
    Download(Url, #[cause] io::Error),

    #[fail(display = "failed to write to '{}': {}", _0, _1)]
    Write(String, #[cause] io::Error),

    #[fail(display = "{}", _0)]
    Package(#[cause] PackageError),

    #[fail(display = "invalid scheme for the URL '{}'", _0)]
    UnknownScheme(Url),

    #[fail(display = "failed to download '{}': {}", _0, _1)]
    Git(String, #[cause] git2::Error),

    #[fail(display = "failed to download '{}': {}", _0, _1)]
    Reqwest(String, #[cause] reqwest::Error),

    #[fail(display = "{}", _0)]
    Progress(#[cause] ProgressError),
}

impl From<ProgressError> for NetworkError {
    fn from(err: ProgressError) -> Self {
        NetworkError::Progress(err)
    }
}

pub(crate) struct Downloader {
    client: Client,
}

impl Downloader {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    pub fn download_pkgs(&self, config: &Config, pkgs: &[BuildFile]) -> Result<(), AggregateError<NetworkError>> {
        let bar_count = pkgs.len() + 1;
        let mut progress = Progress::new(bar_count);

        progress.on_init(|total_bar, bars| {
            for bar in bars.lock().unwrap().iter() {
                bar.set_style(self.bar_style());
            }

            total_bar.set_length(pkgs.len() as u64);
            total_bar.tick();
        });

        progress.on_iter(|pkg: &BuildFile, progbar, total_bar, errors| {
            for (i, url) in pkg.source().iter().enumerate() {
                progbar.set_prefix(&format!("{}/{}", pkg.name(), i + 1));
                progbar.set_position(0);
                
                if let Err(f) = self.download(&progbar, pkg, config, url) {
                    let mut errors = errors.lock().unwrap();
                    errors.push(f);
                    total_bar.set_message(&errors.len().to_string());
                    total_bar.tick();
                }
            }
        });

        progress.run(config, pkgs.par_iter())
    }

    // XXX: design should maybe check if the user specified git/https/http like git: url
    //      if so, just try that, otherwise try to figure it out like below
    fn download(&self, progbar: &ProgressBar, pkg: &BuildFile, config: &Config, url: &Url) -> Result<(), NetworkError> {
        let filename = BuildFile::file_path(url).map_err(|e| NetworkError::Package(e))?;

        match url.scheme() {
            "http" | "https" => {
                // as we require git URLs to be prefixed with "git+", this should be fine 
                self.download_http(progbar, pkg, config, url, filename)
            }
            "git+http" | "git+https" | "git" | "git+ssh" => {
                // can only be git (if it's a valid source URL)
                let orig = url;
                let mut url = url.clone();
                if url.scheme() != "git" {
                    let real_scheme = &orig.scheme()[4..];
                    url.set_scheme(real_scheme).map_err(|_| NetworkError::UnknownScheme(url.clone()))?;
                }
                self.download_git(progbar, pkg, config, &url, filename)
            }
            _ => Err(NetworkError::UnknownScheme(url.clone())),
        }
    }

    fn download_git(&self, progbar: &ProgressBar, pkg: &BuildFile, config: &Config, url: &Url, filename: &str) -> Result<(), NetworkError> {
        progbar.set_style(self.git_style());
        progbar.tick();
        let progress_cb = |progress: git2::Progress| {
            // XXX: it may be faster to just check every half a second or so (to avoid the constant
            //      locking and unlocking)
            progbar.set_length(progress.total_objects() as u64);
            progbar.set_position(progress.received_objects() as u64);
            true
        };

        let mut callbacks = RemoteCallbacks::new();
        callbacks.transfer_progress(progress_cb);

        let mut options = FetchOptions::new();
        options.remote_callbacks(callbacks);

        let download_path = pkg.builddir(config).join(filename);
        if config.clobber && download_path.exists() {
            fs::remove_dir_all(&download_path)
                .map_err(|e| NetworkError::RemoveDir(path_to_string(&download_path), e))?;
        }

        RepoBuilder::new()
            .fetch_options(options)
            .clone(url.as_str(), &download_path)
            .map_err(|e| NetworkError::Git(pkg.name().to_string(), e))?;

        Ok(())
    }

    fn download_http(&self, progbar: &ProgressBar, pkg: &BuildFile, config: &Config, url: &Url, filename: &str) -> Result<(), NetworkError> {
        const BUF_SIZE: usize = 128 * 1024;
        
        let mut resp = self.client
            .get(url.as_str())
            .send()
            .and_then(|res| res.error_for_status())
            .map_err(|e| NetworkError::Reqwest(pkg.name().to_string(), e))?;
        if let Some(&ContentLength(length)) = resp.headers().get::<ContentLength>() {
            progbar.set_style(self.bar_style());
            progbar.set_length(length);
        } else {
            progbar.set_style(self.spinner_style());
        }

        let filepath = pkg.builddir(config).join(filename);
        let mut open_opts = OpenOptions::new();
        let file = if config.clobber {
            open_opts.create(true).truncate(true)
        } else {
            open_opts.create_new(true)
        }.write(true).open(&filepath).map_err(|e| NetworkError::TargetFile(path_to_string(&filepath), e))?;

        let mut writer = BufWriter::new(file);

        let mut buffer = [0; BUF_SIZE];
        loop {
            let n = resp.read(&mut buffer).map_err(|e| NetworkError::Download(url.clone(), e))?;
            if n == 0 {
                break;
            }
            progbar.inc(n as u64);
            progbar.tick();

            writer.write_all(&buffer[..n]).map_err(|e| NetworkError::Write(path_to_string(&filepath), e))?;
        }

        Ok(())
    }

    fn bar_style(&self) -> ProgressStyle {
        ProgressStyle::default_bar().template("{prefix:.bold.dim}: {wide_bar} {bytes}/{total_bytes} {percent}% {eta}")
    }

    fn spinner_style(&self) -> ProgressStyle {
        ProgressStyle::default_spinner().template("{prefix:.bold.dim}: {spinner} {bytes}/?")
    }

    fn git_style(&self) -> ProgressStyle {
        ProgressStyle::default_bar().template("{prefix:.bold.dim}: {wide_bar} {pos}/{len} objects {percent}%")
    }
}
