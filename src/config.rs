use clap::OsValues;
use failure::Error;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::Path;

use builder::Builder;
use network::Downloader;
use package::BuildFile;
use progress::Progress;

pub enum Action<'a> {
    // attempt to download a given package
    Download { pkgs: OsValues<'a> },

    // build the package, downloading the source code first if need be
    Build { pkgs: OsValues<'a> },

    // print a short description of a given package
    Describe { pkgs: OsValues<'a> },
}

impl<'a> Action<'a> {
    pub fn execute(&self, config: &Config) -> Result<(), Error> {
        use Action::*;

        match self {
            Download { pkgs } => {
                let buildfiles = self.gather_buildfiles(config, pkgs)?;

                let downloader = Downloader::new();
                let (init, iter) = downloader.download_setup(config, &buildfiles);

                Progress::new(&buildfiles)
                    .add_step(&*init, &*iter)
                    .run(config, buildfiles.iter())?;
            }
            Build { pkgs } => {
                let buildfiles = self.gather_buildfiles(config, pkgs)?;

                let downloader = Downloader::new();
                let builder = Builder::new();

                let (download_init, download_iter) = downloader.download_setup(config, &buildfiles);
                let (build_init, build_iter) = builder.build_setup(config, &buildfiles);

                Progress::new(&buildfiles)
                    .add_step(&*download_init, &*download_iter)
                    .add_step(&*build_init, &*build_iter)
                    .run(config, buildfiles.iter())?;
            }
            Describe { pkgs } => {
                let buildfiles = self.gather_buildfiles(config, pkgs)?;

                println!(
                    "displaying information about {} package(s)",
                    buildfiles.len()
                );

                for buildfile in buildfiles {
                    println!("\n{}", buildfile.info());
                }
            }
        }

        Ok(())
    }

    fn gather_buildfiles(&self, config: &Config, pkgs: &OsValues) -> Result<Vec<BuildFile>, Error> {
        let mut packages: Vec<&OsStr> = pkgs.clone().collect();
        packages.sort();
        packages.dedup();
        packages
            .into_iter()
            .map(|pkg| BuildFile::open(config.pkgdir, pkg))
            .collect()
    }
}

pub struct Config<'a> {
    pub pkgdir: &'a Path,
    pub builddir: &'a Path,
    pub logdir: &'a Path,
    // FIXME: this should only accept utf-8
    pub licenses: Vec<OsString>,
    pub verbose: bool,
    pub clobber: bool,
    pub fail_fast: bool,
    pub action: Action<'a>,
}

impl<'a> fmt::Display for Config<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "--- begin mkpkg config ---\npkgdir: {}\naccepted licenses: {:?}\nverbose: {}\n--- end mkpkg config ---",
                    self.pkgdir.display(),
                    self.licenses.iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>(),
                    self.verbose)
    }
}
