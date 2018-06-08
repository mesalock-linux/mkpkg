use failure::{Error, ResultExt};
use semver::Version;
use serde_yaml;
use unicode_xid::UnicodeXID;
use url::Url;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use super::Config;

#[derive(Debug, Fail)]
pub enum PackageError {
    #[fail(display = "could not determine file path from the URL: {}", _0)]
    UnknownFilePath(Url),
}

#[derive(Debug, Default)]
pub struct BuildFile {
    path: PathBuf,

    env: Option<HashMap<String, String>>,
    package: Package,
}

#[derive(Debug)]
struct Package {
    name: String,
    version: Version,
    description: String,
    license: Vec<String>,

    // files to download
    source: Vec<String>,
    skip_extract: Option<bool>,

    prepare: Option<Vec<String>>,
    build: Option<Vec<String>>,
    install: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct BuildFileRaw {
    env: Option<HashMap<String, String>>,
    package: PackageRaw,
}

#[derive(Debug, Deserialize)]
struct PackageRaw {
    name: String,
    version: String,
    description: String,
    license: Vec<String>,

    source: Vec<String>,
    skip_extract: Option<bool>,

    prepare: Option<Vec<String>>,
    build: Option<Vec<String>>,
    install: Option<Vec<String>>,
}

impl BuildFile {
    pub fn open<P: AsRef<Path> + ?Sized, S: AsRef<OsStr> + ?Sized>(
        pkgdir: &P,
        pkgname: &S,
    ) -> Result<Self, Error> {
        let (pkgdir, pkgname) = (pkgdir.as_ref(), pkgname.as_ref());

        let build_path = pkgdir.join(pkgname);

        let file = File::open(&build_path).with_context(|err| {
            format!(
                "could not read build file at '{}': {}",
                build_path.display(),
                err
            )
        })?;

        let reader = BufReader::new(file);
        let buildfile: BuildFileRaw = serde_yaml::from_reader(reader)?;

        let (env, mut package) = (buildfile.env, buildfile.package);

        // FIXME: rewrite so that all the variables are substituted at once rather than one at a
        //        time the current way means that if a variable $var=$hi is substituted first, then
        //        if there is another variable $hi, it will get substituted as well (variables
        //        should not depend on each other in the env array to avoid circular references).
        //        of course, if the user ends up triggering this somehow they pretty much asked for
        //        it to happen, but the less footguns the better
        if let Some(ref env) = env {
            for (key, val) in env {
                let key = format!("${}", key);
                package.name = subst_vars(&package.name, &key, val);
                package.version = subst_vars(&package.version, &key, val);
                package.description = subst_vars(&package.description, &key, val);
                for license in &mut package.license {
                    *license = subst_vars(license, &key, val);
                }
                for src in &mut package.source {
                    *src = subst_vars(src, &key, val);
                }
                // don't need to do anything with prepare/build/install as we just attach the env
                // vars as environment variables to `sh`
            }
        }

        Ok(BuildFile {
            path: PathBuf::from(pkgname),

            env: env,
            package: Package {
                name: package.name,
                version: Version::parse(&package.version)?,
                description: package.description,
                license: package.license,

                source: package.source,
                skip_extract: package.skip_extract,

                prepare: package.prepare,
                build: package.build,
                install: package.install,
            },
        })
    }

    // this is for testing the network code
    pub(crate) fn with_urls(urls: Vec<String>) -> Self {
        let mut buildfile = BuildFile::default();
        buildfile.package.source = urls;
        buildfile
    }

    // returns path to build file within pkgbuild_dir
    pub fn path(&self) -> &Path {
        &self.path
    }

    // returns path to the parent directory of this build file
    pub fn parent_dir(&self) -> &Path {
        // should always have a parent because there will always be at least /
        self.path().parent().unwrap()
    }

    pub fn env(&self) -> Option<&HashMap<String, String>> {
        self.env.as_ref()
    }

    pub fn name(&self) -> &str {
        &self.package.name
    }

    pub fn version(&self) -> &Version {
        &self.package.version
    }

    pub fn description(&self) -> &str {
        &self.package.description
    }

    pub fn license(&self) -> &[String] {
        &self.package.license
    }

    pub fn source(&self) -> &[String] {
        &self.package.source
    }

    pub fn skip_extract(&self) -> bool {
        self.package.skip_extract.unwrap_or(false)
    }

    pub fn prepare(&self) -> Option<&Vec<String>> {
        self.package.prepare.as_ref()
    }

    pub fn build(&self) -> Option<&Vec<String>> {
        self.package.build.as_ref()
    }

    pub fn install(&self) -> Option<&Vec<String>> {
        self.package.install.as_ref()
    }

    pub fn builddir(&self, config: &Config) -> PathBuf {
        self.package.builddir(config)
    }

    pub fn pkgbuild_dir<'a: 'b, 'b>(&self, config: &'a Config) -> &'b Path {
        self.package.pkgbuild_dir(config)
    }

    pub fn logdir(&self, config: &Config) -> PathBuf {
        self.package.logdir(config)
    }

    pub fn download_dir(&self, config: &Config) -> PathBuf {
        self.package.download_dir(config)
    }

    pub fn archive_out_dir(&self, config: &Config) -> PathBuf {
        self.package.archive_out_dir(config)
    }

    pub fn stdout_log(&self, config: &Config) -> PathBuf {
        self.package.stdout_log(config)
    }

    pub fn stderr_log(&self, config: &Config) -> PathBuf {
        self.package.stderr_log(config)
    }

    pub fn info(&self) -> String {
        self.package.info()
    }

    pub fn file_path(src: &str) -> Result<String, PackageError> {
        Package::file_path(src)
    }

    pub fn file_download_path(&self, config: &Config, src: &str) -> Result<PathBuf, PackageError> {
        self.package.file_download_path(config, src)
    }
}

impl Package {
    pub fn stdout_log(&self, config: &Config) -> PathBuf {
        self.logdir(config).join("stdout.log")
    }

    pub fn stderr_log(&self, config: &Config) -> PathBuf {
        self.logdir(config).join("stderr.log")
    }

    pub fn logdir(&self, config: &Config) -> PathBuf {
        config
            .log_dir
            .join(format!("{}-{}", self.name, self.version))
    }

    pub fn builddir(&self, config: &Config) -> PathBuf {
        config
            .build_dir
            .join(format!("{}-{}", self.name, self.version))
    }

    pub fn pkgbuild_dir<'a: 'b, 'b>(&self, config: &'a Config) -> &'b Path {
        config.pkgbuild_dir
    }

    pub fn download_dir(&self, config: &Config) -> PathBuf {
        self.builddir(config).join("download")
    }

    pub fn archive_out_dir(&self, config: &Config) -> PathBuf {
        self.builddir(config).join("out")
    }

    // TODO: colors and which section the package is in (e.g. core or testing)
    pub fn info(&self) -> String {
        format!(
            "{} {} {:?}\n{}",
            self.name,
            self.version,
            &self.license[..],
            self.description
        )
    }

    // TODO: probably change sources to use an Either type (so we don't need to parse every time
    //       this is called)
    pub fn file_path(src: &str) -> Result<String, PackageError> {
        if let Ok(url) = Url::parse(src) {
            let url_err = || PackageError::UnknownFilePath(url.clone());

            let filename = url.path_segments().ok_or_else(url_err)?.last().unwrap();
            if filename.len() == 0 {
                Err(url_err())?;
            }

            Ok(filename.to_string())
        } else {
            Ok(src.to_string())
        }
    }

    pub fn file_download_path(&self, config: &Config, src: &str) -> Result<PathBuf, PackageError> {
        Ok(self.download_dir(config).join(Package::file_path(src)?))
    }
}

impl Default for Package {
    fn default() -> Self {
        Self {
            name: String::default(),
            version: Version::new(0, 0, 0),
            description: String::default(),
            license: vec![],

            source: vec![],
            skip_extract: None,

            prepare: None,
            build: None,
            install: None,
        }
    }
}

fn subst_vars(input: &str, key: &str, value: &str) -> String {
    let mut split = input.split(key);
    let result = split.next().unwrap().to_string();
    split.fold(result, |mut acc, val| {
        acc.push_str(match val.chars().next() {
            Some(ch) if UnicodeXID::is_xid_continue(ch) => key,
            _ => value,
        });
        acc.push_str(val);
        acc
    })
}
