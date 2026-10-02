use std::time::Duration;

use mpris_server::{Metadata, PlaybackStatus, Player, Time, TrackId};
use tokio::sync::watch;

use super::MediaState;
use crate::{APP_ID, WindowRequests, tray::TrayCommand};

pub(super) async fn run(
    requests: WindowRequests,
    states: watch::Receiver<MediaState>,
) -> anyhow::Result<()> {
    let player = Player::builder("tufo")
        .identity("Tufo")
        .desktop_entry(APP_ID)
        .can_raise(true)
        .can_quit(true)
        .build()
        .await?;
    bind(&player, &requests);
    tokio::select! {
        () = player.run() => anyhow::bail!("MPRIS service stopped"),
        result = follow_state(&player, states) => result,
    }
}

fn bind(player: &Player, requests: &WindowRequests) {
    let controller = requests.controller.clone();
    player.connect_play(move |_| controller.set_music_paused(false));
    let controller = requests.controller.clone();
    player.connect_pause(move |_| controller.set_music_paused(true));
    let controller = requests.controller.clone();
    player.connect_play_pause(move |_| controller.toggle_play_pause());
    let controller = requests.controller.clone();
    player.connect_next(move |_| controller.next_track());
    let controller = requests.controller.clone();
    player.connect_previous(move |_| controller.previous_track());
    let controller = requests.controller.clone();
    player.connect_stop(move |_| controller.stop_music());
    let controller = requests.controller.clone();
    player.connect_seek(move |_, offset| {
        let micros = offset.as_micros();
        controller.seek_by(micros >= 0, Duration::from_micros(micros.unsigned_abs()));
    });
    let controller = requests.controller.clone();
    player.connect_set_position(move |player, track_id, position| {
        if player.metadata().trackid().as_ref() == Some(track_id)
            && let Ok(micros) = u64::try_from(position.as_micros())
        {
            controller.seek_to(Duration::from_micros(micros));
        }
    });
    let controller = requests.controller.clone();
    player.connect_set_volume(move |_, volume| {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "desktop volume is clamped to the GUI's f32 range"
        )]
        let volume = volume.clamp(0.0, 1.0) as f32;
        controller.set_volume(volume);
    });
    let requests_raise = requests.clone();
    player.connect_raise(move |_| requests_raise.run(TrayCommand::ShowWindow));
    let requests_quit = requests.clone();
    player.connect_quit(move |_| requests_quit.run(TrayCommand::Quit));
}

async fn follow_state(
    player: &Player,
    mut receiver: watch::Receiver<MediaState>,
) -> anyhow::Result<()> {
    let mut previous = None;
    loop {
        let state = receiver.borrow_and_update().clone();
        let position_jumped = state
            .track
            .as_ref()
            .is_some_and(|track| track.position_jumped_since(previous.as_ref()));
        let has_music = state.track.is_some();
        player.set_can_play(has_music).await?;
        player.set_can_pause(has_music).await?;
        player.set_can_go_next(state.can_skip).await?;
        player.set_can_go_previous(state.can_skip).await?;
        player.set_can_seek(state.can_seek).await?;
        player
            .set_volume(if state.muted {
                0.0
            } else {
                f64::from(state.volume.into_inner())
            })
            .await?;
        let (metadata, status, position) = match &state.track {
            Some(track) => {
                let track_id = TrackId::try_from(format!(
                    "/dev/tufo/track/{}",
                    track.metadata.entry_id.to_string().replace('-', "_")
                ))?;
                let mut metadata = Metadata::builder()
                    .trackid(track_id)
                    .title(track.metadata.title.clone())
                    .build();
                metadata.set_album(track.metadata.album.clone());
                metadata.set_length(track.metadata.duration.map(time));
                let status = if track.paused {
                    PlaybackStatus::Paused
                } else {
                    PlaybackStatus::Playing
                };
                (metadata, status, time(track.position))
            }
            None => (
                Metadata::builder().trackid(TrackId::NO_TRACK).build(),
                PlaybackStatus::Stopped,
                Time::ZERO,
            ),
        };
        player.set_metadata(metadata).await?;
        player.set_position(position);
        player.set_playback_status(status).await?;
        if position_jumped {
            player.seeked(position).await?;
        }
        previous = state.track;
        if receiver.changed().await.is_err() {
            return Ok(());
        }
    }
}

fn time(duration: Duration) -> Time {
    Time::from_micros(i64::try_from(duration.as_micros()).unwrap_or(i64::MAX))
}
