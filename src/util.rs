use ansi_term::Color::{Green, Red, Yellow};
use num_cpus;
use walkdir::Error as WalkError;
use walkdir::WalkDir;

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, StripPrefixError};

#[derive(Debug, Fail)]
pub enum UtilError {
    #[fail(display = "could not create directory '{}': {}", _0, _1)]
    CreateDir(String, #[cause] io::Error),

    #[fail(display = "could not copy file from '{}' to '{}': {}", _0, _1, _2)]
    Copy(String, String, #[cause] io::Error),

    #[fail(display = "found invalid directory entry: {}", _0)]
    DirEntry(#[cause] WalkError),

    #[fail(display = "found invalid path '{}': {}", _0, _1)]
    PathPrefix(String, #[cause] StripPrefixError),
}

pub fn display_msg<W: Write>(stream: &mut W, args: fmt::Arguments) -> io::Result<()> {
    writeln!(stream, "{}: {}", crate_name!(), args)
}

pub fn display_err(args: fmt::Arguments) -> io::Result<()> {
    display_msg(
        &mut io::stderr(),
        format_args!("{}: {}", Red.bold().paint("error"), args),
    )
}

pub fn display_warn(args: fmt::Arguments) -> io::Result<()> {
    display_msg(
        &mut io::stderr(),
        format_args!("{}: {}", Yellow.bold().paint("warning"), args),
    )
}

pub fn display_success(args: fmt::Arguments) -> io::Result<()> {
    display_msg(
        &mut io::stdout(),
        format_args!("{} {}", Green.bold().paint("[+]"), args),
    )
}

pub fn display_failure(args: fmt::Arguments) -> io::Result<()> {
    display_msg(
        &mut io::stdout(),
        format_args!("{} {}", Red.bold().paint("[-]"), args),
    )
}

pub fn path_to_string<P: AsRef<Path> + ?Sized>(path: &P) -> String {
    format!("{}", path.as_ref().display())
}

pub fn cpu_count() -> usize {
    num_cpus::get()
}

pub fn copy_dir<S, D>(source: &S, dest: &D) -> Result<(), UtilError>
where
    S: AsRef<Path> + ?Sized,
    D: AsRef<Path> + ?Sized,
{
    let (source, dest) = (source.as_ref(), dest.as_ref());

    let parent = match source.parent() {
        Some(val) => val,
        // FIXME: figure out what this should do (basically this means the source is '/', which i don't think can happen)
        None => unimplemented!(),
    };

    for entry in WalkDir::new(source) {
        let entry = entry.map_err(|e| UtilError::DirEntry(e))?;

        let subpath = entry
            .path()
            .strip_prefix(parent)
            .map_err(|e| UtilError::PathPrefix(path_to_string(entry.path()), e))?;

        let path = dest.join(subpath);
        if entry.file_type().is_dir() {
            fs::create_dir(&path).map_err(|e| UtilError::CreateDir(path_to_string(&path), e))?;
        } else {
            fs::copy(entry.path(), &path).map_err(|e| {
                UtilError::Copy(path_to_string(entry.path()), path_to_string(&path), e)
            })?;
        }
    }

    Ok(())
}
