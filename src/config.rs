use clap::OsValues;
use failure::Error;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Write};
use std::path::Path;

use archive::Archiver;
use builder::Builder;
use network::Downloader;
use package::BuildFile;
use progress::Progress;
use util;

pub enum Action<'a> {
    // attempt to download a given package
    Download {
        pkgs: OsValues<'a>,
    },

    // try to install the package, first building it if it's not already built
    Install {
        force: bool,
        pkgs: OsValues<'a>,
    },

    // build the package, downloading the source code first if need be
    Build {
        pkgs: OsValues<'a>,
    },

    // print a short description of a given package
    Describe {
        pkgs: OsValues<'a>,
    },
}

impl<'a> Action<'a> {
    pub fn execute(&self, config: &Config) -> Result<(), Error> {
        use Action::*;

        match self {
            Download { pkgs } => {
                let buildfiles = self.gather_buildfiles(config, pkgs)?;
                self.download(config, &buildfiles)?;
            }
            Install { force, pkgs } => {
                for pkg in pkgs.clone().into_iter() {
                    let _buildfile = BuildFile::open(config.pkgdir, pkg);


                }
            }
            Build { pkgs } => {
                let buildfiles = self.gather_buildfiles(config, pkgs)?;

                let bar_count = buildfiles.len() + 1;

                let downloader = Downloader::new();
                let builder = Builder::new();

                let (download_init, download_iter) = downloader.download_setup(config, &buildfiles);
                let (build_init, build_iter) = builder.build_setup(config, &buildfiles);

                {
                    let mut progress = Progress::new(bar_count);

                    progress.add_step(&*download_init, &*download_iter);
                    progress.add_step(&*build_init, &*build_iter);

                    progress.run(config, buildfiles.iter())?;
                }
            }
            Describe { pkgs } => {
                let buildfiles = self.gather_buildfiles(config, pkgs)?;

                println!("displaying information about {} package(s)", buildfiles.len());

                for buildfile in buildfiles {
                    println!("\n{}", buildfile.info());
                }
            }
        }

        Ok(())
    }

    fn download(&self, config: &Config, pkgs: &[BuildFile]) -> Result<(), Error> {
        Ok(Downloader::new().download_pkgs(config, pkgs)?)
    }

    fn gather_buildfiles(&self, config: &Config, pkgs: &OsValues) -> Result<Vec<BuildFile>, Error> {
        let mut packages: Vec<&OsStr> = pkgs.clone().collect();
        packages.sort();
        packages.dedup();
        packages.into_iter().map(|pkg| BuildFile::open(config.pkgdir, pkg)).collect()
    }

    fn verify_action(act_name: &str, pkgs: &[BuildFile]) -> Result<bool, Error> {
        let stdout_raw = io::stdout();
        let mut stdout = stdout_raw.lock();

        writeln!(&mut stdout, "Planning to {} the following {} packages", act_name, pkgs.len())?;
        write!(&mut stdout, "Continue? (y/n) ")?;

        let mut line = String::new();
        io::stdin().read_line(&mut line)?;

        Ok(if line.starts_with("y") || line.starts_with("Y") {
            true
        } else {
            false
        })
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