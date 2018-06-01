use ansi_term::Color::{Green, Red, Yellow};
use std::fmt;
use std::io::{self, Write};
use std::path::Path;

pub fn display_msg<W: Write>(stream: &mut W, args: fmt::Arguments) -> io::Result<()> {
    writeln!(stream, "{}: {}", crate_name!(), args)
}

pub fn display_err(args: fmt::Arguments) -> io::Result<()> {
    display_msg(&mut io::stderr(), format_args!("{}: {}", Red.bold().paint("error"), args))
}

pub fn display_warn(args: fmt::Arguments) -> io::Result<()> {
    display_msg(&mut io::stderr(), format_args!("{}: {}", Yellow.bold().paint("warning"), args))
}

pub fn display_success(args: fmt::Arguments) -> io::Result<()> {
    display_msg(&mut io::stdout(), format_args!("{} {}", Green.bold().paint("[+]"), args))
}

pub fn display_failure(args: fmt::Arguments) -> io::Result<()> {
    display_msg(&mut io::stdout(), format_args!("{} {}", Red.bold().paint("[-]"), args))
}

pub fn path_to_string<P: AsRef<Path> + ?Sized>(path: &P) -> String {
    format!("{}", path.as_ref().display())
}
