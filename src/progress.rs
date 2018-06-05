use failure::Fail;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rayon;

use std::collections::VecDeque;
use std::fmt;
use std::fs;
use std::io;
use std::mem;
use std::sync::{self, Arc, Mutex};

use config::Config;
use package::BuildFile;
use util::path_to_string;

#[derive(Debug, Fail)]
pub enum ProgressError {
    #[fail(display = "failed to lock mutex: {}", _0)]
    Lock(#[cause] sync::PoisonError<VecDeque<ProgressBar>>),

    #[fail(display = "failed to clear progress bars: {}", _0)]
    Multibar(#[cause] io::Error),

    #[fail(display = "could not create directory '{}': {}", _0, _1)]
    CreateDir(String, #[cause] io::Error),
}

#[derive(Debug)]
pub struct AggregateError<T: Fail> {
    pub(crate) errs: Vec<T>,
}

impl<T: Fail> Fail for AggregateError<T> { }

impl<T: Fail> fmt::Display for AggregateError<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "found the following {} error(s) while downloading packages", self.errs.len())?;
        for e in &self.errs {
            writeln!(f, "\t{}", e)?;
        }
        Ok(())
    }
}

pub struct Progress<In, It, E>
where
    In: Fn(&ProgressBar, &Mutex<VecDeque<ProgressBar>>),
    It: Fn(&BuildFile, &ProgressBar, &ProgressBar, &Fn(E)) -> Result<(), E>,
    E: From<ProgressError> + Fail,
{
    bar_count: usize,
    init_fn: Option<In>,
    iter_fn: Option<It>,
    _phantom: ::std::marker::PhantomData<E>,
}

impl<In, It, E> Progress<In, It, E>
where
    In: Fn(&ProgressBar, &Mutex<VecDeque<ProgressBar>>) + Send + Sync,
    It: Fn(&BuildFile, &ProgressBar, &ProgressBar, &Fn((E))) -> Result<(), E> + Send + Sync,
    E: From<ProgressError> + Fail,
{
    pub fn new(bar_count: usize) -> Self {
        Self {
            bar_count: bar_count.min(rayon::current_num_threads()),
            init_fn: None,
            iter_fn: None,
            _phantom: ::std::marker::PhantomData,
        }
    }

    pub fn on_init(&mut self, cb: In) {
        self.init_fn = Some(cb);
    }

    pub fn on_iter(&mut self, cb: It) {
        self.iter_fn = Some(cb);
    }

    pub fn run<'a, I: ParallelIterator<Item = &'a BuildFile>>(mut self, config: &Config, iter: I) -> Result<(), AggregateError<E>> {
        if let Err(f) = fs::create_dir_all(config.builddir) {
            return Err(AggregateError { errs: vec![ProgressError::CreateDir(path_to_string(config.builddir), f).into()] });
        }

        let mut init_fn = None;
        let mut iter_fn = None;

        mem::swap(&mut init_fn, &mut self.init_fn);
        mem::swap(&mut iter_fn, &mut self.iter_fn);

        let (multibar, total_bar, bars) = Self::create_multibar(config, self.bar_count.max(2));

        if let Some(init_fn) = init_fn {
            init_fn(&total_bar, &bars);
        }

        let errors = rayon::scope(|s| {
            let errors = Arc::new(Mutex::new(vec![]));
            let errors_clone = errors.clone();
            s.spawn(|_| {
                let errors = errors_clone;
                iter.for_each(|item| {
                    // FIXME: because we no longer control the threads completely, we should check
                    //        the lock result to see if there was an error

                    // this should be fine as we should have the same number of progress bars as
                    // threads rayon uses for this iterator
                    if let Some(ref iter_fn) = iter_fn {
                        let add_error = |err: E| {
                            let mut errors = errors.lock().unwrap();
                            errors.push(err);
                            total_bar.set_message(&errors.len().to_string());
                            total_bar.tick();

                            if config.fail_fast {
                                // TODO: figure out a way to exit immediately
                            }
                        };

                        let builddir = item.builddir(config);
                        if !builddir.exists() {
                            if let Err(f) = fs::create_dir(&builddir) {
                                let nerr = ProgressError::CreateDir(path_to_string(&builddir), f).into();
                                add_error(nerr);
                            }
                        }

                        let progbar = { bars.lock().unwrap().pop_front().unwrap() };

                        if let Err(f) = iter_fn(item, &progbar, &total_bar, &add_error) {
                            add_error(f);
                        }

                        total_bar.inc(1);
                        total_bar.tick();

                        progbar.set_style(ProgressStyle::default_bar().template("Waiting/Done"));
                        progbar.tick();

                        bars.lock().unwrap().push_back(progbar);
                    }
                });
                for bar in bars.lock().unwrap().iter() {
                    bar.finish();
                }
                total_bar.finish_and_clear();
            });

            if let Err(f) = multibar.join_and_clear() {
                errors.lock().unwrap().push(ProgressError::Multibar(f).into());
            }
            errors
        });

        let errors = Arc::try_unwrap(errors).unwrap().into_inner().unwrap();
        if errors.len() > 0 {
            Err(AggregateError { errs: errors })
        } else {
            Ok(())
        }
    }

    // spawn bar_count progress bars (with one being the total progress bar)
    fn create_multibar(config: &Config, bar_count: usize) -> (MultiProgress, ProgressBar, Arc<Mutex<VecDeque<ProgressBar>>>) {
        let multibar = MultiProgress::new();
        // XXX: not sure if this is the best way (if we keep it like this, we need to display progress another way though)
        if config.verbose {
            multibar.set_draw_target(::indicatif::ProgressDrawTarget::hidden());
        }

        let total_bar = multibar.add(Self::create_total_progbar());
        total_bar.set_message("0");

        let mut bars = VecDeque::with_capacity(bar_count - 1);
        for _ in 0..bar_count - 1 {
            bars.push_back(multibar.add(Self::create_progbar()));
        }
        let bars = Arc::new(Mutex::new(bars));

        (multibar, total_bar, bars)
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
        ProgressStyle::default_bar().template("{prefix}{wide_bar} {pos}/{len} packages ({msg} errors)")
    }
}
