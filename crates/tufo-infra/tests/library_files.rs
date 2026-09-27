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

fn chunk_length(length: usize) -> [u8; 4] {
    u32::try_from(length).unwrap_or(u32::MAX).to_le_bytes()
}
