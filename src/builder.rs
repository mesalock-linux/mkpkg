use indicatif::ProgressStyle;
use rayon::prelude::*;

use std::io::Write;
use std::process::{Command, Stdio};

use config::Config;
use package::BuildFile;
use progress::{AggregateError, Progress, ProgressError};

#[derive(Debug, Fail)]
pub enum BuildError {
    #[fail(display = "{}", _0)]
    Progress(#[cause] ProgressError),
}

impl From<ProgressError> for BuildError {
    fn from(err: ProgressError) -> Self {
        BuildError::Progress(err)
    }
}

pub struct Builder {

}

// XXX: instead of printing all the output to stdout, just dump it in a log
impl Builder {
    pub fn new() -> Self {
        Self { }
    }

    pub fn build_pkgs(&self, config: &Config, pkgs: &[BuildFile]) -> Result<(), AggregateError<BuildError>> {
        let bar_count = pkgs.len() + 1;
        let mut progress = Progress::new(bar_count);

        progress.on_init(|total_bar, bars| {
            for bar in bars.lock().unwrap().iter() {
                bar.set_style(self.spinner_style());
            }

            total_bar.set_length(pkgs.len() as u64);
            total_bar.tick();
        });

        progress.on_iter(|pkg: &BuildFile, progbar, total_bar, errors| {
            progbar.set_prefix(pkg.name());

            if let Err(f) = self.prepare(config, pkg) {
                let mut errors = errors.lock().unwrap();
                errors.push(f);
                total_bar.set_message(&errors.len().to_string());
                total_bar.tick();
            }
        });

        progress.run(config, pkgs.par_iter())
    }

    fn prepare(&self, config: &Config, pkg: &BuildFile) -> Result<(), BuildError> {
        if let Some(prep) = pkg.prepare() {
            for cmd in prep {
                self.run_command(config, pkg, &cmd)?
            }
        }
        
        Ok(())
    }

    // FIXME: obviously this needs to check for errors
    fn run_command(&self, config: &Config, pkg: &BuildFile, cmd: &str) -> Result<(), BuildError> {
        let mut sh = Command::new("/bin/sh");
        if !config.verbose {
            // TODO: write to log (we are just nulling them for now)
            sh.stdout(Stdio::null()).stderr(Stdio::null());
        }
        if let Some(env) = pkg.env() {
            sh.envs(env);
        }
        let mut child = sh.current_dir(pkg.archive_out_dir(config)).stdin(Stdio::piped()).spawn().unwrap();
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, "{}", cmd).unwrap();
        Ok(())
    }

    fn spinner_style(&self) -> ProgressStyle {
        ProgressStyle::default_spinner().template("{prefix:.bold.dim}: {spinner} {pos}/{len}")
    }
}
