use std::time::Duration;

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use tokio::sync::watch;

use super::MediaState;
use crate::{WindowRequests, tray::TrayCommand};

pub(super) async fn run(
    requests: WindowRequests,
    mut states: watch::Receiver<MediaState>,
) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    let hwnd = None;
    #[cfg(windows)]
    let hwnd = {
        use slint::{
            ComponentHandle as _,
            winit_030::{
                WinitWindowAccessor as _,
                winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle},
            },
        };
        let window = requests
            .window
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("Tufo's window closed"))?;
        let native = window.window().winit_window().await?;
        let RawWindowHandle::Win32(handle) = native.window_handle()?.as_raw() else {
            anyhow::bail!("Tufo has no Windows window handle");
        };
        Some(handle.hwnd.get() as *mut std::ffi::c_void)
    };
    let mut controls = MediaControls::new(PlatformConfig {
        display_name: "Tufo",
        dbus_name: "tufo",
        hwnd,
    })?;
    controls.attach(move |event| {
        let requests = requests.clone();
        let _ = slint::invoke_from_event_loop(move || handle_event(&requests, &event));
    })?;
    let mut previous: Option<MediaState> = None;
    loop {
        let state = states.borrow_and_update().clone();
        let metadata = state.track.as_ref().map(|track| &track.metadata);
        let metadata_previous = previous
            .as_ref()
            .and_then(|state| state.track.as_ref())
            .map(|track| &track.metadata);
        if previous.is_none() || metadata != metadata_previous {
            controls.set_metadata(metadata.map_or_else(MediaMetadata::default, |metadata| {
                MediaMetadata {
                    title: Some(&metadata.title),
                    album: metadata.album.as_deref(),
                    duration: metadata.duration,
                    ..MediaMetadata::default()
                }
            }))?;
        }
        controls.set_playback(match state.track.as_ref() {
            Some(track) if track.paused => MediaPlayback::Paused {
                progress: Some(MediaPosition(track.position)),
            },
            Some(track) => MediaPlayback::Playing {
                progress: Some(MediaPosition(track.position)),
            },
            None => MediaPlayback::Stopped,
        })?;
        previous = Some(state);
        if states.changed().await.is_err() {
            return Ok(());
        }
    }
}

fn handle_event(requests: &WindowRequests, event: &MediaControlEvent) {
    let controller = &requests.controller;
    match event {
        MediaControlEvent::Play => controller.set_music_paused(false),
        MediaControlEvent::Pause => controller.set_music_paused(true),
        MediaControlEvent::Toggle => controller.toggle_play_pause(),
        MediaControlEvent::Next => controller.next_track(),
        MediaControlEvent::Previous => controller.previous_track(),
        MediaControlEvent::Stop => controller.stop_music(),
        MediaControlEvent::Seek(direction) => {
            controller.seek_by(
                *direction == SeekDirection::Forward,
                Duration::from_secs(10),
            );
        }
        MediaControlEvent::SeekBy(direction, offset) => {
            controller.seek_by(*direction == SeekDirection::Forward, *offset);
        }
        MediaControlEvent::SetPosition(position) => controller.seek_to(position.0),
        MediaControlEvent::Raise => requests.run(TrayCommand::ShowWindow),
        MediaControlEvent::Quit => requests.run(TrayCommand::Quit),
        _ => {}
    }
}
