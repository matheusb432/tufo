use std::{
    io::{self, Read as _},
    path::PathBuf,
};

use tufo_client::desktop::files;

use crate::ui::TourId;

const TOURS: &[TourId] = &[
    TourId::Home,
    TourId::Songs,
    TourId::Recent,
    TourId::Playlists,
    TourId::Soundboards,
    TourId::Playlist,
    TourId::Soundboard,
    TourId::NewCollection,
    TourId::AddToPlaylist,
    TourId::EditCollection,
    TourId::EditTrack,
    TourId::EditSound,
    TourId::Artwork,
    TourId::Search,
    TourId::Settings,
    TourId::Kenku,
    TourId::LoopEditor,
];

fn stable_id(tour: TourId) -> &'static str {
    match tour {
        TourId::Home => "home",
        TourId::Songs => "songs",
        TourId::Recent => "recent",
        TourId::Playlists => "playlists",
        TourId::Soundboards => "soundboards",
        TourId::Playlist => "playlist",
        TourId::Soundboard => "soundboard",
        TourId::NewCollection => "new-collection",
        TourId::AddToPlaylist => "add-to-playlist",
        TourId::EditCollection => "edit-collection",
        TourId::EditTrack => "edit-track",
        TourId::EditSound => "edit-sound",
        TourId::Artwork => "artwork",
        TourId::Search => "search",
        TourId::Settings => "settings",
        TourId::Kenku => "kenku",
        TourId::LoopEditor => "loop-editor",
    }
}

const STORAGE_BYTES_MAX: u64 = 4096;

#[derive(Default)]
pub(super) struct SeenTours(Vec<TourId>);

impl SeenTours {
    pub(super) fn contains(&self, tour: TourId) -> bool {
        self.0.contains(&tour)
    }

    pub(super) fn insert(&mut self, tour: TourId) {
        if !self.contains(tour) {
            self.0.push(tour);
        }
    }

    fn decode(text: &str) -> Self {
        let mut seen = Self::default();
        if text.len() > usize::try_from(STORAGE_BYTES_MAX).unwrap_or(4096) {
            return seen;
        }
        for line in text.lines() {
            if let Some(tour) = TOURS.iter().find(|tour| stable_id(**tour) == line.trim()) {
                seen.insert(*tour);
            }
        }
        seen
    }

    fn encode(&self) -> String {
        self.0
            .iter()
            .map(|tour| stable_id(*tour))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub(super) struct SeenFile(PathBuf);

impl SeenFile {
    pub(super) fn from_environment() -> Option<Self> {
        let directories = directories::ProjectDirs::from("", "", "tufo")?;
        Some(Self(
            directories
                .state_dir()
                .unwrap_or_else(|| directories.data_local_dir())
                .join("guided-tours.seen"),
        ))
    }

    pub(super) fn load(&self) -> io::Result<SeenTours> {
        let mut text = String::new();
        match std::fs::File::open(&self.0) {
            Ok(file) => {
                file.take(STORAGE_BYTES_MAX + 1).read_to_string(&mut text)?;
                Ok(SeenTours::decode(&text))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SeenTours::default()),
            Err(error) => Err(error),
        }
    }

    pub(super) fn save(&self, seen: &SeenTours) -> io::Result<()> {
        files::write_atomically(&self.0, &seen.encode())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_and_oversized_state_never_enters_the_catalog() {
        let seen = SeenTours::decode("home\n{broken\nsettings\nhome\nunknown\n");
        assert_eq!(seen.encode(), "home\nsettings");
        assert_eq!(SeenTours::decode(&"x".repeat(4097)).encode(), "");
        let mut seen = SeenTours::default();
        for _ in 0..100 {
            for tour in TOURS {
                seen.insert(*tour);
            }
        }
        assert_eq!(seen.0.len(), TOURS.len());
    }

    #[test]
    fn restart_and_unavailable_storage_preserve_other_settings() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let settings = folder.path().join("config.toml");
        std::fs::write(&settings, "language = \"pt-BR\"\n")?;
        let file = SeenFile(folder.path().join("state/guided-tours.seen"));
        let mut seen = file.load()?;
        seen.insert(TourId::Home);
        file.save(&seen)?;
        assert!(file.load()?.contains(TourId::Home));
        let blocked = folder.path().join("blocked.seen");
        std::fs::create_dir(&blocked)?;
        let unavailable = SeenFile(blocked);
        assert!(unavailable.load().is_err());
        assert!(unavailable.save(&seen).is_err());
        assert_eq!(std::fs::read_to_string(settings)?, "language = \"pt-BR\"\n");
        Ok(())
    }
}
