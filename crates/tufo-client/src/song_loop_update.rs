use std::time::Duration;

use tufo_models::library::song_loop::{LoopMarkers, LoopRegion, LoopRegionError};

use crate::{ClientError, TufoClient, song_loops::UpdateSongLoop};

const UPDATE_TIMEOUT: Duration = Duration::from_secs(300);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum SongLoopUpdateError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("the song loop check or update did not finish within five minutes")]
    TimedOut(#[source] tokio::time::error::Elapsed),
    #[error("the library snapshot did not arrive within 30 seconds")]
    SnapshotTimedOut(#[source] tokio::time::error::Elapsed),
    #[error("the server ended the library stream before its first snapshot")]
    SnapshotEnded,
    #[error("the song's audio changed since its markers were placed")]
    SourceChanged,
    #[error(transparent)]
    InvalidMarkers(#[from] LoopRegionError),
}

/// Checks against current audio and saved markers, then sends the original partial request.
pub async fn execute(
    client: &TufoClient,
    update: &UpdateSongLoop,
) -> Result<(), SongLoopUpdateError> {
    tokio::time::timeout(UPDATE_TIMEOUT, async {
        let waveform = client.get_song_waveform(update.audio_file_id).await?;
        if waveform.fingerprint != update.source_fingerprint {
            return Err(SongLoopUpdateError::SourceChanged);
        }
        let library = tokio::time::timeout(SNAPSHOT_TIMEOUT, async {
            let mut snapshots = client.watch_library().await?;
            snapshots
                .next()
                .await
                .ok_or(SongLoopUpdateError::SnapshotEnded)?
                .map_err(SongLoopUpdateError::from)
        })
        .await
        .map_err(SongLoopUpdateError::SnapshotTimedOut)??;
        let base = library.saved_song_loop(update.audio_file_id).map_or_else(
            || LoopMarkers::whole_file(waveform.timing.frame_count),
            |saved| saved.region.markers(),
        );
        LoopRegion::try_new(waveform.timing, update.markers_over(base))?;
        client.update_song_loop(update).await?;
        Ok(())
    })
    .await
    .map_err(SongLoopUpdateError::TimedOut)?
}
