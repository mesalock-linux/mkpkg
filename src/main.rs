extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_yaml;
#[macro_use]
extern crate clap;
extern crate ansi_term;
extern crate crossbeam;
extern crate failure;
extern crate num_cpus;
extern crate semver;
extern crate url;
#[macro_use]
extern crate failure_derive;
extern crate indicatif;
extern crate tempfile;
extern crate term_size;
extern crate unicode_xid;
extern crate walkdir;

// downloading source code/patches
extern crate git2;
extern crate reqwest;

// compression of downloaded files
extern crate bzip2;
extern crate flate2;
extern crate tar;
extern crate xz2;

use clap::{Arg, ArgMatches, SubCommand};
use std::ffi::OsStr;
use std::path::Path;
use std::process;

use config::{Action, Config};

mod archive;
mod builder;
mod config;
mod network;
mod package;
mod progress;
#[allow(dead_code)]
mod util;

fn main() {
    let matches = app_from_crate!()
                    .arg(Arg::with_name("pkgdir")
                            .long("pkgdir")
                            .takes_value(true)
                            .default_value_os(OsStr::new("."))
                            .help("Set the directory in which to search for packages"))
                    .arg(Arg::with_name("builddir")
                            .long("builddir")
                            .takes_value(true)
                            .default_value_os(OsStr::new("build"))
                            .help("Set the directory in which to download and build packages"))
                    .arg(Arg::with_name("logdir")
                            .long("logdir")
                            .takes_value(true)
                            .default_value_os(OsStr::new("logs"))
                            .help("Set the directory in which build logs will be stored"))
                    .arg(Arg::with_name("accept")
                            .long("accept")
                            .takes_value(true)
                            .default_value("all")
                            .help("Sets which licenses should automatically be accepted"))
                    .arg(Arg::with_name("verbose")
                            .long("verbose")
                            .help("Print out as much information as possible"))
                    .arg(Arg::with_name("clobber")
                            .long("clobber")
                            .help("Clobber any existing output from previous build attempts"))
                    .arg(Arg::with_name("fail-fast")
                            .long("fail-fast")
                            .help("Stop as soon as an error occurs"))
                    .subcommand(SubCommand::with_name("download")
                            .arg(Arg::with_name("PKG")
                                    .index(1)
                                    .required(true)
                                    .multiple(true)))
                    .subcommand(SubCommand::with_name("describe")
                            .arg(Arg::with_name("PKG")
                                    .index(1)
                                    .required(true)
                                    .multiple(true)))
                    .subcommand(SubCommand::with_name("build")
                            .arg(Arg::with_name("PKG")
                                    .index(1)
                                    .required(true)
                                    .multiple(true)))
                    .subcommand(SubCommand::with_name("install")
                            .arg(Arg::with_name("force")
                                    .long("force")
                                    .help("Force installation to continue even if doing so would \
                                           overwrite existing files"))
                            .arg(Arg::with_name("PKG")
                                    .index(1)
                                    .required(true)
                                    .multiple(true)))
                    // important to note that we require a package argument (unlike the shell
                    // version which just installed all packages), so we need some sort of shell
                    // script to just call mkpkg with all the packages as arguments to build
                    // everything
                    .get_matches();

    let pkgdir = Path::new(matches.value_of_os("pkgdir").unwrap());
    let builddir = Path::new(matches.value_of_os("builddir").unwrap());
    let logdir = Path::new(matches.value_of_os("logdir").unwrap());

    let licenses = matches
        .values_of_os("accept")
        .map(|it| it.map(|v| v.into()).collect())
        .unwrap_or_else(|| vec![]);

    let config = Config {
        pkgdir: &pkgdir,
        builddir: &builddir,
        logdir: &logdir,
        licenses: licenses,
        verbose: matches.is_present("verbose"),
        clobber: matches.is_present("clobber"),
        fail_fast: matches.is_present("fail-fast"),
        action: determine_action(&matches),
    };

    if config.verbose {
        println!("{}", config);
    }

    if let Err(f) = config.action.execute(&config) {
        let _ = util::display_err(format_args!("{}", f));
        process::exit(1);
    }
}

fn determine_action<'a>(matches: &'a ArgMatches<'a>) -> Action<'a> {
    match matches.subcommand() {
        ("install", Some(matches)) => Action::Install {
            force: matches.is_present("force"),
            pkgs: matches.values_of_os("PKG").unwrap(),
        },
        ("build", Some(matches)) => Action::Build {
            pkgs: matches.values_of_os("PKG").unwrap(),
        },
        ("download", Some(matches)) => Action::Download {
            pkgs: matches.values_of_os("PKG").unwrap(),
        },
        ("describe", Some(matches)) => Action::Describe {
            pkgs: matches.values_of_os("PKG").unwrap(),
        },
        _ => unreachable!(),
    }
}
