use slint::{ComponentHandle as _, ModelRc, VecModel};
use tufo_client::library::Background;
use tufo_models::library::{BackgroundPreset, CollectionEntryId, CollectionId, SoundIcon};

use crate::{
    presentation::{
        CardView, CollectionPageView, LibraryPageView, NavItemView, PageKind, SoundTileView,
        TrackRowView,
    },
    ui::{AppWindow, Clock, PlayerView, Session, SoundChip},
};

pub(super) fn page(page: PageKind) -> LibraryPageView {
    let cards = |titles: &[&str], base: u128| {
        titles
            .iter()
            .enumerate()
            .map(|(index, title)| CardView {
                id: CollectionId::from_uuid(uuid::Uuid::from_u128(base + index as u128)),
                title: (*title).to_owned(),
                count_label: "14 sounds".to_owned(),
                background: Background::Preset(BackgroundPreset::ALL[index]),
                playing: base == 100 && index == 1,
                missing: false,
            })
            .collect::<Vec<_>>()
    };
    let playlists = cards(
        &[
            "ambient",
            "boss",
            "combat",
            "dungeons",
            "Frostmarch",
            "IronveilCovenant",
            "moment",
            "tavern",
        ],
        1,
    );
    let soundboards = cards(
        &[
            "combat",
            "Effects",
            "environment",
            "monsters",
            "OLD",
            "portals",
            "tunes",
        ],
        100,
    );
    let nav = |cards: &[CardView]| {
        cards
            .iter()
            .map(|card| NavItemView {
                id: card.id,
                title: card.title.clone(),
                playing: card.playing,
                missing: false,
            })
            .collect()
    };
    let collection =
        matches!(page, PageKind::Playlist | PageKind::Soundboard).then(|| CollectionPageView {
            title: "Frostmarch".to_owned(),
            summary: "13 tracks, 112 minutes".to_owned(),
            background: playlists[4].background.clone(),
            linked_folder: None,
            missing: false,
            order_key: 0,
            order_descending: false,
            tracks: (0..13)
                .map(|index| TrackRowView {
                    id: CollectionEntryId::from_uuid(uuid::Uuid::from_u128(200 + index)),
                    title: format!("Track {index}"),
                    duration: "4:20".to_owned(),
                    missing: false,
                    current: false,
                    playing: false,
                })
                .collect(),
            sounds: (0..14)
                .map(|index| SoundTileView {
                    id: CollectionEntryId::from_uuid(uuid::Uuid::from_u128(300 + index)),
                    title: format!("Sound {index}"),
                    icon: SoundIcon::default(),
                    looping: false,
                    volume: 0.8,
                    fade_in_ms: 0,
                    fade_out_ms: 0,
                    playing: false,
                    progress: 0.0,
                    missing: false,
                })
                .collect(),
        });
    LibraryPageView {
        nav_playlists: nav(&playlists),
        nav_soundboards: nav(&soundboards),
        playlist_count: playlists.len(),
        soundboard_count: soundboards.len(),
        playlists,
        soundboards,
        collection,
    }
}

pub(super) fn playback(window: &AppWindow, playing: bool) {
    window.global::<Clock>().set_enabled(playing);
    window.global::<Session>().set_connected(true);
    window
        .global::<PlayerView>()
        .set_sounds(ModelRc::new(VecModel::from(vec![SoundChip {
            id: "fireplace".into(),
            title: "Fireplace".into(),
            progress: 0.2,
            looping: true,
        }])));
}
