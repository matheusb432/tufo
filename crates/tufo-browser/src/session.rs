//! The open tabs' addresses, saved so the next start restores them. Logins persist in Chromium's
//! own profile beside this file; nothing of a running page does.

use std::{
    io::{self, Write as _},
    path::Path,
};

use serde::{Deserialize, Serialize};
use tufo_models::browser::PageAddress;

const SESSION_FILE_NAME: &str = "session.json";
/// Tabs one browser keeps open; opening another fails until one closes.
pub(crate) const TABS_MAX: usize = 12;

/// The tabs to restore, in order, and which one was selected.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Session {
    pub(crate) addresses: Vec<PageAddress>,
    pub(crate) selected: Option<usize>,
}

#[derive(Serialize, Deserialize)]
struct SessionFile {
    addresses: Vec<String>,
    selected: Option<usize>,
}

impl Session {
    /// Reads the saved session, dropping addresses a tab may not open. A missing or unreadable
    /// file restores no tabs.
    pub(crate) fn load(profile: &Path) -> Self {
        let path = profile.join(SESSION_FILE_NAME);
        let file = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(error) => {
                tracing::warn!(%error, "cannot read the saved tabs");
                return Self::default();
            }
        };
        match serde_json::from_slice::<SessionFile>(&file) {
            Ok(file) => Self::from_file(&file),
            Err(error) => {
                tracing::warn!(%error, "the saved tabs do not parse");
                Self::default()
            }
        }
    }

    fn from_file(file: &SessionFile) -> Self {
        let mut addresses = Vec::new();
        let mut selected = None;
        for (index, address) in file.addresses.iter().enumerate() {
            if addresses.len() == TABS_MAX {
                break;
            }
            let Ok(address) = PageAddress::parse(address) else {
                continue;
            };
            if file.selected == Some(index) {
                selected = Some(addresses.len());
            }
            addresses.push(address);
        }
        Self {
            selected: selected.or_else(|| (!addresses.is_empty()).then_some(0)),
            addresses,
        }
    }

    /// Replaces the saved session at once, so a crash leaves the old one or the new one.
    pub(crate) fn save(&self, profile: &Path) -> io::Result<()> {
        let file = SessionFile {
            addresses: self
                .addresses
                .iter()
                .map(|address| address.as_str().to_owned())
                .collect(),
            selected: self.selected,
        };
        let mut temporary = tempfile::NamedTempFile::new_in(profile)?;
        serde_json::to_writer(&mut temporary, &file)?;
        temporary.flush()?;
        temporary
            .persist(profile.join(SESSION_FILE_NAME))
            .map_err(|error| error.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(text: &str) -> PageAddress {
        PageAddress::parse(text).unwrap()
    }

    #[test]
    fn sessions_survive_a_restart() {
        let profile = tempfile::tempdir().unwrap();
        let session = Session {
            addresses: vec![
                address("https://ambience.example/forest"),
                address("http://music.example/"),
            ],
            selected: Some(1),
        };

        session.save(profile.path()).unwrap();

        assert_eq!(Session::load(profile.path()), session);
    }

    #[test]
    fn missing_and_damaged_sessions_restore_nothing() {
        let profile = tempfile::tempdir().unwrap();
        assert_eq!(Session::load(profile.path()), Session::default());

        std::fs::write(profile.path().join(SESSION_FILE_NAME), b"{not json").unwrap();
        assert_eq!(Session::load(profile.path()), Session::default());
    }

    #[test]
    fn unsafe_addresses_are_dropped_and_the_selection_follows_its_tab() {
        let session = Session::from_file(&SessionFile {
            addresses: vec![
                "file:///home/player/notes.html".to_owned(),
                "https://ambience.example/".to_owned(),
                "chrome://settings".to_owned(),
                "https://music.example/".to_owned(),
            ],
            selected: Some(3),
        });

        assert_eq!(
            session,
            Session {
                addresses: vec![
                    address("https://ambience.example/"),
                    address("https://music.example/")
                ],
                selected: Some(1),
            }
        );
    }

    #[test]
    fn restoring_keeps_at_most_the_tab_limit() {
        let session = Session::from_file(&SessionFile {
            addresses: (0..TABS_MAX + 3)
                .map(|index| format!("https://site{index}.example/"))
                .collect(),
            selected: Some(TABS_MAX + 1),
        });

        assert_eq!(session.addresses.len(), TABS_MAX);
        assert_eq!(session.selected, Some(0));
    }
}
