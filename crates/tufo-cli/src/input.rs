use serde::{Deserialize, Deserializer, de::DeserializeOwned};
use tufo_models::library::{AudioFileId, CollectionEntryId, CollectionId, LibraryFolderId};

const JSON_PAYLOAD_BYTES_MAX: usize = 1024 * 1024;

pub(crate) fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    if text.len() > JSON_PAYLOAD_BYTES_MAX {
        return Err("JSON payload must be at most 1 MiB".to_owned());
    }
    if !text.trim_start().starts_with('{') {
        return Err("JSON payload must be one object".to_owned());
    }
    serde_json::from_str(text).map_err(|error| format!("invalid JSON payload: {error}"))
}

pub(crate) fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Default)]
pub(crate) enum NullableField<T> {
    #[default]
    Omitted,
    Clear,
    Set(T),
}

impl<'de, T> Deserialize<'de> for NullableField<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(|value| value.map_or(Self::Clear, Self::Set))
    }
}

pub(crate) fn parse_collection_id(text: &str) -> Result<CollectionId, String> {
    let error = || "collection ID must be a lowercase hyphenated UUID".to_owned();
    let id = CollectionId::from_uuid(text.parse().map_err(|_| error())?);
    (id.to_string() == text).then_some(id).ok_or_else(error)
}

pub(crate) fn parse_entry_id(text: &str) -> Result<CollectionEntryId, String> {
    let error = || "entry ID must be a lowercase hyphenated UUID".to_owned();
    let id = CollectionEntryId::from_uuid(text.parse().map_err(|_| error())?);
    (id.to_string() == text).then_some(id).ok_or_else(error)
}

pub(crate) fn parse_audio_file_id(text: &str) -> Result<AudioFileId, String> {
    let error = || "audio file ID must be a lowercase hyphenated UUID".to_owned();
    let id = AudioFileId::from_uuid(text.parse().map_err(|_| error())?);
    (id.to_string() == text).then_some(id).ok_or_else(error)
}

pub(crate) fn parse_folder_id(text: &str) -> Result<LibraryFolderId, String> {
    let error = || "folder ID must be a lowercase hyphenated UUID".to_owned();
    let id = LibraryFolderId::from_uuid(text.parse().map_err(|_| error())?);
    (id.to_string() == text).then_some(id).ok_or_else(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_require_lowercase_hyphenated_uuids() -> Result<(), String> {
        let canonical = "01a0cf81-04dd-7d42-9a53-2904e3318bc7";
        assert_eq!(parse_collection_id(canonical)?.to_string(), canonical);
        assert_eq!(parse_entry_id(canonical)?.to_string(), canonical);
        assert_eq!(parse_audio_file_id(canonical)?.to_string(), canonical);
        assert_eq!(parse_folder_id(canonical)?.to_string(), canonical);
        for invalid in [
            canonical.to_uppercase(),
            canonical.replace('-', ""),
            "invalid".to_owned(),
        ] {
            assert!(parse_collection_id(&invalid).is_err());
            assert!(parse_entry_id(&invalid).is_err());
            assert!(parse_audio_file_id(&invalid).is_err());
            assert!(parse_folder_id(&invalid).is_err());
        }
        Ok(())
    }
}
