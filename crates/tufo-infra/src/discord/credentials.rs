//! The bot token file, which only its owner may read.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::{fmt, io, path::Path};

#[cfg(unix)]
use unix::{create_owner_only, ensure_owner_only};
#[cfg(windows)]
use windows::{create_owner_only, ensure_owner_only};

#[derive(serde::Deserialize, serde::Serialize)]
struct CredentialsFile {
    bot_token: String,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadCredentialsError {
    #[error("the Discord credentials file cannot be read")]
    Read(#[source] io::Error),
    #[cfg(unix)]
    #[error(
        "the Discord credentials file is readable by other users (mode {mode:o}); run chmod 600"
    )]
    Exposed { mode: u32 },
    #[cfg(windows)]
    #[error(
        "the Discord credentials file is readable by other users; remove every permission entry \
         except your own, SYSTEM, and Administrators"
    )]
    Exposed,
    #[cfg(windows)]
    #[error("the Discord credentials file's permissions cannot be read")]
    Permissions(#[source] io::Error),
    // The TOML error's text can quote the token, so it is deliberately dropped.
    #[error("the Discord credentials file must be TOML with a bot_token string")]
    Malformed,
    #[error("the Discord credentials file has an empty bot_token")]
    EmptyToken,
}

#[derive(Debug, thiserror::Error)]
pub enum SaveCredentialsError {
    #[error("the Discord bot token is empty")]
    EmptyToken,
    #[error("the Discord credentials file cannot be written")]
    Write(#[source] io::Error),
    #[error(transparent)]
    Check(#[from] LoadCredentialsError),
}

/// A Discord bot token. Its `Debug` output never shows the token.
#[derive(Clone)]
pub struct DiscordCredentials {
    bot_token: String,
}

impl DiscordCredentials {
    /// Loads `bot_token` from a TOML file that no other user can read.
    pub fn load(path: &Path) -> Result<Self, LoadCredentialsError> {
        ensure_owner_only(path)?;
        let text = std::fs::read_to_string(path).map_err(LoadCredentialsError::Read)?;
        let file = toml::from_str::<CredentialsFile>(&text)
            .map_err(|_| LoadCredentialsError::Malformed)?;
        let bot_token = file.bot_token.trim();
        if bot_token.is_empty() {
            return Err(LoadCredentialsError::EmptyToken);
        }
        Ok(Self {
            bot_token: bot_token.to_owned(),
        })
    }

    /// Replaces the token file at `path` with `bot_token`, readable only by the current user.
    /// The file is written beside `path` and renamed over it, so a reader never sees half a
    /// token.
    pub fn save(path: &Path, bot_token: &str) -> Result<(), SaveCredentialsError> {
        let bot_token = bot_token.trim();
        if bot_token.is_empty() {
            return Err(SaveCredentialsError::EmptyToken);
        }
        let text = toml::to_string(&CredentialsFile {
            bot_token: bot_token.to_owned(),
        })
        .map_err(|error| SaveCredentialsError::Write(io::Error::other(error)))?;
        let folder = path
            .parent()
            .ok_or_else(|| SaveCredentialsError::Write(io::ErrorKind::InvalidInput.into()))?;
        std::fs::create_dir_all(folder).map_err(SaveCredentialsError::Write)?;
        let staging = path.with_extension("toml.new");
        let _ = std::fs::remove_file(&staging);
        let written = create_owner_only(&staging).and_then(|mut file| {
            use std::io::Write as _;
            file.write_all(text.as_bytes())?;
            file.sync_all()
        });
        if let Err(error) = written.and_then(|()| std::fs::rename(&staging, path)) {
            let _ = std::fs::remove_file(&staging);
            return Err(SaveCredentialsError::Write(error));
        }
        ensure_owner_only(path)?;
        Ok(())
    }

    /// Returns whether it saved `bot_token`.
    pub fn save_unless_present(path: &Path, bot_token: &str) -> Result<bool, SaveCredentialsError> {
        if path.try_exists().map_err(SaveCredentialsError::Write)? {
            return Ok(false);
        }
        Self::save(path, bot_token).map(|()| true)
    }

    pub(super) fn bot_token(&self) -> &str {
        &self.bot_token
    }
}

impl fmt::Debug for DiscordCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DiscordCredentials")
            .field("bot_token", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token file only the current user can read.
    pub(super) fn private_credentials_file(contents: &str) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        file
    }

    #[test]
    fn loads_a_private_token_file() {
        let file = private_credentials_file("bot_token = \" abc.def.ghi \"\n");

        let credentials = DiscordCredentials::load(file.path()).unwrap();

        assert_eq!(credentials.bot_token(), "abc.def.ghi");
        assert!(!format!("{credentials:?}").contains("abc"));
    }

    #[test]
    fn a_saved_token_loads_back() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("tufo").join("discord.toml");

        DiscordCredentials::save(&path, " abc.def\"ghi ").unwrap();

        assert_eq!(
            DiscordCredentials::load(&path).unwrap().bot_token(),
            "abc.def\"ghi"
        );
        assert!(matches!(
            DiscordCredentials::save(&path, "  "),
            Err(SaveCredentialsError::EmptyToken)
        ));
    }

    #[test]
    fn a_saved_token_is_never_replaced_by_one_saved_unless_present() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("discord.toml");

        let first = DiscordCredentials::save_unless_present(&path, "first").unwrap();
        let second = DiscordCredentials::save_unless_present(&path, "second").unwrap();

        assert!(first);
        assert!(!second);
        assert_eq!(
            DiscordCredentials::load(&path).unwrap().bot_token(),
            "first"
        );
    }

    #[test]
    fn malformed_files_never_echo_their_contents() {
        let file = private_credentials_file("bot_token = abc.def.ghi\n");

        let error = DiscordCredentials::load(file.path()).unwrap_err();

        assert!(matches!(error, LoadCredentialsError::Malformed));
        assert!(!error.to_string().contains("abc"));
    }
}
