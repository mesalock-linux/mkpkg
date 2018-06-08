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

use clap::{AppSettings, Arg, ArgMatches, SubCommand};
use std::ffi::OsStr;
use std::path::Path;
use std::process;
use std::u32;

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
                    .setting(AppSettings::SubcommandRequired)
                    .arg(Arg::with_name("pkgbuild-dir")
                            .long("pkgbuild-dir")
                            .takes_value(true)
                            .default_value_os(OsStr::new("."))
                            .help("Set the directory in which to search for package build files"))
                    .arg(Arg::with_name("build-dir")
                            .long("build-dir")
                            .takes_value(true)
                            .default_value_os(OsStr::new("build"))
                            .help("Set the directory in which to download and build packages"))
                    .arg(Arg::with_name("log-dir")
                            .long("log-dir")
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
                    .arg(Arg::with_name("parallel-download")
                            .long("parallel-download")
                            .short("d")
                            .takes_value(true)
                            .validator(is_u32)
                            .help("Set the number of downloads to occur in parallel"))
                    .arg(Arg::with_name("parallel-build")
                            .long("parallel-build")
                            .short("b")
                            .takes_value(true)
                            .validator(is_u32)
                            .help("Set the number of builds to occur in parallel"))
                    .subcommand(SubCommand::with_name("download")
                            .arg(Arg::with_name("PKGBUILD")
                                    .index(1)
                                    .required(true)
                                    .multiple(true)))
                    .subcommand(SubCommand::with_name("describe")
                            .arg(Arg::with_name("PKGBUILD")
                                    .index(1)
                                    .required(true)
                                    .multiple(true)))
                    .subcommand(SubCommand::with_name("build")
                            .arg(Arg::with_name("PKGBUILD")
                                    .index(1)
                                    .required(true)
                                    .multiple(true)))
                    // important to note that we require a package argument (unlike the shell
                    // version which just installed all packages), so we need some sort of shell
                    // script to just call mkpkg with all the packages as arguments to build
                    // everything
                    .get_matches();

    let pkgdir = Path::new(matches.value_of_os("pkgbuild-dir").unwrap());
    let builddir = Path::new(matches.value_of_os("build-dir").unwrap());
    let logdir = Path::new(matches.value_of_os("log-dir").unwrap());

    let licenses = matches
        .values_of_os("accept")
        .map(|it| it.map(|v| v.into()).collect())
        .unwrap_or_else(|| vec![]);

    let config = Config {
        pkgbuild_dir: &pkgdir,
        build_dir: &builddir,
        log_dir: &logdir,
        licenses: licenses,
        verbose: matches.is_present("verbose"),
        clobber: matches.is_present("clobber"),
        fail_fast: matches.is_present("fail-fast"),
        parallel_download: convert_u32(matches.value_of("parallel-download")),
        parallel_build: convert_u32(matches.value_of("parallel-build")),
        action: determine_action(&matches),
    };

    if let Err(f) = config.action.execute(&config) {
        let _ = util::display_err(format_args!("{}", f));
        process::exit(1);
    }
}

fn determine_action<'a>(matches: &'a ArgMatches<'a>) -> Action<'a> {
    match matches.subcommand() {
        ("build", Some(matches)) => Action::Build {
            pkgs: matches.values_of_os("PKGBUILD").unwrap(),
        },
        ("download", Some(matches)) => Action::Download {
            pkgs: matches.values_of_os("PKGBUILD").unwrap(),
        },
        ("describe", Some(matches)) => Action::Describe {
            pkgs: matches.values_of_os("PKGBUILD").unwrap(),
        },
        _ => unreachable!(),
    }
}

fn is_u32(val: String) -> Result<(), String> {
    u32::from_str_radix(&val, 10)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn convert_u32(val: Option<&str>) -> Option<u32> {
    val.map(|s| u32::from_str_radix(s, 10).unwrap())
}
