use tufo_infra::audio::sound_cache::{LoadedSound, load_sound};

mod support;

use support::{SAMPLE_RATE, constant, correlation, stereo_sine, write_ogg_opus, write_wav};

#[test]
fn short_sounds_decode_into_memory_and_long_ones_stream() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let door = directory.path().join("door.wav");
    let rain = directory.path().join("rain.wav");
    write_wav(&door, SAMPLE_RATE, &constant(0.5, 48_000))?;
    write_wav(&rain, 8_000, &constant(0.1, 8_000 * 21))?;

    let door = load_sound(&door)?;
    let rain = load_sound(&rain)?;

    assert!(matches!(door, LoadedSound::Decoded(data) if data.frames.len() == 48_000));
    assert!(matches!(rain, LoadedSound::Streamed(_)));
    Ok(())
}

#[test]
fn opus_sounds_decode_to_exactly_their_samples() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let chime = directory.path().join("chime.opus");
    // At 1 kHz, the 312-frame encoder delay left in would invert the waveform.
    let original = stereo_sine(48_000, 1_000.0, 0.5);
    write_ogg_opus(&chime, 2, &original)?;

    let LoadedSound::Decoded(data) = load_sound(&chime)? else {
        anyhow::bail!("a one-second sound streamed");
    };

    assert_eq!(data.frames.len(), 48_000);
    let decoded_left = data
        .frames
        .iter()
        .map(|frame| frame.left)
        .collect::<Vec<_>>();
    let original_left = original.iter().step_by(2).copied().collect::<Vec<_>>();
    let similarity = correlation(&decoded_left, &original_left);
    assert!(
        similarity > 0.95,
        "the decoded sine matches the original only {similarity}"
    );
    Ok(())
}
