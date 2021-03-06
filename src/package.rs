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

    #[fail(display = "the check step is required unless skip_check is true")]
    NeedsCheck,
}

#[derive(Debug, Default)]
pub struct BuildFile {
    path: PathBuf,

    env: HashMap<String, String>,
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
    skip_check: Option<bool>,

    prepare: Option<Vec<String>>,
    build: Option<Vec<String>>,
    check: Option<Vec<String>>,
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
    skip_check: Option<bool>,

    prepare: Option<Vec<String>>,
    build: Option<Vec<String>>,
    check: Option<Vec<String>>,
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

        let (mut env, mut package) = (buildfile.env, buildfile.package);

        // if check hasn't been given and skip_extract is not present, error out
        if !package.skip_check.unwrap_or(false) && package.check.is_none() {
            Err(PackageError::NeedsCheck)?;
        }

        // FIXME: rewrite so that all the variables are substituted at once rather than one at a
        //        time the current way means that if a variable $var=$hi is substituted first, then
        //        if there is another variable $hi, it will get substituted as well (variables
        //        should not depend on each other in the env array to avoid circular references).
        //        of course, if the user ends up triggering this somehow they pretty much asked for
        //        it to happen, but the less footguns the better
        if let Some(ref mut env) = env {
            for (key, val) in env.iter() {
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

        let mut env = env.unwrap_or_default();

        env.insert("name".into(), package.name.clone());
        env.insert("version".into(), package.version.clone());
        // TODO: support ${var} too
        package.description = subst_vars(&package.description, "$name", &package.name);
        package.description = subst_vars(&package.description, "$version", &package.version);
        for src in &mut package.source {
            *src = subst_vars(src, "$name", &package.name);
            *src = subst_vars(src, "$version", &package.version);
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
                skip_check: package.skip_check,

                prepare: package.prepare,
                build: package.build,
                check: package.check,
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

    pub fn env(&self) -> &HashMap<String, String> {
        &self.env
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

    pub fn skip_check(&self) -> bool {
        self.package.skip_check.unwrap_or(false)
    }

    pub fn prepare(&self) -> Option<&Vec<String>> {
        self.package.prepare.as_ref()
    }

    pub fn build(&self) -> Option<&Vec<String>> {
        self.package.build.as_ref()
    }

    pub fn check(&self) -> Option<&Vec<String>> {
        self.package.check.as_ref()
    }

    pub fn install(&self) -> Option<&Vec<String>> {
        self.package.install.as_ref()
    }

    pub fn base_dir(&self, config: &Config) -> PathBuf {
        self.package.base_dir(config)
    }

    pub fn build_dir(&self, config: &Config) -> PathBuf {
        self.package.build_dir(config)
    }

    pub fn pkg_dir(&self, config: &Config) -> PathBuf {
        self.package.pkg_dir(config)
    }

    pub fn pkgbuild_dir<'a: 'b, 'b>(&self, config: &'a Config) -> &'b Path {
        self.package.pkgbuild_dir(config)
    }

    pub fn log_dir(&self, config: &Config) -> PathBuf {
        self.package.log_dir(config)
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
        self.log_dir(config).join("stdout.log")
    }

    pub fn stderr_log(&self, config: &Config) -> PathBuf {
        self.log_dir(config).join("stderr.log")
    }

    pub fn base_dir(&self, config: &Config) -> PathBuf {
        config
            .build_dir
            .join(format!("{}-{}", self.name, self.version))
    }

    pub fn log_dir(&self, config: &Config) -> PathBuf {
        self.base_dir(config).join("log")
    }

    pub fn build_dir(&self, config: &Config) -> PathBuf {
        self.base_dir(config).join("build")
    }

    pub fn pkg_dir(&self, config: &Config) -> PathBuf {
        self.base_dir(config).join("pkg")
    }

    pub fn pkgbuild_dir<'a: 'b, 'b>(&self, config: &'a Config) -> &'b Path {
        config.pkgbuild_dir
    }

    pub fn download_dir(&self, config: &Config) -> PathBuf {
        self.base_dir(config).join("src")
    }

    pub fn archive_out_dir(&self, config: &Config) -> PathBuf {
        self.build_dir(config)
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
            skip_check: None,

            prepare: None,
            build: None,
            check: None,
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
