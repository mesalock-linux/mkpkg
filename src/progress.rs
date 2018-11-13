use crossbeam;
use crossbeam_utils;
use crossbeam::queue::SegQueue as Queue;
use failure::{Error, Fail};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use std::fmt;
use std::fs;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Mutex;
use std::time::Duration;

use config::Config;
use package::BuildFile;
use util::{self, path_to_string};

#[derive(Debug, Fail)]
pub enum ProgressError {
    #[fail(display = "failed to clear progress bars: {}", _0)]
    Multibar(#[cause] io::Error),

    #[fail(display = "could not create directory '{}': {}", _0, _1)]
    CreateDir(String, #[cause] io::Error),
}

#[derive(Debug)]
pub struct AggregateError {
    pub(crate) errs: Vec<Error>,
}

impl Fail for AggregateError {}

impl fmt::Display for AggregateError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(
            f,
            "found the following {} error(s) while working on packages",
            self.errs.len()
        )?;
        // TODO: this would be a good place to use textwrap
        for e in &self.errs {
            writeln!(f, "\t{}", e)?;
        }
        Ok(())
    }
}

pub type InitFn<'a> = (Fn(&ProgressBar, &ProgressBar) + Send + Sync + 'a);
pub type IterFn<'a> = (Fn(&Config, &BuildFile, &ProgressBar, &ProgressBar, &Fn(Error))
    -> Result<(), Error>
     + Send
     + Sync
     + 'a);

pub struct Progress<'a> {
    bar_count: usize,
    init_fns: Vec<&'a InitFn<'a>>,
    iter_fns: Vec<&'a IterFn<'a>>,
}

impl<'a> Progress<'a> {
    pub fn new(config: &Config, pkgs: &[BuildFile]) -> Self {
        let mut bar_count = (pkgs.len() + 1).min(util::cpu_count());
        if let Some(dl_num) = config.parallel_download {
            bar_count = bar_count.min(dl_num as usize + 1);
        }
        if let Some(build_num) = config.parallel_build {
            bar_count = bar_count.min(build_num as usize + 1);
        }

        Self {
            bar_count: bar_count.max(2),
            init_fns: vec![],
            iter_fns: vec![],
        }
    }

    pub fn add_step(&mut self, init: &'a InitFn<'a>, iter: &'a IterFn<'a>) -> &mut Self {
        self.init_fns.push(init);
        self.iter_fns.push(iter);
        self
    }

    pub fn run<'b, I: Iterator<Item = &'b BuildFile>>(
        &self,
        config: &Config,
        iter: I,
    ) -> Result<(), AggregateError> {
        if let Err(f) = fs::create_dir_all(config.build_dir) {
            return Err(AggregateError {
                errs: vec![ProgressError::CreateDir(path_to_string(config.build_dir), f).into()],
            });
        } else if self.iter_fns.len() == 0 {
            return Ok(());
        }

        let (multibar, total_bar) = Self::create_multibar(config);
        let total_bar = &total_bar;

        let first_queue = Queue::new();
        for buildfile in iter {
            first_queue.push(buildfile);
        }
        let mut queues = vec![first_queue];
        for _ in 1..self.iter_fns.len() {
            queues.push(Queue::new());
        }

        let queues = &queues[..];
        let init_fns = &self.init_fns[..];
        let iter_fns = &self.iter_fns[..];

        let mut bars = vec![];
        for _ in 1..self.bar_count {
            bars.push(multibar.add(Self::create_progbar()));
        }
        let bars = &bars[..];

        let counter = AtomicUsize::new(self.bar_count.max(2) - 1);
        let counter = &counter;

        // TODO: reduce flickering when building many packages (use multibar.set_move_cursor(true))
        let errors = Mutex::new(vec![]);
        crossbeam_utils::thread::scope(|s| {
            let errors = &errors;
            // FIXME: this is a stupid way to signal the thread
            let (_tx, rx) = mpsc::channel();
            s.spawn(move |_| loop {
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(()) | Err(RecvTimeoutError::Timeout) => {
                        for bar in bars {
                            bar.tick();
                        }
                    }
                    _ => break,
                };
            });
            for i in 0..self.bar_count - 1 {
                s.spawn(move |_| {
                    let progbar = &bars[i];
                    self.progress_handler(
                        config, init_fns, iter_fns, queues, errors, total_bar, progbar,
                    );

                    progbar.finish_with_message("Done");
                    if counter.fetch_sub(1, Ordering::SeqCst) == 1 {
                        total_bar.finish_and_clear();
                    }
                });
            }

            if let Err(f) = multibar.join() {
                errors
                    .lock()
                    .unwrap()
                    .push(ProgressError::Multibar(f).into());
            }
        }).unwrap();

        let errors = errors.into_inner().unwrap();
        if errors.len() > 0 {
            Err(AggregateError { errs: errors })
        } else {
            Ok(())
        }
    }

    // TODO: allow downloads and builds to have different numbers of threads executing in parallel
    fn progress_handler<'b>(
        &self,
        config: &Config,
        init_fns: &[&'a InitFn<'a>],
        iter_fns: &[&'a IterFn<'a>],
        queues: &[Queue<&'b BuildFile>],
        errors: &Mutex<Vec<Error>>,
        total_bar: &ProgressBar,
        progbar: &ProgressBar,
    ) {
        let mut current_queue = 0;

        init_fns[current_queue](total_bar, progbar);

        loop {
            if let Some(buildfile) = queues[current_queue].try_pop() {
                let add_error = |err: Error| {
                    let mut errors = errors.lock().unwrap();
                    errors.push(err);
                    total_bar.set_message(&errors.len().to_string());

                    if config.fail_fast {
                        // TODO: figure out a way to exit immediately
                    }
                };

                let builddir = buildfile.base_dir(config);
                if !builddir.exists() {
                    if let Err(f) = fs::create_dir(&builddir) {
                        let nerr = ProgressError::CreateDir(path_to_string(&builddir), f).into();
                        add_error(nerr);
                        continue;
                    }
                }

                if let Err(f) =
                    iter_fns[current_queue](config, buildfile, progbar, total_bar, &add_error)
                {
                    add_error(f);
                } else {
                    if current_queue + 1 < queues.len() {
                        queues[current_queue + 1].push(buildfile);
                    } else {
                        // XXX: is it clearer if the total bar increases even on failure?
                        total_bar.inc(1);
                    }
                }
            } else {
                current_queue += 1;
                if current_queue == queues.len() {
                    break;
                }

                init_fns[current_queue](total_bar, progbar);
            }
        }
    }

    // spawn bar_count progress bars (with one being the total progress bar)
    fn create_multibar(config: &Config) -> (MultiProgress, ProgressBar) {
        let multibar = MultiProgress::new();
        // XXX: not sure if this is the best way (if we keep it like this, we need to display progress another way though)
        if config.verbose {
            multibar.set_draw_target(::indicatif::ProgressDrawTarget::hidden());
        }

        let total_bar = multibar.add(Self::create_total_progbar());
        total_bar.set_message("0");

        (multibar, total_bar)
    }

    fn create_progbar() -> ProgressBar {
        let progbar = ProgressBar::new(0);
        progbar
    }

    fn create_total_progbar() -> ProgressBar {
        let progbar = ProgressBar::new(0);
        progbar.set_style(Self::total_style());
        progbar
    }

    fn total_style() -> ProgressStyle {
        ProgressStyle::default_bar()
            .template("{prefix}{wide_bar} {pos}/{len} packages ({msg} errors)")
    }
}
