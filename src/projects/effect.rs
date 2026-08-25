//! Background catalogue loading for the Projects surface.
//!
//! The worker owns process-bound discovery and exposes only request/poll. It
//! runs one catalogue at a time, drains queued requests before the next load,
//! and tags completions so the surface can reject stale configuration results.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Instant;

use crate::data::{self, Config, Entry, Theme};
use crate::runner::SystemRunner;
use crate::{source, trace};

struct CatalogRequest {
    generation: u64,
    config: Config,
    theme: Theme,
}

pub(super) struct CatalogCompletion {
    pub generation: u64,
    pub entries: Vec<Entry>,
}

pub(super) enum Poll {
    Pending,
    Ready(CatalogCompletion),
    Disconnected,
}

pub(super) struct CatalogWorker {
    jobs: Sender<CatalogRequest>,
    done: Receiver<CatalogCompletion>,
}

impl CatalogWorker {
    pub fn spawn() -> Self {
        let (job_tx, job_rx) = mpsc::channel::<CatalogRequest>();
        let (done_tx, done_rx) = mpsc::channel::<CatalogCompletion>();
        thread::spawn(move || {
            while let Ok(mut request) = job_rx.recv() {
                while let Ok(newer) = job_rx.try_recv() {
                    request = newer;
                }
                let started = Instant::now();
                let runner = SystemRunner;
                // These ghq calls are independent and each takes a noticeable
                // process-startup cost on macOS. Overlap them, then keep the
                // resulting repository snapshot for both repo rows and probes.
                let (root, repos) = thread::scope(|scope| {
                    let root = scope.spawn(|| data::ghq_root(&runner));
                    let repos = data::load_repo_names(&runner);
                    let root: String = root.join().unwrap_or_default();
                    (root, repos)
                });
                let entries = source::load_all(
                    &request.config,
                    &source::LoadCtx {
                        runner: &runner,
                        theme: &request.theme,
                        root: &root,
                        repos: Some(&repos),
                    },
                );
                if trace::enabled() {
                    trace::span_with("catalog.load", started, &entries.len().to_string());
                    trace::mark_with("sources.ready", &entries.len().to_string());
                }
                if done_tx
                    .send(CatalogCompletion {
                        generation: request.generation,
                        entries,
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
        Self {
            jobs: job_tx,
            done: done_rx,
        }
    }

    pub fn request(&self, generation: u64, config: Config, theme: Theme) -> bool {
        self.jobs
            .send(CatalogRequest {
                generation,
                config,
                theme,
            })
            .is_ok()
    }

    pub fn poll(&self) -> Poll {
        match self.done.try_recv() {
            Ok(completion) => Poll::Ready(completion),
            Err(TryRecvError::Empty) => Poll::Pending,
            Err(TryRecvError::Disconnected) => Poll::Disconnected,
        }
    }
}
