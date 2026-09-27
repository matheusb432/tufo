//! An audio file open for decoding, which every decoder of a track reads at its own position.

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

use symphonia::core::io::MediaSource;

/// An open audio file. Decoders read it through the page cache as they go, so a track costs only
/// its decode-ahead buffer in memory, however long the file. Seeking or moving to another output
/// reopens the same handle, which keeps playing a file renamed or deleted meanwhile.
#[derive(Debug, Clone)]
pub struct TrackFile {
    file: Arc<File>,
    byte_len: u64,
}

impl TrackFile {
    /// Opens `path` for reading. Blocks the calling thread.
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let byte_len = file.metadata()?.len();
        Ok(Self {
            file: Arc::new(file),
            byte_len,
        })
    }

    /// A reader from the start of the file, independent of every other reader.
    pub(super) fn reader(&self) -> TrackFileReader {
        TrackFileReader {
            file: Arc::clone(&self.file),
            byte_len: self.byte_len,
            position: 0,
        }
    }
}

/// Reads a [`TrackFile`] at its own position with positional reads, so readers sharing the file
/// never move each other.
pub(super) struct TrackFileReader {
    file: Arc<File>,
    byte_len: u64,
    position: u64,
}

impl Read for TrackFileReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = read_at(&self.file, buffer, self.position)?;
        self.position += u64::try_from(read).unwrap_or(u64::MAX);
        Ok(read)
    }
}

impl Seek for TrackFileReader {
    fn seek(&mut self, target: SeekFrom) -> io::Result<u64> {
        let position = match target {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(offset) => self.byte_len.checked_add_signed(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
        }
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the seek lands before the start of the file",
            )
        })?;
        self.position = position;
        Ok(position)
    }
}

impl MediaSource for TrackFileReader {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.byte_len)
    }
}

#[cfg(unix)]
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buffer, offset)
}

/// Every reader passes its own offset, so the file pointer `seek_read` moves is never used.
#[cfg(windows)]
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buffer, offset)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn track_file(contents: &[u8]) -> anyhow::Result<(tempfile::NamedTempFile, TrackFile)> {
        let mut temporary = tempfile::NamedTempFile::new()?;
        temporary.write_all(contents)?;
        let file = TrackFile::open(temporary.path())?;
        Ok((temporary, file))
    }

    #[test]
    fn readers_of_one_file_keep_their_own_positions() -> anyhow::Result<()> {
        let (_temporary, file) = track_file(b"abcdef")?;
        let mut first = file.reader();
        let mut second = file.reader();
        let mut buffer = [0; 3];

        first.read_exact(&mut buffer)?;
        second.seek(SeekFrom::End(-2))?;
        let mut tail = Vec::new();
        second.read_to_end(&mut tail)?;
        first.read_exact(&mut buffer)?;

        assert_eq!(&buffer, b"def");
        assert_eq!(tail, b"ef");
        Ok(())
    }

    #[test]
    fn seeking_before_the_start_fails() -> anyhow::Result<()> {
        let (_temporary, file) = track_file(b"abc")?;
        let mut reader = file.reader();

        let error = reader.seek(SeekFrom::Current(-1)).err();

        assert_eq!(
            error.map(|error| error.kind()),
            Some(io::ErrorKind::InvalidInput)
        );
        Ok(())
    }
}
