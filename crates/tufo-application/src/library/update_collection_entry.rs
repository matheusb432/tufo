//! Retitling a collection entry's file, or changing how it plays as a sound or the icon its tile
//! shows.

use tufo_models::library::{AudioFile, BuiltinTitle, CollectionEntry, CollectionEntryId};
use tufo_wire::library::UpdateCollectionEntry;

use super::{Library, LibraryStorageError, library_files::LibraryFiles};
use crate::{
    error::ErrorMeta,
    playback::{player::Player, update_sound},
};

#[derive(Debug, thiserror::Error, ErrorMeta)]
pub enum UpdateCollectionEntryError {
    #[error("the entry does not exist")]
    #[meta(class = NotFound, reason = Self::NOT_FOUND)]
    NotFound,
    #[error(transparent)]
    #[meta(transparent)]
    Storage(#[from] LibraryStorageError),
}

impl UpdateCollectionEntryError {
    pub const NOT_FOUND: &'static str = "library.update_collection_entry.not_found";
}

pub async fn execute<F: LibraryFiles>(
    id: CollectionEntryId,
    request: UpdateCollectionEntry,
    library: &Library<F>,
    player: &impl Player,
) -> Result<(), UpdateCollectionEntryError> {
    let _change = library.begin_change().await;
    let mut database = library.database();
    let entry = CollectionEntry::filter_by_id(id)
        .first()
        .exec(&mut database)
        .await
        .map_err(LibraryStorageError::from)?
        .ok_or(UpdateCollectionEntryError::NotFound)?;
    let UpdateCollectionEntry { title, sound, icon } = request;
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
