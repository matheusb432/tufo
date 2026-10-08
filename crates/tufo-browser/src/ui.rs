//! Work for CEF's UI thread, which owns every browser. Other threads hand it closures.

#![allow(
    clippy::transmute_ptr_to_ptr,
    reason = "cef-rs's wrap macros transmute the reference to each object's base"
)]

use std::{
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use cef::{
    ImplTask, Task, ThreadId, WrapTask, post_delayed_task, post_task, rc::Rc as _, wrap_task,
};
use tokio::sync::oneshot;

type Work = Box<dyn FnOnce() + Send>;

wrap_task! {
    struct ClosureTask {
        work: Arc<Mutex<Option<Work>>>,
    }

    impl Task {
        fn execute(&self) {
            let work = self.work.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(work) = work {
                work();
            }
        }
    }
}

fn task(work: impl FnOnce() + Send + 'static) -> Task {
    ClosureTask::new(Arc::new(Mutex::new(Some(Box::new(work)))))
}

/// Runs `work` on the UI thread soon; work posted after shutdown never runs.
pub(crate) fn run_on_ui(work: impl FnOnce() + Send + 'static) {
    post_task(ThreadId::UI, Some(&mut task(work)));
}

pub(crate) fn run_on_ui_after(delay: Duration, work: impl FnOnce() + Send + 'static) {
    let delay_ms = i64::try_from(delay.as_millis()).unwrap_or(i64::MAX);
    post_delayed_task(ThreadId::UI, Some(&mut task(work)), delay_ms);
}

#[derive(Debug, thiserror::Error)]
#[error("the browser is closing")]
pub(crate) struct BrowserClosing;

/// Runs `work` on the UI thread and returns its result.
pub(crate) async fn call_on_ui<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, BrowserClosing> {
    let (sender, receiver) = oneshot::channel();
    run_on_ui(move || {
        let _ = sender.send(work());
    });
    receiver.await.map_err(|_| BrowserClosing)
}
