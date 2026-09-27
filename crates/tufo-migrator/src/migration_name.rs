//! Migration names, which become catalogue file names.

use std::str::FromStr;

use thiserror::Error;

const MIGRATION_NAME_LENGTH_MAX: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MigrationName(String);

impl MigrationName {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for MigrationName {
    type Err = MigrationNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let segments_valid = value.split('_').all(migration_name_segment_valid);
        if value.is_empty()
            || value.len() > MIGRATION_NAME_LENGTH_MAX
            || !segments_valid
            || !value.as_bytes()[0].is_ascii_lowercase()
        {
            return Err(MigrationNameError);
        }

        Ok(Self(value.to_owned()))
    }
}

fn migration_name_segment_valid(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .enumerate()
            .all(|(index, byte)| byte.is_ascii_lowercase() || index > 0 && byte.is_ascii_digit())
}

#[derive(Debug, Error)]
#[error("migration name must be 1-64 characters of lowercase snake_case")]
pub(super) struct MigrationNameError;

#[cfg(test)]
mod tests {
    use super::MigrationName;

    #[test]
    fn migration_name_accepts_lowercase_snake_case() {
        let name = "add_sound_settings".parse::<MigrationName>().unwrap();

        assert_eq!(name.as_str(), "add_sound_settings");
    }

    #[test]
    fn migration_name_rejects_unsafe_file_name_shapes() {
        for name in [
            "",
            "AddNodes",
            "add-nodes",
            "add__nodes",
            "../nodes",
            "1_node",
        ] {
            assert!(name.parse::<MigrationName>().is_err(), "accepted {name:?}");
        }
    }
}
