//! The one thread that runs askama's generator and parses Rust with `syn`.
//!
//! Both keep state in thread-locals: the generator its tooling host, proc-macro2 the text of
//! everything it has parsed, which this thread frees after every job. Putting all of it on
//! one named thread keeps that state in one place and the async workers free of it.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, anyhow};

type Job = Box<dyn FnOnce() + Send>;

/// A handle to the generator thread. Cloning it shares the thread.
#[derive(Clone)]
pub struct Generator {
    jobs: mpsc::Sender<Job>,
}

impl Generator {
    pub fn spawn() -> anyhow::Result<Self> {
        let (jobs, queue) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name("askama-generator".to_owned())
            .spawn(move || {
                // Ends when every handle is dropped, with the server.
                for job in queue {
                    // A panic is reported to the caller by its dropped reply, and the thread
                    // carries on with the next job.
                    let _ = catch_unwind(AssertUnwindSafe(job));
                    proc_macro2::extra::invalidate_current_thread_spans();
                }
            })
            .context("starting the generator thread")?;
        Ok(Self { jobs })
    }

    /// Runs `work` on the generator thread and waits for its result.
    pub async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> anyhow::Result<T> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.jobs
            .send(Box::new(move || {
                let _ = reply.send(work());
            }))
            .map_err(|_| anyhow!("the generator thread has stopped"))?;
        result
            .await
            .map_err(|_| anyhow!("the generator panicked on this input"))
    }
}
