use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;

pub(super) fn spawn_snapshot_stream<S, M>(
    mut snapshots: watch::Receiver<S>,
    mut draining: watch::Receiver<bool>,
    encode: impl Fn(watch::Ref<'_, S>) -> M + Send + 'static,
) -> ReceiverStream<Result<M, Status>>
where
    S: Send + Sync + 'static,
    M: Send + 'static,
{
    // One slot keeps backpressure: a slow reader holds the forwarder, which then sends the
    // latest snapshot instead of queueing every intermediate one.
    let (sender, receiver) = mpsc::channel(1);
    tokio::spawn(async move {
        loop {
            let message = encode(snapshots.borrow_and_update());
            tokio::select! {
                sent = sender.send(Ok(message)) => {
                    if sent.is_err() {
                        return;
                    }
                }
                _ = draining.wait_for(|draining| *draining) => return,
            }
            tokio::select! {
                changed = snapshots.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                _ = draining.wait_for(|draining| *draining) => return,
                () = sender.closed() => return,
            }
        }
    });
    ReceiverStream::new(receiver)
}
