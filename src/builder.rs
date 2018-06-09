use failure::Error;
use indicatif::{ProgressBar, ProgressStyle};
use term_size;

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use archive::{ArchiveError, Archiver};
use config::Config;
use package::BuildFile;
use progress::{InitFn, IterFn};
use util::{self, path_to_string};

#[derive(Debug, Fail)]
pub enum BuildError {
    #[fail(display = "{}", _0)]
    Archive(#[cause] ArchiveError),

    #[fail(display = "could not remove directory '{}': {}", _0, _1)]
    RemoveDir(String, #[cause] io::Error),

    #[fail(display = "could not create directory '{}': {}", _0, _1)]
    CreateDir(String, #[cause] io::Error),

    #[fail(display = "could not create log file '{}': {}", _0, _1)]
    LogFile(String, #[cause] io::Error),

    #[fail(display = "could not find real path for '{}': {}", _0, _1)]
    Canonicalize(String, #[cause] io::Error),

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

pub struct Builder {}

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
        Self {}
    }

    pub fn build_setup<'a>(
        &'a self,
        _config: &Config,
        pkgs: &[BuildFile],
    ) -> (Box<InitFn<'a>>, Box<IterFn<'a>>) {
        let pkgslen = pkgs.len();

        let init_fn = move |total_bar: &ProgressBar, bar: &ProgressBar| {
            bar.set_style(self.spinner_style());

            // FIXME: this gets called every time the build step is run for a package
            total_bar.set_prefix("Building... ");
            total_bar.set_length(pkgslen as u64);
        };

        // TODO: make archiver.extract() take a progbar or something (or maybe Archiver::new()?)
        let iter_fn = move |config: &Config,
                            pkg: &BuildFile,
                            progbar: &ProgressBar,
                            _total_bar: &ProgressBar,
                            _add_error: &Fn(Error)| {
            let inner = || -> Result<(), BuildError> {
                progbar.set_prefix(pkg.name());

                progbar.set_message("extracting");
                let archiver = Archiver::new();
                archiver
                    .extract(config, pkg)
                    .map_err(|e| BuildError::Archive(e))?;

                let steps = &[
                    (pkg.download_dir(config), pkg.prepare()),
                    (pkg.archive_out_dir(config), pkg.build()),
                    (pkg.archive_out_dir(config), pkg.install()),
                ];

                // FIXME: verbose mode doesn't work well as it interferes with the progress bar (perhaps
                //        disable the progress bar if verbose mode is enabled?)
                let (stdout, stderr) = if !config.verbose {
                    let logdir = pkg.log_dir(config);
                    if !logdir.exists() {
                        fs::create_dir_all(&logdir)
                            .map_err(|e| BuildError::CreateDir(path_to_string(&logdir), e))?;
                    }
                    let (stdout_name, stderr_name) =
                        (pkg.stdout_log(config), pkg.stderr_log(config));
                    let stdout = File::create(&stdout_name)
                        .map_err(|e| BuildError::LogFile(path_to_string(&stdout_name), e))?;
                    let stderr = File::create(&stderr_name)
                        .map_err(|e| BuildError::LogFile(path_to_string(&stderr_name), e))?;
                    (Some(stdout), Some(stderr))
                } else {
                    (None, None)
                };

                let pkgdir = pkg.pkg_dir(config);
                if pkgdir.exists() {
                    fs::remove_dir_all(&pkgdir)
                        .map_err(|e| BuildError::RemoveDir(path_to_string(&pkgdir), e))?;
                }
                fs::create_dir(&pkgdir)
                    .map_err(|e| BuildError::CreateDir(path_to_string(&pkgdir), e))?;

                for (cur_dir, step) in steps {
                    self.run_step(
                        progbar,
                        config,
                        pkg,
                        &cur_dir,
                        *step,
                        stdout.as_ref(),
                        stderr.as_ref(),
                    )?;
                }

                // now that everything is built and put in place we need to package up pkgdir
                progbar.set_message("packaging");
                archiver
                    .package(config, pkg)
                    .map_err(|e| BuildError::Archive(e))
            };
            inner().map_err(|e| e.into())
        };

        (Box::new(init_fn), Box::new(iter_fn))
    }

    fn run_step(
        &self,
        progbar: &ProgressBar,
        config: &Config,
        pkg: &BuildFile,
        cur_dir: &Path,
        step: Option<&Vec<String>>,
        stdout: Option<&File>,
        stderr: Option<&File>,
    ) -> Result<(), BuildError> {
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
                let msg = msg.chars()
                    .take(
                        term_size::dimensions().unwrap_or((80, 0)).0 - pkg.name().chars().count()
                            - 6,
                    )
                    .collect::<String>();
                progbar.set_message(&msg);

                let (stdout, stderr) = if !config.verbose {
                    // try to clone the file rather than reopening it
                    let out = Some(stdout.unwrap().try_clone().map_err(|e| {
                        BuildError::LogFile(path_to_string(stdout_name.as_ref().unwrap()), e)
                    })?);
                    let err = Some(stderr.unwrap().try_clone().map_err(|e| {
                        BuildError::LogFile(path_to_string(stderr_name.as_ref().unwrap()), e)
                    })?);
                    (out, err)
                } else {
                    (None, None)
                };

                self.run_command(config, pkg, cmd, cur_dir, stdout, stderr)?;
            }
        }

        Ok(())
    }

    // TODO: isolate the commands in chroot/namespace (this would require mounting any dependencies
    //       in the chrooted/namespaced environment), which means we would need to specify
    //       dependencies in the build file
    fn run_command(
        &self,
        config: &Config,
        pkg: &BuildFile,
        cmd: &str,
        cur_dir: &Path,
        stdout: Option<File>,
        stderr: Option<File>,
    ) -> Result<(), BuildError> {
        let mut sh = Command::new("/bin/sh");
        // TODO: verbose mode should probably act like `tee` and write to both stdout/stderr and the logs
        if let Some(stdout) = stdout {
            sh.stdout(stdout);
        }
        if let Some(stderr) = stderr {
            sh.stderr(stderr);
        }

        // TODO: load user-specified default env vars from a file
        //       should replace the below
        sh.env("MAKEFLAGS", format!("-j{}", util::cpu_count()));

        if let Some(env) = pkg.env() {
            sh.envs(env);
        }

        let pkgdir = pkg.pkg_dir(config);
        let builddir = pkg.archive_out_dir(config);
        let srcdir = pkg.download_dir(config);
        sh.env(
            "pkgdir",
            pkgdir
                .canonicalize()
                .map_err(|e| BuildError::Canonicalize(path_to_string(&pkgdir), e))?,
        );
        sh.env(
            "builddir",
            builddir
                .canonicalize()
                .map_err(|e| BuildError::Canonicalize(path_to_string(&builddir), e))?,
        );
        sh.env(
            "srcdir",
            srcdir
                .canonicalize()
                .map_err(|e| BuildError::Canonicalize(path_to_string(&srcdir), e))?,
        );
        let mut child = sh.current_dir(cur_dir)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| BuildError::Spawn(cmd.to_string(), e))?;
        {
            let stdin = child
                .stdin
                .as_mut()
                .ok_or_else(|| BuildError::Stdin(cmd.to_string()))?;
            writeln!(stdin, "{}", cmd).map_err(|e| BuildError::WriteChild(cmd.to_string(), e))?;
        }

        let status = child
            .wait()
            .map_err(|e| BuildError::Wait(cmd.to_string(), e))?;

        if status.success() {
            Ok(())
        } else {
            // XXX: instead of status code, maybe dump the output?
            Err(BuildError::Command(
                pkg.name().to_owned(),
                cmd.to_string(),
                status.code(),
            ))
        }
    }

    fn spinner_style(&self) -> ProgressStyle {
        ProgressStyle::default_spinner()
            .tick_chars(r"/|\- ")
            .template("{prefix:.bold.dim}: {spinner} [{msg}]")
    }
}
