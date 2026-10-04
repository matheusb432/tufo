//! The stress run's workload: a crossfading playlist, two extra songs repeating short saved loops,
//! a looping ambience, and an effect every 10 s, then a check of the server's playback diagnostics.
//!
//! `just stress` starts an isolated server pinned to two CPUs, loads those CPUs with stress-ng,
//! and runs this against it. It exits non-zero when playback fell behind.

use std::{num::NonZeroU64, path::PathBuf, time::Duration};

use anyhow::{Context as _, bail, ensure};
use clap::Parser;
use tufo_client::{
    LocalEndpoint, TufoClient,
    library::{
        CollectionSource, CollectionView, EntryView, ImportPaths, LibrarySnapshot,
        UpdateCollectionEntry,
    },
    player::{UpdateMusicLayer, UpdatePlayerSettings},
    song_loops::UpdateSongLoop,
};
use tufo_models::{
    discord::{GuildId, VoiceChannelId, VoiceChannelTarget},
    library::{CollectionKind, Volume, song_loop::SourceFrames},
    path::LocalPath,
    playback::{OutputTarget, diagnostics::PlayerDiagnostics, player_state::CrossfadeDuration},
};

const EFFECT_INTERVAL: Duration = Duration::from_secs(10);
/// Skipping ahead now and then makes the run cross several crossfades.
const SKIP_INTERVAL: Duration = Duration::from_secs(45);
const LATE_TICK_P999_MAX: Duration = Duration::from_millis(5);
const LATE_TICK_MAX: Duration = Duration::from_millis(20);
/// Short loops cross a seam every few seconds.
const LOOP_SECONDS_MIN: u64 = 4;
const LOOP_SECONDS_MAX: u64 = 8;
const LOOP_CROSSFADE: Duration = Duration::from_millis(200);

#[derive(Parser)]
struct Arguments {
    /// A folder of music; each folder in it that holds audio becomes a playlist.
    #[arg(long)]
    music: PathBuf,
    /// A folder of sound effects; defaults to the music folder.
    #[arg(long)]
    sounds: Option<PathBuf>,
    /// A Discord voice channel as `guild_id:channel_id`; local output when absent.
    #[arg(long)]
    voice: Option<String>,
    #[arg(long, default_value_t = 10)]
    minutes: u64,
    /// Plays everything at zero volume, for checking the harness itself.
    #[arg(long)]
    silent: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse();
    let sounds_root = std::fs::canonicalize(arguments.sounds.as_ref().unwrap_or(&arguments.music))
        .context("resolving the sounds folder")?;
    let client = TufoClient::connect_lazy(&LocalEndpoint::from_environment()?)?;
    let snapshot = import(&client, &arguments).await?;
    let playlist = snapshot
        .collections
        .iter()
        .filter(|collection| collection.kind == CollectionKind::Playlist)
        .max_by_key(|collection| collection.entries.len())
        .context("the music folder holds no playlist")?;
    let sounds = snapshot
        .collections
        .iter()
        .find(|collection| {
            collection.kind == CollectionKind::Soundboard
                && collection.entries.len() >= 2
                && matches!(&collection.source, CollectionSource::Linked { folder, .. }
                    if folder.as_ref().starts_with(&sounds_root))
        })
        .context("the sounds folder needs at least two audio files")?;
    prepare_sounds(&client, sounds, arguments.silent).await?;
    if let Some(voice) = &arguments.voice {
        client
            .select_output(OutputTarget::DiscordVoice(voice_target(voice)?))
            .await?;
    }
    client
        .update_player_settings(&UpdatePlayerSettings {
            crossfade: Some(CrossfadeDuration::default()),
            volume: arguments.silent.then_some(Volume::try_new(0.0)?),
            ..UpdatePlayerSettings::default()
        })
        .await?;
    for entry in playlist.entries.iter().skip(1).take(2) {
        save_short_loop(&client, entry).await?;
    }
    client.play_collection(playlist.id, None).await?;
    for entry in playlist.entries.iter().skip(1).take(2) {
        client.add_music_layer(entry.id).await?;
        client
            .update_music_layer(
                entry.id,
                UpdateMusicLayer {
                    volume: None,
                    looping: Some(true),
                },
            )
            .await?;
    }
    client.play_sound(sounds.entries[0].id).await?;
    println!(
        "Playing {} ({} tracks) with a looping {} for {} min.",
        playlist.title,
        playlist.entries.len(),
        sounds.entries[0].title,
        arguments.minutes
    );
    run_workload(&client, sounds, Duration::from_mins(arguments.minutes)).await?;
    client.stand_by().await?;
    // Let the fade, output release, and diagnostics tick finish before reading the totals.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let diagnostics = client.player_diagnostics().await?;
    report(&diagnostics)
}

async fn import(client: &TufoClient, arguments: &Arguments) -> anyhow::Result<LibrarySnapshot> {
    let music = LocalPath::try_new(arguments.music.clone())?;
    let sounds = LocalPath::try_new(
        arguments
            .sounds
            .clone()
            .unwrap_or_else(|| arguments.music.clone()),
    )?;
    client
        .import_paths(&ImportPaths {
            kind: CollectionKind::Playlist,
            paths: vec![music],
        })
        .await?;
    client
        .import_paths(&ImportPaths {
            kind: CollectionKind::Soundboard,
            paths: vec![sounds],
        })
        .await?;
    client
        .watch_library()
        .await?
        .next()
        .await
        .context("the server ended the library stream")?
        .map_err(Into::into)
}

/// Saves a loop from a quarter into the song, so the song's layer wraps every few seconds.
async fn save_short_loop(client: &TufoClient, entry: &EntryView) -> anyhow::Result<()> {
    let waveform = client.get_song_waveform(entry.audio_file_id).await?;
    let timing = waveform.timing;
    let frame_count = timing.frame_count.0;
    let rate = u64::from(timing.sample_rate.get());
    let loop_start = frame_count / 4;
    let loop_length = (frame_count / 4).clamp(LOOP_SECONDS_MIN * rate, LOOP_SECONDS_MAX * rate);
    ensure!(
        loop_start >= rate && loop_start + loop_length <= frame_count,
        "{} is too short for a stress loop",
        entry.title
    );
    let crossfade = timing.frames_in(LOOP_CROSSFADE);
    client
        .update_song_loop(&UpdateSongLoop {
            audio_file_id: entry.audio_file_id,
            source_fingerprint: waveform.fingerprint,
            playback_start: Some(SourceFrames(loop_start / 2)),
            loop_start: Some(SourceFrames(loop_start)),
            loop_end: Some(SourceFrames(loop_start + loop_length)),
            crossfade: Some(crossfade),
        })
        .await?;
    Ok(())
}

/// Loops the first sound; the rest stay one-shot.
async fn prepare_sounds(
    client: &TufoClient,
    soundboard: &CollectionView,
    silent: bool,
) -> anyhow::Result<()> {
    for (index, entry) in soundboard.entries.iter().take(2).enumerate() {
        let mut sound = entry.sound;
        sound.looping = index == 0;
        if silent {
            sound.volume = Volume::try_new(0.0)?;
        }
        client
            .update_collection_entry(
                entry.id,
                &UpdateCollectionEntry {
                    sound: Some(sound),
                    ..UpdateCollectionEntry::default()
                },
            )
            .await?;
    }
    Ok(())
}

async fn run_workload(
    client: &TufoClient,
    soundboard: &CollectionView,
    duration: Duration,
) -> anyhow::Result<()> {
    let effect = soundboard.entries[1].id;
    let started = tokio::time::Instant::now();
    let mut effects = tokio::time::interval(EFFECT_INTERVAL);
    let mut skips = tokio::time::interval(SKIP_INTERVAL);
    skips.tick().await;
    while started.elapsed() < duration {
        tokio::select! {
            _ = effects.tick() => client.play_sound(effect).await?,
            _ = skips.tick() => client.skip_to_next_track().await?,
        }
    }
    Ok(())
}

fn report(diagnostics: &PlayerDiagnostics) -> anyhow::Result<()> {
    println!(
        "Music underrun frames: {}",
        diagnostics.music_underrun_frames
    );
    ensure!(
        diagnostics.music_underrun_frames == 0,
        "music playback fell behind for {} frames",
        diagnostics.music_underrun_frames
    );
    println!(
        "Sound underrun frames: {}; local stream errors: {} ({} discarded)",
        diagnostics.sound_underrun_frames,
        diagnostics.local_stream_errors,
        diagnostics.local_stream_errors_discarded
    );
    ensure!(
        diagnostics.sound_underrun_frames == 0,
        "sound playback fell behind for {} frames",
        diagnostics.sound_underrun_frames
    );
    ensure!(
        diagnostics.local_stream_errors == 0 && diagnostics.local_stream_errors_discarded == 0,
        "the local output reported stream errors"
    );
    let Some(pacing) = diagnostics.voice_pacing else {
        println!("Local output: now check pw-top's ERR column for Tufo's node, then listen back.");
        return Ok(());
    };
    println!(
        "Voice ticks: {} ({} over 5 ms, {} over 20 ms); lateness p50 {:?}, p99 {:?}, p99.9 {:?}, max {:?}",
        pacing.ticks,
        pacing.late_ticks,
        pacing.very_late_ticks,
        pacing.lateness_p50,
        pacing.lateness_p99,
        pacing.lateness_p999,
        pacing.lateness_max
    );
    if pacing.lateness_p999 > LATE_TICK_P999_MAX || pacing.lateness_max >= LATE_TICK_MAX {
        bail!(
            "voice ticks ran late: p99.9 must stay at or under {LATE_TICK_P999_MAX:?} and the \
             worst under {LATE_TICK_MAX:?}"
        );
    }
    println!("Voice pacing passed; now confirm by listening that nothing sped up or slowed down.");
    Ok(())
}

fn voice_target(text: &str) -> anyhow::Result<VoiceChannelTarget> {
    let (guild, channel) = text
        .split_once(':')
        .context("--voice takes guild_id:channel_id")?;
    let id = |part: &str| {
        part.parse::<u64>()
            .ok()
            .and_then(NonZeroU64::new)
            .context("Discord ids are positive integers")
    };
    Ok(VoiceChannelTarget {
        guild_id: GuildId::new(id(guild)?),
        channel_id: VoiceChannelId::new(id(channel)?),
    })
}
