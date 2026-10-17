//! Default titles for what scans find on disk.

use std::path::Path;

use tufo_models::{library::Title, path::FolderPath, playback::AudioFilePath};

/// A linked collection's title: its folder's path under the library folder, joined with " / ",
/// or the library folder's own name for the library folder itself.
#[must_use]
pub fn linked_collection_title(library_folder: &FolderPath, folder: &FolderPath) -> Title {
    let relative = folder
        .as_ref()
        .strip_prefix(library_folder.as_ref())
        .unwrap_or(folder.as_ref());
    let components = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>();
    title_or_name(&components.join(" / "), folder.as_ref())
}

/// An audio file's default title: its file name without the extension.
#[must_use]
pub fn audio_file_title(path: &AudioFilePath) -> Title {
    let stem = path
        .as_ref()
        .file_stem()
        .map(|stem| stem.to_string_lossy())
        .unwrap_or_default();
    title_or_name(&stem, path.as_ref())
}

/// `text` as a title, or the path's last component when `text` is blank.
fn title_or_name(text: &str, path: &Path) -> Title {
    if !text.trim().is_empty() {
        return Title::from_name_lossy(text);
    }
    let name = path
        .file_name()
        .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy());
    Title::from_name_lossy(&name)
}
