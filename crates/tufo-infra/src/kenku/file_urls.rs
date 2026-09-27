//! Paths from Kenku FM's `encodeFilePath` URLs, such as `file://C:/Music/a.mp3` on Windows.

use std::path::PathBuf;

use percent_encoding::percent_decode_str;

const FILE_URL_PREFIX: &str = "file://";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathStyle {
    Unix,
    Windows,
}

impl PathStyle {
    const CURRENT: Self = if cfg!(windows) {
        Self::Windows
    } else {
        Self::Unix
    };
}

pub(super) fn file_url_path(url: &str) -> Option<PathBuf> {
    local_path_text(url, PathStyle::CURRENT).map(PathBuf::from)
}

fn local_path_text(url: &str, style: PathStyle) -> Option<String> {
    let encoded = url.strip_prefix(FILE_URL_PREFIX)?;
    let decoded = percent_decode_str(encoded).decode_utf8().ok()?;
    match style {
        PathStyle::Unix => decoded.starts_with('/').then(|| decoded.into_owned()),
        PathStyle::Windows => {
            let without_root_slash = match decoded.strip_prefix('/') {
                Some(rest) if starts_with_drive(rest) => rest,
                _ => &decoded,
            };
            let windows = without_root_slash.replace('/', "\\");
            (starts_with_drive(&windows) || windows.starts_with(r"\\")).then_some(windows)
        }
    }
}

fn starts_with_drive(text: &str) -> bool {
    let mut characters = text.chars();
    characters
        .next()
        .is_some_and(|letter| letter.is_ascii_alphabetic())
        && characters.next() == Some(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_urls_decode_to_absolute_paths() {
        assert_eq!(
            local_path_text(
                "file:///home/me/music/%5Bcalm%5D-mist%20slowed%20%C3%A7.ogg",
                PathStyle::Unix
            )
            .as_deref(),
            Some("/home/me/music/[calm]-mist slowed ç.ogg")
        );
    }

    #[test]
    fn windows_urls_decode_to_drive_and_share_paths() {
        assert_eq!(
            local_path_text(
                "file://C:/Users/Me/M%C3%BAsica/a%20b.mp3",
                PathStyle::Windows
            )
            .as_deref(),
            Some(r"C:\Users\Me\Música\a b.mp3")
        );
        assert_eq!(
            local_path_text("file:///D:/sfx/door.wav", PathStyle::Windows).as_deref(),
            Some(r"D:\sfx\door.wav")
        );
        assert_eq!(
            local_path_text("file:////nas/rpg/boss.ogg", PathStyle::Windows).as_deref(),
            Some(r"\\nas\rpg\boss.ogg")
        );
    }

    #[test]
    fn other_urls_and_relative_paths_name_no_file() {
        assert_eq!(
            local_path_text("https://example.com/a.mp3", PathStyle::Unix),
            None
        );
        assert_eq!(local_path_text("file://music/a.mp3", PathStyle::Unix), None);
        assert_eq!(
            local_path_text("file://music/a.mp3", PathStyle::Windows),
            None
        );
        assert_eq!(local_path_text("file:///a%FF.mp3", PathStyle::Unix), None);
    }
}
