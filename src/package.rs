use failure::{Error, ResultExt};
use serde::{Deserialize, Deserializer};
use serde_yaml;
use semver::Version;
use url::Url;
use url_serde;
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

#[derive(Debug, Deserialize)]
pub struct BuildFile {
    package: Package,
}

#[derive(Debug, Deserialize)]
struct Package {
    name: String,
    version: Version,
    description: String,
    license: Vec<String>,
    
    // files to download
    #[serde(deserialize_with = "vec_urls")]
    source: Vec<Url>,

    prepare: Option<Vec<String>>,
    build: Option<Vec<String>>,
    install: Option<Vec<String>>,
}

impl BuildFile {
    pub fn open<P: AsRef<Path> + ?Sized, S: AsRef<OsStr> + ?Sized>(pkgdir: &P, pkgname: &S) -> Result<Self, Error> {
        let (pkgdir, pkgname) = (pkgdir.as_ref(), pkgname.as_ref());

        let build_path = pkgdir.join(pkgname).join("BUILD");

        let file = File::open(&build_path).with_context(|err| {
            format!("could not read build file at '{}': {}", build_path.display(), err)
        })?;

        let reader = BufReader::new(file);
        Ok(serde_yaml::from_reader(reader)?)
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

    pub fn source(&self) -> &[Url] {
        &self.package.source
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

    pub fn info(&self) -> String {
        self.package.info()
    }

    pub fn file_path(url: &Url) -> Result<&str, PackageError> {
        Package::file_path(url)
    }

    pub fn file_build_path(&self, config: &Config, url: &Url) -> Result<PathBuf, PackageError> {
        self.package.file_build_path(config, url)
    }
}

impl Package {
    pub fn builddir(&self, config: &Config) -> PathBuf {
        config.builddir.join(format!("{}-{}", self.name, self.version))
    }

    // TODO: colors and which section the package is in (e.g. core or testing)
    pub fn info(&self) -> String {
        format!("{} {} {:?}\n{}", self.name, self.version, &self.license[..], self.description)
    }

    pub fn file_path(url: &Url) -> Result<&str, PackageError> {
        let url_err = || {
            PackageError::UnknownFilePath(url.clone())
        };

        let filename = url.path_segments().ok_or_else(url_err)?.last().unwrap();
        if filename.len() == 0 {
            Err(url_err())?;
        }

        Ok(filename)
    }

    pub fn file_build_path(&self, config: &Config, url: &Url) -> Result<PathBuf, PackageError> {
        Ok(self.builddir(config).join(Package::file_path(url)?))
    }
}

fn vec_urls<'de, D>(deserializer: D) -> Result<Vec<Url>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Wrapper(#[serde(with = "url_serde")] Url);

    let vec = Vec::deserialize(deserializer)?;
    Ok(vec.into_iter().map(|Wrapper(url)| url).collect())
}
