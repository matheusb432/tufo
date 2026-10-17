//! The disk adapter against real folders of generated WAV and Ogg Opus files.

use std::{fs, path::Path, time::Duration};

use support::{stereo_sine, wav_bytes, write_ogg_opus};
use tufo_application::library::library_files::{
    InspectAudioError, LibraryFiles as _, ScanFolderError,
};
use tufo_infra::library_files::DiskLibraryFiles;
use tufo_models::{path::FolderPath, playback::AudioFilePath};

mod support;

fn folder(path: &Path) -> anyhow::Result<FolderPath> {
    Ok(FolderPath::try_new(path.to_path_buf())?)
}

fn audio(path: &Path) -> anyhow::Result<AudioFilePath> {
    Ok(AudioFilePath::try_new(path.to_path_buf())?)
}

#[tokio::test]
async fn scans_list_each_folders_audio_files_and_skip_the_rest() {
    let library = tempfile::tempdir().unwrap();
    let boss = library.path().join("FROSTMARCH").join("boss");
    fs::create_dir_all(&boss).unwrap();
    fs::write(boss.join("Dragon.WAV"), wav_bytes(1, 440.0, None)).unwrap();
    fs::write(boss.join("notes.txt"), "not audio").unwrap();
    fs::write(library.path().join("tavern.wav"), wav_bytes(1, 330.0, None)).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(boss.join("Dragon.WAV"), library.path().join("link.wav")).unwrap();

    let scan = DiskLibraryFiles
        .scan_folder(folder(library.path()).unwrap())
        .await
        .unwrap();

    assert!(!scan.truncated);
    let listing = scan
        .folders
        .iter()
        .map(|folder| {
            let names = folder
                .audio_files
                .iter()
                .map(|file| file.path.as_ref().file_name().unwrap().to_owned())
                .collect::<Vec<_>>();
            (folder.path.as_ref().clone(), names)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        listing,
        [
            (library.path().to_path_buf(), vec!["tavern.wav".into()]),
            (library.path().join("FROSTMARCH"), vec![]),
            (boss.clone(), vec!["Dragon.WAV".into()]),
        ]
    );
}

#[tokio::test]
async fn scanning_a_missing_folder_says_so() {
    let library = tempfile::tempdir().unwrap();

    let result = DiskLibraryFiles
        .scan_folder(folder(&library.path().join("gone")).unwrap())
        .await;

    assert!(matches!(result, Err(ScanFolderError::NotFound)));
}

#[tokio::test]
async fn a_rename_keeps_the_file_identity() {
    let library = tempfile::tempdir().unwrap();
    let before = library.path().join("tavern.wav");
    let after = library.path().join("inn.wav");
    fs::write(&before, wav_bytes(1, 330.0, None)).unwrap();
    let stamp = DiskLibraryFiles
        .stat_file(audio(&before).unwrap())
        .await
        .unwrap()
        .unwrap();

    fs::rename(&before, &after).unwrap();

    let renamed = DiskLibraryFiles
        .stat_file(audio(&after).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(renamed, stamp);
    assert_eq!(
        DiskLibraryFiles
            .stat_file(audio(&before).unwrap())
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn fingerprints_ignore_tags_but_not_audio() {
    let library = tempfile::tempdir().unwrap();
    let plain = library.path().join("plain.wav");
    let tagged = library.path().join("tagged.wav");
    let other = library.path().join("other.wav");
    fs::write(&plain, wav_bytes(2, 440.0, None)).unwrap();
    fs::write(&tagged, wav_bytes(2, 440.0, Some("Dragon"))).unwrap();
    fs::write(&other, wav_bytes(2, 441.0, None)).unwrap();

    let plain = DiskLibraryFiles
        .inspect_audio(audio(&plain).unwrap())
        .await
        .unwrap();
    let tagged = DiskLibraryFiles
        .inspect_audio(audio(&tagged).unwrap())
        .await
        .unwrap();
    let other = DiskLibraryFiles
        .inspect_audio(audio(&other).unwrap())
        .await
        .unwrap();

    assert_eq!(plain.fingerprint, tagged.fingerprint);
    assert_ne!(plain.fingerprint, other.fingerprint);
    assert_eq!(plain.duration, Duration::from_secs(2));
}

#[tokio::test]
async fn a_file_that_is_not_audio_is_unsupported() {
    let library = tempfile::tempdir().unwrap();
    let fake = library.path().join("fake.mp3");
    fs::write(&fake, "just text").unwrap();

    assert!(
        DiskLibraryFiles
            .inspect_audio(audio(&fake).unwrap())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn opus_files_are_found_and_timed_without_their_encoder_delay() {
    let library = tempfile::tempdir().unwrap();
    let chime = library.path().join("chime.opus");
    write_ogg_opus(&chime, 2, &stereo_sine(72_000, 440.0, 0.5)).unwrap();

    let scan = DiskLibraryFiles
        .scan_folder(folder(library.path()).unwrap())
        .await
        .unwrap();
    let facts = DiskLibraryFiles
        .inspect_audio(audio(&chime).unwrap())
        .await
        .unwrap();

    assert_eq!(scan.folders[0].audio_files[0].path.as_ref(), &chime);
    assert_eq!(facts.duration, Duration::from_millis(1_500));
}

#[tokio::test]
async fn an_mp3_behind_cover_art_larger_than_the_probe_depth_is_found_and_timed() {
    const FRAMES_IN_3_S: usize = 125;
    /// Symphonia searches 1 MiB past a tag it can't read.
    const COVER_BYTES_PAST_PROBE_DEPTH: usize = 2 * 1024 * 1024;
    let library = tempfile::tempdir().unwrap();
    let bare = library.path().join("bare.mp3");
    let covered = library.path().join("covered.mp3");
    fs::write(&bare, silent_mp3_bytes(FRAMES_IN_3_S, None).unwrap()).unwrap();
    fs::write(
        &covered,
        silent_mp3_bytes(FRAMES_IN_3_S, Some(COVER_BYTES_PAST_PROBE_DEPTH)).unwrap(),
    )
    .unwrap();

    let bare = DiskLibraryFiles
        .inspect_audio(audio(&bare).unwrap())
        .await
        .unwrap();
    let covered = DiskLibraryFiles
        .inspect_audio(audio(&covered).unwrap())
        .await
        .unwrap();

    assert_eq!(covered.fingerprint, bare.fingerprint);
    assert_eq!(covered.duration, Duration::from_secs(3));
}

#[tokio::test]
async fn a_file_whose_codec_has_no_decoder_is_unsupported() {
    let library = tempfile::tempdir().unwrap();
    let adpcm = library.path().join("adpcm.wav");
    fs::write(&adpcm, ima_adpcm_wav_bytes()).unwrap();

    let error = DiskLibraryFiles
        .inspect_audio(audio(&adpcm).unwrap())
        .await
        .unwrap_err();

    assert!(
        matches!(
            &error,
            InspectAudioError::Unsupported(reason)
                if format!("{reason:#}").starts_with("no decoder reads its")
        ),
        "{error:?}"
    );
}

/// A WAV file of IMA ADPCM, which the container reader understands but no decoder here reads.
fn ima_adpcm_wav_bytes() -> Vec<u8> {
    const BLOCK_BYTES: u16 = 256;
    const FRAMES_PER_BLOCK: u16 = 505;
    let mut format = Vec::new();
    format.extend_from_slice(&0x0011_u16.to_le_bytes()); // IMA ADPCM
    format.extend_from_slice(&1_u16.to_le_bytes()); // mono
    format.extend_from_slice(&8_000_u32.to_le_bytes());
    format.extend_from_slice(&4_055_u32.to_le_bytes()); // bytes per second
    format.extend_from_slice(&BLOCK_BYTES.to_le_bytes());
    format.extend_from_slice(&4_u16.to_le_bytes()); // bits per sample
    format.extend_from_slice(&2_u16.to_le_bytes()); // extension size
    format.extend_from_slice(&FRAMES_PER_BLOCK.to_le_bytes());
    let data = vec![0_u8; usize::from(BLOCK_BYTES) * 4];
    let mut chunks = b"fmt ".to_vec();
    chunks.extend_from_slice(&chunk_length(format.len()));
    chunks.extend_from_slice(&format);
    chunks.extend_from_slice(b"data");
    chunks.extend_from_slice(&chunk_length(data.len()));
    chunks.extend_from_slice(&data);
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&chunk_length(chunks.len() + 4));
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(&chunks);
    bytes
}

/// Silent mono MPEG-1 Layer III frames of 24 ms at 48 kHz, optionally behind an ID3v2.3 tag
/// holding a cover picture of `cover_bytes`.
fn silent_mp3_bytes(frame_count: usize, cover_bytes: Option<usize>) -> anyhow::Result<Vec<u8>> {
    // 128 kb/s at 48 kHz makes every frame 384 bytes, with no padding.
    const FRAME_HEADER: [u8; 4] = [0xFF, 0xFB, 0x94, 0xC0];
    const FRAME_BYTES: usize = 384;
    let mut bytes = Vec::new();
    if let Some(cover_bytes) = cover_bytes {
        let mut picture = b"\0image/png\0\x03\0".to_vec();
        // Never 0xFF, so no stray frame sync hides inside the picture.
        picture.extend((0..=250).cycle().take(cover_bytes));
        let mut frame = b"APIC".to_vec();
        frame.extend_from_slice(&u32::try_from(picture.len())?.to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&picture);
        let tag_size = u32::try_from(frame.len())?;
        bytes.extend_from_slice(b"ID3\x03\0\0");
        for shift in [21, 14, 7, 0] {
            bytes.push(u8::try_from((tag_size >> shift) & 0x7F)?);
        }
        bytes.extend_from_slice(&frame);
    }
    for _ in 0..frame_count {
        bytes.extend_from_slice(&FRAME_HEADER);
        bytes.resize(bytes.len() + FRAME_BYTES - FRAME_HEADER.len(), 0);
    }
    Ok(bytes)
}

fn chunk_length(length: usize) -> [u8; 4] {
    u32::try_from(length).unwrap_or(u32::MAX).to_le_bytes()
}
