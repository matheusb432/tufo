//! Writing every saved loop into a portable document.

use std::collections::HashMap;

use tufo_models::library::{AudioFile, song_loop::SongLoop};

use super::{
    Library, LibraryStorageError,
    library_files::LibraryFiles,
    song_loop_document::{DocumentSong, DocumentSongIdentity, SongLoopDocument},
};

/// Each loop names the audio it was saved against, including loops that need review.
pub async fn execute<F: LibraryFiles>(library: &Library<F>) -> Result<String, LibraryStorageError> {
    let mut database = library.database();
    let paths = AudioFile::all()
        .exec(&mut database)
        .await?
        .into_iter()
        .map(|file| (file.id, file.file_path))
        .collect::<HashMap<_, _>>();
    let mut songs = Vec::new();
    for saved in SongLoop::all().exec(&mut database).await? {
        let Some(path) = paths.get(&saved.audio_file_id) else {
            continue;
        };
        let region = saved
            .region()
            .map_err(|error| LibraryStorageError(error.into()))?;
        songs.push(DocumentSong {
            path: path.clone(),
            identity: DocumentSongIdentity::Fingerprint(saved.fingerprint),
            timing: region.timing(),
            markers: region.markers(),
        });
    }
    songs.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(SongLoopDocument { songs }.to_json())
}
