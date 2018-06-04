use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use term_size;

use std::borrow::Cow;
use std::fs::{self, File};
use std::io::{self, Write};
use std::process::{Command, Stdio};

use archive::Archiver;
use config::Config;
use package::BuildFile;
use progress::{AggregateError, Progress, ProgressError};
use util::path_to_string;

#[derive(Debug, Fail)]
pub enum BuildError {
    #[fail(display = "{}", _0)]
    Progress(#[cause] ProgressError),

    #[fail(display = "could not remove directory '{}': {}", _0, _1)]
    RemoveDir(String, #[cause] io::Error),

    #[fail(display = "could not create directory '{}': {}", _0, _1)]
    CreateDir(String, #[cause] io::Error),

    #[fail(display = "could not create log file '{}': {}", _0, _1)]
    LogFile(String, #[cause] io::Error),

    #[fail(display = "could not execute command '{}': {}", _0, _1)]
    Spawn(String, #[cause] io::Error),

    #[fail(display = "issue waiting for command '{}' to exit: {}", _0, _1)]
    Wait(String, #[cause] io::Error),

    #[fail(display = "could not open stdin for '{}'", _0)]
    Stdin(String),

    #[fail(display = "could not write to stdin for '{}': {}", _0, _1)]
    WriteChild(String, #[cause] io::Error),

    #[fail(display = "package '{}' failed on command '{}' with {:?}", _0, _1, _2)]
    Command(String, String, Option<i32>),
}

impl From<ProgressError> for BuildError {
    fn from(err: ProgressError) -> Self {
        BuildError::Progress(err)
    }
}

pub struct Builder {

}

// XXX: it might be cool to have a build progress system based on a reference computer
//      for example, the system could compare the difference in build speed between the user's
//      computer and the time it supposedly takes the reference computer (which would be called
//      standard build units/SBUs or something like that) for several package builds and then
//      estimate the speed of the user's computer relative to the reference computer, using that
//      and the SBUs to estimate the amount of time the build will take and fill the progress bar
//      using that estimate
// XXX: would be nice if build step tracked how long it takes to build a package and displays that
//      at the end (and maybe the amount of time it has taken so far during the build)
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
                // FIXME: this spawns an extra thread for every progress bar (a fix might be to
                //        catch SIGCHLD in another thread and set condvars or something for the
                //        progress bar threads such that they will only stop ticking the progress
                //        bar when the child they spawned is dead).  we might be able to use
                //        `wait_timeout` for this.
                bar.enable_steady_tick(250);
            }

            total_bar.set_prefix("Building... ");
            total_bar.set_length(pkgs.len() as u64);
            total_bar.tick();
        });

        progress.on_iter(|pkg: &BuildFile, progbar, _total_bar, _add_error| {
            progbar.set_prefix(pkg.name());

            let steps = &[pkg.prepare(), pkg.build(), pkg.install()];

            // FIXME: verbose mode doesn't work well as it interferes with the progress bar (perhaps
            //        disable the progress bar if verbose mode is enabled?)
            let (stdout, stderr) = if !config.verbose {
                // TODO: write to correct log directories
                let (stdout_name, stderr_name) = (pkg.stdout_log(config), pkg.stderr_log(config));
                let stdout = File::create(&stdout_name).map_err(|e| BuildError::LogFile(stdout_name, e))?;
                let stderr = File::create(&stderr_name).map_err(|e| BuildError::LogFile(stderr_name, e))?;
                (Some(stdout), Some(stderr))
            } else {
                (None, None)
            };

            let pkgdir = pkg.builddir(config).join("pkgdir");
            if pkgdir.exists() {
                fs::remove_dir_all(&pkgdir).map_err(|e| BuildError::RemoveDir(path_to_string(&pkgdir), e))?;
            }
            fs::create_dir(pkgdir).map_err(|e| BuildError::CreateDir(path_to_string(&pkgdir), e))?;

            for &step in steps {
                self.run_step(progbar, config, pkg, step, stdout.as_ref(), stderr.as_ref())?;
            }

            // now that everything is built and put in place we need to package up pkgdir
            progbar.set_message("packaging");
            // TODO: handle error
            Archiver::new().package(config, pkg).unwrap();

            Ok(())
        });

        progress.run(config, pkgs.par_iter())
    }

    fn run_step(&self, progbar: &ProgressBar, config: &Config, pkg: &BuildFile, step: Option<&Vec<String>>, stdout: Option<&File>, stderr: Option<&File>) -> Result<(), BuildError> {
        let (stdout_name, stderr_name) = if !config.verbose {
            (Some(pkg.stdout_log(config)), Some(pkg.stderr_log(config)))
        } else {
            (None, None)
        };

        if let Some(step) = step {
            for cmd in step {
                // XXX: this only prints the first line, which might be the best we can do, but is
                //      not very descriptive (one way to get around this might be to just display
                //      the name of the step we are on instead?  not sure if that is better)
                // NOTE: the "- 6" comes from the template of the progress bar (pkgname: spinner [command])
                //                                                                     123      45       6
                let msg = cmd.lines().next().unwrap();
                let msg = msg.chars().take(term_size::dimensions().unwrap_or((80, 0)).0 - pkg.name().chars().count() - 6).collect::<String>();
                progbar.set_message(&msg);

                let (stdout, stderr) = if !config.verbose {
                    // try to clone the file rather than reopening it
                    // FIXME: rather than failing if the clone fails, this should first probably
                    //        try to open the file again and fail if the open fails
                    let out = Some(stdout.unwrap().try_clone().map_err(|e| BuildError::LogFile(stdout_name.clone().unwrap(), e))?);
                    let err = Some(stderr.unwrap().try_clone().map_err(|e| BuildError::LogFile(stderr_name.clone().unwrap(), e))?);
                    (out, err)
                } else {
                    (None, None)
                };

                self.run_command(config, pkg, cmd, stdout, stderr)?;
            }
        }

        Ok(())
    }

    // TODO: isolate the commands in chroot/namespace (this would require mounting any dependencies
    //       in the chrooted/namespaced environment), which means we would need to specify
    //       dependencies in the build file
    fn run_command(&self, config: &Config, pkg: &BuildFile, cmd: &str, stdout: Option<File>, stderr: Option<File>) -> Result<(), BuildError> {
        let mut sh = Command::new("/bin/sh");
        if let Some(stdout) = stdout {
            sh.stdout(stdout);
        }
        if let Some(stderr) = stderr {
            sh.stderr(stderr);
        }

        if let Some(env) = pkg.env() {
            sh.envs(env);
        }
        // FIXME: it should be like below but can't be as we use pkgdir elsewhere
        //sh.env("pkgdir", config.pkgdir());
        // FIXME: check error
        sh.env("pkgdir", pkg.builddir(config).join("pkgdir").canonicalize().unwrap());
        let mut child = sh
            .current_dir(pkg.archive_out_dir(config))
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| BuildError::Spawn(cmd.to_string(), e))?;
        {
            let stdin = child.stdin.as_mut().ok_or_else(|| BuildError::Stdin(cmd.to_string()))?;
            writeln!(stdin, "{}", cmd).map_err(|e| BuildError::WriteChild(cmd.to_string(), e))?;
        }

        let status = child.wait().map_err(|e| BuildError::Wait(cmd.to_string(), e))?;

        if status.success() {
            Ok(())
        } else {
            // XXX: instead of status code, maybe dump the output?
            Err(BuildError::Command(pkg.name().to_owned(), cmd.to_string(), status.code()))
        }
    }

    fn spinner_style(&self) -> ProgressStyle {
        ProgressStyle::default_spinner().tick_chars(r"/|\- ").template("{prefix:.bold.dim}: {spinner} [{msg}]")
    }
}
