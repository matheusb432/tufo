//! Forgetting a song's loop, so it plays its whole file again.

use tufo_models::library::{AudioFileId, song_loop::SongLoop};

use super::{Library, LibraryStorageError, library_files::LibraryFiles};

pub async fn execute<F: LibraryFiles>(
    id: AudioFileId,
    library: &Library<F>,
) -> Result<(), LibraryStorageError> {
    let _change = library.begin_change().await;
    SongLoop::filter_by_audio_file_id(id)
        .delete()
        .exec(&mut library.database())
        .await?;
    library.publish().await
}
