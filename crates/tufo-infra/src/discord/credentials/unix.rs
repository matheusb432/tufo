//! On Unix, a private token file grants nothing to its group or other users.

use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::Path,
};

use super::LoadCredentialsError;

/// Creates a new file that only its owner can read or write.
pub(super) fn create_owner_only(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

pub(super) fn ensure_owner_only(path: &Path) -> Result<(), LoadCredentialsError> {
    let mode = std::fs::metadata(path)
        .map_err(LoadCredentialsError::Read)?
        .permissions()
        .mode()
        & 0o777;
    if mode & 0o077 != 0 {
        return Err(LoadCredentialsError::Exposed { mode });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs::Permissions;

    use super::*;
    use crate::discord::credentials::{DiscordCredentials, tests::private_credentials_file};

    #[test]
    fn rejects_a_file_other_users_can_read() {
        let file = private_credentials_file("bot_token = \"abc.def.ghi\"\n");
        std::fs::set_permissions(file.path(), Permissions::from_mode(0o644)).unwrap();

        let error = DiscordCredentials::load(file.path()).unwrap_err();

        assert!(matches!(
            error,
            LoadCredentialsError::Exposed { mode: 0o644 }
        ));
    }
}
