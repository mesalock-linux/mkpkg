use failure::Fail;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rayon;

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::mem;
use std::sync::{self, Arc, Mutex};

#[derive(Debug, Fail)]
pub enum ProgressError {
    #[fail(display = "failed to lock mutex: {}", _0)]
    Lock(#[cause] sync::PoisonError<VecDeque<ProgressBar>>),

    #[fail(display = "failed to clear progress bars: {}", _0)]
    Multibar(#[cause] io::Error),
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

pub struct Progress<In, It, T, E>
where
    In: Fn(&ProgressBar, &Mutex<VecDeque<ProgressBar>>),
    It: Fn(T, &ProgressBar, &ProgressBar, &Mutex<Vec<E>>),
    T: Send,
    E: From<ProgressError> + Fail,
{
    multibar: MultiProgress,
    total_bar: ProgressBar,
    bars: Arc<Mutex<VecDeque<ProgressBar>>>,
    init_fn: Option<In>,
    iter_fn: Option<It>,
    _x: ::std::marker::PhantomData<T>,
    _y: ::std::marker::PhantomData<E>,
}

impl<In, It, T, E> Progress<In, It, T, E>
where
    In: Fn(&ProgressBar, &Mutex<VecDeque<ProgressBar>>) + Send + Sync,
    It: Fn(T, &ProgressBar, &ProgressBar, &Mutex<Vec<E>>) + Send + Sync,
    T: Send + Sync,
    E: From<ProgressError> + Fail,
{
    pub fn new(bar_count: usize) -> Self {
        let (multibar, total_bar, bars) = Self::create_multibar(bar_count);
        Self {
            multibar: multibar,
            total_bar: total_bar,
            bars: bars,
            init_fn: None,
            iter_fn: None,
            _x: ::std::marker::PhantomData,
            _y: ::std::marker::PhantomData,
        }
    }

    //pub fn with_

    pub fn on_init(&mut self, cb: In) {
        self.init_fn = Some(cb);
    }

    pub fn on_iter(&mut self, cb: It) {
        self.iter_fn = Some(cb);
    }

    pub fn run<I: ParallelIterator<Item = T>>(mut self, iter: I) -> Result<(), AggregateError<E>> {
        let mut init_fn = None;
        let mut iter_fn = None;

        mem::swap(&mut init_fn, &mut self.init_fn);
        mem::swap(&mut iter_fn, &mut self.iter_fn);

        if let Some(init_fn) = init_fn {
            init_fn(&self.total_bar, &self.bars);
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
                        let progbar = { self.bars.lock().unwrap().pop_front().unwrap() };

                        iter_fn(item, &progbar, &self.total_bar, &errors);

                        self.bars.lock().unwrap().push_back(progbar);
                    }
                });
                for bar in self.bars.lock().unwrap().iter() {
                    bar.finish();
                }
                self.total_bar.finish_and_clear();
            });

            if let Err(f) = self.multibar.join_and_clear() {
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
    fn create_multibar(bar_count: usize) -> (MultiProgress, ProgressBar, Arc<Mutex<VecDeque<ProgressBar>>>) {
        let multibar = MultiProgress::new();

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
        ProgressStyle::default_bar().template("{wide_bar} {pos}/{len} packages ({msg} errors)")
    }
}
