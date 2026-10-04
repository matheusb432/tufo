//! Retitling a collection entry's file, or changing how it plays as a sound or the icon its tile
//! shows.

use tufo_models::library::{
    AudioFile, BuiltinTitle, CollectionEntry, CollectionEntryId, SoundSettings,
};
use tufo_wire::library::{SoundSettingsPatch, UpdateCollectionEntry};

use super::{Library, LibraryStorageError, library_files::LibraryFiles};
use crate::{
    error::ErrorMeta,
    playback::{player::Player, update_sound},
};

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum UpdateCollectionEntryError {
    #[error("sound and sound_patch cannot both be supplied")]
    #[meta(class = InvalidArgument, reason = Self::CONFLICTING_SOUND_INPUTS)]
    ConflictingSoundInputs,
    #[error("the entry does not exist")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
}

impl UpdateCollectionEntryError {
    pub const CONFLICTING_SOUND_INPUTS: &'static str =
        "library.update_collection_entry.conflicting_sound_inputs";
    pub const NOT_FOUND: &'static str = "library.update_collection_entry.not_found";
}

pub async fn execute<F: LibraryFiles>(
    id: CollectionEntryId,
    request: UpdateCollectionEntry,
    library: &Library<F>,
    player: &impl Player,
) -> Result<(), UpdateCollectionEntryError> {
    if request.sound.is_some() && request.sound_patch.is_some() {
        return Err(UpdateCollectionEntryError::ConflictingSoundInputs);
    }
    let _change = library.begin_change().await;
    let mut database = library.database();
    let entry = CollectionEntry::filter_by_id(id)
        .first()
        .exec(&mut database)
        .await
        .map_err(LibraryStorageError::from)?
        .ok_or(UpdateCollectionEntryError::NotFound)?;
    let UpdateCollectionEntry {
        title,
        sound,
        icon,
        sound_patch,
    } = request;
    let sound = sound
        .or_else(|| sound_patch.and_then(|patch| apply_sound_patch(entry.sound_settings(), patch)));
    if let Some(title) = title {
        AudioFile::update_by_id(entry.audio_file_id)
            .title(title)
            .builtin_title(None::<BuiltinTitle>)
            .exec(&mut database)
            .await
            .map_err(LibraryStorageError::from)?;
    }
    if let Some(sound) = sound {
        CollectionEntry::update_by_id(id)
            .looping(sound.looping)
            .volume(sound.volume)
            .fade_in(sound.fade_in)
            .fade_out(sound.fade_out)
            .exec(&mut database)
            .await
            .map_err(LibraryStorageError::from)?;
    }
    if let Some(icon) = icon {
        CollectionEntry::update_by_id(id)
            .icon(icon)
            .exec(&mut database)
            .await
            .map_err(LibraryStorageError::from)?;
    }
    library.publish().await?;
    if let Some(sound) = sound {
        update_sound::execute(id, sound, player, library.database()).await;
    }
    Ok(())
}

fn apply_sound_patch(current: SoundSettings, patch: SoundSettingsPatch) -> Option<SoundSettings> {
    if patch.looping.is_none()
        && patch.volume.is_none()
        && patch.fade_in.is_none()
        && patch.fade_out.is_none()
    {
        return None;
    }
    Some(SoundSettings {
        looping: patch.looping.unwrap_or(current.looping),
        volume: patch.volume.unwrap_or(current.volume),
        fade_in: patch.fade_in.unwrap_or(current.fade_in),
        fade_out: patch.fade_out.unwrap_or(current.fade_out),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tufo_models::library::{FadeDuration, Volume};

    use super::*;

    #[test]
    fn sound_patches_keep_current_unselected_properties_and_apply_false_and_zero()
    -> Result<(), String> {
        let current = SoundSettings {
            looping: true,
            volume: Volume::try_new(0.7).map_err(|error| error.to_string())?,
            fade_in: FadeDuration::try_new(Duration::from_millis(600))
                .map_err(|error| error.to_string())?,
            fade_out: FadeDuration::try_new(Duration::from_millis(900))
                .map_err(|error| error.to_string())?,
        };
        let volume = Volume::try_new(0.0).map_err(|error| error.to_string())?;
        assert_eq!(
            apply_sound_patch(
                current,
                SoundSettingsPatch {
                    looping: Some(false),
                    volume: Some(volume),
                    ..SoundSettingsPatch::default()
                }
            ),
            Some(SoundSettings {
                looping: false,
                volume,
                ..current
            })
        );
        assert_eq!(
            apply_sound_patch(current, SoundSettingsPatch::default()),
            None
        );
        Ok(())
    }
}
