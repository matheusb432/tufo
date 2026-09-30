use std::{num::NonZeroU64, path::PathBuf, time::Duration};

use tufo_client::library::{
    Background, CollectionSource, CollectionView, EntryView, KenkuImportReport, LibrarySnapshot,
};
use tufo_models::{
    discord::{GuildId, VoiceChannel, VoiceChannelId, VoiceChannelTarget},
    library::{
        AudioFileId, CollectionEntryId, CollectionId, CollectionKind, EntryOrder, SoundSettings,
        Title,
    },
    playback::{
        AudioFilePath, OutputTarget,
        player_state::{ActiveSound, MusicState, PlayerState, PositionSample},
        queue::QueueEntry,
    },
};

use super::*;

fn uuid(seed: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(seed)
}

fn entry(seed: u128, title: &str, seconds: u64) -> EntryView {
    EntryView {
        builtin_title: None,
        id: CollectionEntryId::from_uuid(uuid(seed)),
        audio_file_id: AudioFileId::from_uuid(uuid(seed)),
        title: Title::try_new(title.to_owned()).unwrap(),
        path: AudioFilePath::try_new(std::env::temp_dir().join(PathBuf::from(title))).unwrap(),
        duration: Duration::from_secs(seconds),
        added_at: jiff::Timestamp::UNIX_EPOCH,
        missing: false,
        sound: SoundSettings::default(),
        icon: SoundIcon::default(),
    }
}

fn collection(
    seed: u128,
    kind: CollectionKind,
    title: &str,
    entries: Vec<EntryView>,
) -> CollectionView {
    CollectionView {
        builtin_title: None,
        id: CollectionId::from_uuid(uuid(seed)),
        kind,
        title: Title::try_new(title.to_owned()).unwrap(),
        background: Background::Gradient,
        order: EntryOrder::default(),
        source: CollectionSource::Manual,
        entries,
    }
}

fn snapshot() -> LibrarySnapshot {
    LibrarySnapshot {
        revision: 1,
        collections: vec![
            collection(
                1,
                CollectionKind::Playlist,
                "Boss fights",
                vec![
                    entry(10, "Ancient Dragon", 241),
                    entry(11, "Lich King", 3_725),
                ],
            ),
            collection(
                2,
                CollectionKind::Playlist,
                "Tavern",
                vec![entry(12, "Lute", 60)],
            ),
            collection(
                3,
                CollectionKind::Soundboard,
                "Storm",
                vec![entry(13, "Thunder", 4)],
            ),
        ],
    }
}

#[test]
fn track_metadata_uses_the_file_folder_and_the_date_in_the_local_timezone() {
    let mut entry = entry(10, "Ancient Dragon", 241);
    entry.path = AudioFilePath::try_new(
        std::env::temp_dir()
            .join("RPG music")
            .join("Boss fights")
            .join("dragon.ogg"),
    )
    .unwrap();
    entry.added_at = "2026-01-11T01:30:00Z".parse().unwrap();
    let row = track_row(&entry, &PlayerState::default());
    assert_eq!(
        row.source,
        std::env::temp_dir()
            .join("RPG music")
            .join("Boss fights")
            .to_string_lossy(),
    );
    assert_eq!(
        date_added(
            entry.added_at,
            jiff::tz::TimeZone::fixed(jiff::tz::Offset::constant(-3))
        ),
        "Jan 10, 2026",
    );
}

fn playing(entry_seed: u128, paused: bool) -> PlayerState {
    PlayerState {
        music: Some(MusicState {
            collection_id: CollectionId::from_uuid(uuid(1)),
            entry: QueueEntry {
                entry_id: CollectionEntryId::from_uuid(uuid(entry_seed)),
                path: AudioFilePath::try_new(std::env::temp_dir().join("x")).unwrap(),
                title: "Ancient Dragon".to_owned(),
                duration: Some(Duration::from_secs(241)),
            },
            position: PositionSample {
                position: Duration::from_secs(60),
                sampled_at: SystemTime::UNIX_EPOCH,
            },
            duration: Some(Duration::from_secs(241)),
            paused,
        }),
        ..PlayerState::default()
    }
}

#[test]
fn an_extra_song_keeps_the_playlist_and_player_active_after_the_main_track_stops() {
    let snapshot = snapshot();
    let mut player = PlayerState::default();
    player
        .music_layers
        .try_insert(tufo_models::playback::music_layers::MusicLayerState {
            music: playing(10, false).music.unwrap(),
            volume: tufo_models::library::Volume::default(),
            looping: false,
        })
        .unwrap();
    player.music = playing(11, false).music;
    assert_eq!(
        player_bar(&snapshot, &player, SystemTime::UNIX_EPOCH).title,
        "Lich King"
    );
    player.music = None;
    let page = library_page(
        &snapshot,
        &player,
        Navigation {
            page: PageKind::Playlist,
            collection_id: Some(CollectionId::from_uuid(uuid(1))),
        },
        "",
        SystemTime::UNIX_EPOCH,
    );
    assert!(page.collection.unwrap().tracks[0].playing);
    let bar = player_bar(&snapshot, &player, SystemTime::UNIX_EPOCH);
    assert!(bar.has_playback && bar.playing && !bar.has_music && bar.can_add_layer);
    assert_eq!(bar.title, "Ancient Dragon");
    assert_eq!(bar.layer_count, 1);
    assert_eq!(bar.music_layers.len(), 1);
}

#[test]
fn the_filter_narrows_cards_on_grids_and_entries_on_collection_pages() {
    let snapshot = snapshot();
    let grid = library_page(
        &snapshot,
        &PlayerState::default(),
        Navigation {
            page: PageKind::Playlists,
            collection_id: None,
        },
        "tav",
        SystemTime::UNIX_EPOCH,
    );
    let page = library_page(
        &snapshot,
        &PlayerState::default(),
        Navigation {
            page: PageKind::Playlist,
            collection_id: Some(CollectionId::from_uuid(uuid(1))),
        },
        "king",
        SystemTime::UNIX_EPOCH,
    );

    let titles = grid
        .playlists
        .iter()
        .map(|card| card.title.as_str())
        .collect::<Vec<_>>();
    assert_eq!(titles, ["Tavern"]);
    assert_eq!(grid.playlist_count, 2);
    let tracks = page.collection.unwrap().tracks;
    assert_eq!(tracks.len(), 1);
    assert_eq!(
        (tracks[0].title.as_str(), tracks[0].duration.as_str()),
        ("Lich King", "1:02:05")
    );
}

#[test]
fn the_current_track_shows_as_playing_until_paused() {
    let snapshot = snapshot();
    let navigation = Navigation {
        page: PageKind::Playlist,
        collection_id: Some(CollectionId::from_uuid(uuid(1))),
    };

    let running = library_page(
        &snapshot,
        &playing(10, false),
        navigation,
        "",
        SystemTime::UNIX_EPOCH,
    );
    let paused = library_page(
        &snapshot,
        &playing(10, true),
        navigation,
        "",
        SystemTime::UNIX_EPOCH,
    );

    let running_track = &running.collection.unwrap().tracks[0];
    let paused_track = &paused.collection.unwrap().tracks[0];
    assert!(running_track.current && running_track.playing);
    assert!(paused_track.current && !paused_track.playing);
    assert!(running.playlists[0].playing);
    assert!(!paused.playlists[0].playing);
}

#[test]
fn the_sidebar_lists_every_collection_whatever_the_filter_and_marks_what_plays() {
    let snapshot = snapshot();

    let view = library_page(
        &snapshot,
        &playing(10, false),
        Navigation {
            page: PageKind::Playlists,
            collection_id: None,
        },
        "tav",
        SystemTime::UNIX_EPOCH,
    );

    let playlists = view
        .nav_playlists
        .iter()
        .map(|item| (item.title.as_str(), item.playing))
        .collect::<Vec<_>>();
    assert_eq!(playlists, [("Boss fights", true), ("Tavern", false)]);
    assert_eq!(view.nav_soundboards.len(), 1);
    assert_eq!(view.playlists[0].count_label, "1 track");
}

#[test]
fn a_collection_sums_up_its_tracks_and_minutes_or_its_sounds_and_how_many_play() {
    let snapshot = snapshot();
    let player = PlayerState {
        sounds: vec![ActiveSound {
            entry_id: CollectionEntryId::from_uuid(uuid(13)),
            started_at: SystemTime::UNIX_EPOCH,
            duration: Duration::from_secs(4),
            looping: true,
        }],
        ..PlayerState::default()
    };
    let open = |page, seed| {
        let navigation = Navigation {
            page,
            collection_id: Some(CollectionId::from_uuid(uuid(seed))),
        };
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        library_page(&snapshot, &player, navigation, "", now)
            .collection
            .unwrap()
    };

    let playlist = open(PageKind::Playlist, 1);
    let soundboard = open(PageKind::Soundboard, 3);

    assert_eq!(playlist.summary, "2 tracks, 67 minutes");
    assert_eq!(soundboard.summary, "1 sound, 1 playing");
    assert!((soundboard.sounds[0].progress - 0.25).abs() < 0.01);
}

#[test]
fn the_player_bar_advances_a_running_position_and_counts_down() {
    let snapshot = snapshot();
    let mut player = playing(10, false);
    player.sounds.push(ActiveSound {
        entry_id: CollectionEntryId::from_uuid(uuid(13)),
        started_at: SystemTime::UNIX_EPOCH,
        duration: Duration::from_secs(4),
        looping: true,
    });

    let bar = player_bar(
        &snapshot,
        &player,
        SystemTime::UNIX_EPOCH + Duration::from_secs(5),
    );

    assert_eq!(bar.elapsed, "1:05");
    assert_eq!(bar.remaining, "-2:56");
    assert_eq!(bar.collection_title, "Boss fights");
    assert_eq!(bar.sounds[0].title, "Thunder");
    assert!((bar.sounds[0].progress - 0.25).abs() < 0.01);
}

#[test]
fn a_sound_chip_fills_through_its_current_pass_and_stays_full_once_ended() {
    let snapshot = snapshot();
    let progress_at = |looping: bool, since_start: Duration| {
        let player = PlayerState {
            sounds: vec![ActiveSound {
                entry_id: CollectionEntryId::from_uuid(uuid(13)),
                started_at: SystemTime::UNIX_EPOCH,
                duration: Duration::from_secs(4),
                looping,
            }],
            ..PlayerState::default()
        };
        player_bar(&snapshot, &player, SystemTime::UNIX_EPOCH + since_start).sounds[0].progress
    };

    let stopped_looping = progress_at(false, Duration::from_secs(5));
    let ended = progress_at(false, Duration::from_millis(4_100));

    assert!((stopped_looping - 0.25).abs() < 0.01);
    assert!((ended - 1.0).abs() < f32::EPSILON);
}

fn channel(guild: &str, guild_id: u64, name: &str, channel_id: u64, position: i32) -> VoiceChannel {
    VoiceChannel {
        target: VoiceChannelTarget {
            guild_id: GuildId::new(NonZeroU64::new(guild_id).unwrap()),
            channel_id: VoiceChannelId::new(NonZeroU64::new(channel_id).unwrap()),
        },
        guild_name: guild.to_owned(),
        channel_name: name.to_owned(),
        position,
    }
}

#[test]
fn outputs_list_this_computer_then_each_guilds_channels_by_position() {
    let channels = [
        channel("Moonlit Vale", 2, "Crypt", 21, 1),
        channel("Amber Keep", 3, "Great Hall", 31, 0),
        channel("Moonlit Vale", 2, "Watchtower", 22, 0),
    ];
    let state = PlayerState {
        output: OutputTarget::DiscordVoice(channels[0].target),
        ..PlayerState::default()
    };

    let (rows, targets) = output_rows(&state, &channels);

    let labels = rows
        .iter()
        .map(|row| row.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        [
            "This Computer",
            "Amber Keep",
            "Great Hall",
            "Moonlit Vale",
            "Watchtower",
            "Crypt"
        ]
    );
    let selected = rows.iter().find(|row| row.selected).unwrap();
    assert_eq!(targets[selected.index.unwrap()], state.output);
    assert_eq!(rows[1].initial, "A");
}

#[test]
fn an_active_channel_missing_from_the_list_stays_selected() {
    let gone = channel("Moonlit Vale", 9, "Gone", 99, 0);
    let state = PlayerState {
        output: OutputTarget::DiscordVoice(gone.target),
        ..PlayerState::default()
    };

    let (rows, targets) = output_rows(&state, &[]);

    assert_eq!(rows.last().unwrap().label, "Discord channel 99");
    assert!(rows.last().unwrap().selected);
    assert_eq!(targets.len(), 2);
}

fn output_state(output: OutputTarget, voice: VoiceConnectionState) -> PlayerState {
    PlayerState {
        output,
        voice,
        ..PlayerState::default()
    }
}

#[test]
fn the_rail_shows_this_computer_or_the_active_channel_under_its_guild() {
    let great_hall = channel("Amber Keep", 3, "Great Hall", 31, 0);
    let on_great_hall = output_state(
        OutputTarget::DiscordVoice(great_hall.target),
        VoiceConnectionState::Connected,
    );

    let (local, _) = drawer(
        &output_state(OutputTarget::Local, VoiceConnectionState::NotConfigured),
        std::slice::from_ref(&great_hall),
    );
    let (listed, _) = drawer(&on_great_hall, std::slice::from_ref(&great_hall));
    let (unlisted, _) = drawer(&on_great_hall, &[]);

    assert_eq!(local.current_output.kind, OutputRowKind::Local);
    assert_eq!(local.current_output.label, "This Computer");
    assert_eq!(listed.current_output.kind, OutputRowKind::Channel);
    assert_eq!(
        (
            listed.current_output.label.as_str(),
            listed.current_output.guild_name.as_str(),
            listed.current_output.guild_initial.as_str()
        ),
        ("Great Hall", "Amber Keep", "A")
    );
    assert_eq!(unlisted.current_output.label, "Discord channel 31");
    assert_eq!(unlisted.current_output.guild_initial, "");
}

#[test]
fn the_rail_marks_a_channel_while_voice_joins_it_or_after_it_dropped() {
    let great_hall =
        OutputTarget::DiscordVoice(channel("Amber Keep", 3, "Great Hall", 31, 0).target);
    let badge = |output, voice| drawer(&output_state(output, voice), &[]).0.voice_badge;

    assert_eq!(
        badge(great_hall, VoiceConnectionState::Connecting),
        VoiceBadge::Joining
    );
    assert_eq!(
        badge(
            great_hall,
            VoiceConnectionState::Reconnecting { attempt: 2 }
        ),
        VoiceBadge::Joining
    );
    assert_eq!(
        badge(
            great_hall,
            VoiceConnectionState::Disconnected { reason: None }
        ),
        VoiceBadge::Dropped
    );
    assert_eq!(
        badge(great_hall, VoiceConnectionState::Connected),
        VoiceBadge::None
    );
    assert_eq!(
        badge(
            OutputTarget::Local,
            VoiceConnectionState::Disconnected { reason: None }
        ),
        VoiceBadge::None
    );
}

#[test]
fn backgrounds_survive_the_windows_fields() {
    let image = Background::Image(
        tufo_models::path::ImageFilePath::try_new(std::env::temp_dir().join("tavern.png")).unwrap(),
    );
    let backgrounds = BackgroundPreset::ALL
        .into_iter()
        .map(Background::Preset)
        .chain([Background::Gradient, image]);

    for background in backgrounds {
        let (preset, image_path) = background_fields(&background);

        assert_eq!(
            background_from_fields(preset, &image_path),
            Some(background)
        );
    }
}

#[test]
fn fields_naming_no_background_are_refused() {
    let preset_count = i32::try_from(BackgroundPreset::ALL.len()).unwrap();

    assert_eq!(background_from_fields(preset_count, ""), None);
    assert_eq!(background_from_fields(-1, "relative/tavern.png"), None);
}

#[test]
fn the_picker_groups_every_preset_once() {
    let mut grouped = PRESET_GROUPS
        .iter()
        .flat_map(|(_, presets)| presets.iter().copied())
        .collect::<Vec<_>>();
    grouped.sort();

    assert_eq!(grouped, BackgroundPreset::ALL);
}

#[test]
fn kenku_reports_name_what_came_and_what_stayed_behind() {
    let full = KenkuImportReport {
        collections_imported: 12,
        files_imported: 206,
        skipped_collections: vec![Title::try_new("combat".to_owned()).unwrap()],
        unreadable_files: vec![
            AudioFilePath::try_new(std::env::temp_dir().join("gone.ogg")).unwrap(),
        ],
        unsupported_sources: vec!["https://example.com/a.mp3".to_owned(); 2],
        discord_bot_token_imported: true,
    };
    let repeated = KenkuImportReport {
        skipped_collections: vec![Title::try_new("combat".to_owned()).unwrap(); 2],
        ..KenkuImportReport::default()
    };

    assert_eq!(
        kenku_report_text(&full),
        "Imported 12 collections with 206 files from Kenku FM. 1 collection was already in \
         Tufo. 1 file could not be found or read. Left out 2 web addresses Tufo cannot play. \
         Restarting so the bot logs in with Kenku FM's token."
    );
    assert_eq!(
        kenku_report_text(&repeated),
        "2 collections were already in Tufo."
    );
    assert_eq!(
        kenku_report_text(&KenkuImportReport::default()),
        "Kenku FM's library is empty."
    );
}

#[test]
fn server_diagnostics_never_become_gui_text() {
    use tufo_application::playback::{
        play_entry_sound::PlayEntrySoundError, player::PlayAudioError,
    };
    use tufo_client::error;

    let diagnostic = "secret-path: provider rejected a private request";
    let unreadable = error::status(
        Code::FailedPrecondition,
        diagnostic,
        PlayAudioError::UNREADABLE,
    );
    assert_eq!(
        error_text(&ClientError::Status(unreadable)),
        "Tufo could not read the audio file. Check its permissions."
    );
    let missing = error::status(
        Code::FailedPrecondition,
        diagnostic,
        PlayEntrySoundError::MISSING,
    );
    assert_eq!(
        error_text(&ClientError::Status(missing)),
        "The audio file is missing. Restore it and try again."
    );
    for unknown in [
        tonic::Status::data_loss(diagnostic),
        error::status(Code::Internal, diagnostic, "future_operation.failure"),
        error::status(Code::Internal, diagnostic, PlayAudioError::UNREADABLE),
    ] {
        assert_eq!(
            error_text(&ClientError::Status(unknown)),
            "Tufo could not complete the request. Check the server log."
        );
    }
    assert_eq!(
        voice_status(&VoiceConnectionState::Disconnected {
            reason: Some(diagnostic.to_owned())
        }),
        ""
    );
}

#[test]
fn generated_titles_localize_before_filtering_and_natural_ordering() {
    let mut generated = entry(1, "Original", 1);
    generated.builtin_title = Some(BuiltinTitle::Rain);
    let mut board = collection(
        1,
        CollectionKind::Soundboard,
        "Original",
        vec![entry(2, "Track 10", 1), entry(3, "Track 2", 1), generated],
    );
    board.builtin_title = Some(BuiltinTitle::Effects);
    let mut library = LibrarySnapshot {
        revision: 1,
        collections: vec![board],
    };

    localize_snapshot(&mut library);

    let board = &library.collections[0];
    assert_eq!(board.title.as_str(), "Effects");
    assert_eq!(
        board
            .entries
            .iter()
            .map(|entry| entry.title.as_str())
            .collect::<Vec<_>>(),
        ["Rain", "Track 2", "Track 10"]
    );
    assert_eq!(crate::search::search(&library, "rain")[0].title, "Rain");
}
