use kira::Frame;
use tufo_infra::audio::{
    Mixer, MixerVolumes,
    effect_sound::{EffectSource, effect_sound},
    memory::{AudioMemory, PreparationCancellation},
    packed_frame::PackedFrame,
    sound_cache::{
        LoadedSound, SoundCache, SoundKey, SoundLoadOptions, load_sound, load_sound_with_memory,
    },
};

#[test]
fn cache_pressure_keeps_playing_allocations_until_their_last_owner_releases() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("ambience.wav");
    write_wav(&path, 8_000, &constant(0.25, 16_000))?;
    let memory = AudioMemory::new(128 * 1024);
    let LoadedSound::Decoded(sound) = load_sound_with_memory(
        &path,
        LOAD_OPTIONS,
        &memory,
        &PreparationCancellation::default(),
    )?
    else {
        anyhow::bail!("the sound fits the memory budget");
    };
    let playing = sound.clone();
    let key = SoundKey::read(&path)?;
    let mut cache = SoundCache::new(128 * 1024, std::time::Duration::from_secs(60));
    cache.insert(key.clone(), sound, std::time::Instant::now());
    cache.evict_unused_for(128 * 1024, &memory);
    assert!(cache.get(&key, std::time::Instant::now()).is_some());
    assert!(memory.snapshot().bytes_used > 0);
    drop(playing);
    cache.evict_unused_for(128 * 1024, &memory);
    assert!(cache.get(&key, std::time::Instant::now()).is_none());
    assert_eq!(memory.snapshot().bytes_used, 0);
    Ok(())
}
use tufo_models::library::SoundSettings;

mod support;

use support::{
    SAMPLE_RATE, constant, correlation, render, stereo_sine, wav_bytes, write_ogg_opus, write_wav,
};

const LOAD_OPTIONS: SoundLoadOptions = SoundLoadOptions {
    looping: false,
    bytes_max: 64 * 1024 * 1024,
};

#[test]
fn short_sounds_decode_into_memory_and_long_ones_stream() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let door = directory.path().join("door.wav");
    let rain = directory.path().join("rain.wav");
    write_wav(&door, SAMPLE_RATE, &constant(0.5, 48_000))?;
    write_wav(&rain, 8_000, &constant(0.1, 8_000 * 21))?;

    let door = load_sound(&door, LOAD_OPTIONS)?;
    let rain = load_sound(&rain, LOAD_OPTIONS)?;

    assert!(matches!(door, LoadedSound::Decoded(sound) if sound.frames().len() == 48_000));
    assert!(matches!(rain, LoadedSound::Streamed(_)));
    Ok(())
}

#[test]
fn long_loops_decode_only_when_their_frames_fit_the_byte_budget() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("ambience.wav");
    let frames = 8_000 * 21;
    write_wav(&path, 8_000, &constant(0.25, frames))?;
    let options = SoundLoadOptions {
        looping: true,
        bytes_max: frames * size_of::<PackedFrame>(),
    };

    let LoadedSound::Decoded(sound) = load_sound(&path, options)? else {
        anyhow::bail!("the loop fits exactly in the byte budget");
    };

    assert_eq!(sound.frames().len(), frames);
    assert!(
        sound
            .frames()
            .iter()
            .all(|&frame| Frame::from(frame) == Frame::new(0.25, 0.25))
    );
    assert!(matches!(
        load_sound(
            &path,
            SoundLoadOptions {
                bytes_max: options.bytes_max - 1,
                ..options
            }
        )?,
        LoadedSound::Streamed(_)
    ));
    Ok(())
}

#[test]
fn a_cached_loop_keeps_its_pitch_and_seam_at_another_output_rate() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("ambience.wav");
    std::fs::write(&path, wav_bytes(21, 440.0, None))?;
    let LoadedSound::Decoded(sound) = load_sound(
        &path,
        SoundLoadOptions {
            looping: true,
            ..LOAD_OPTIONS
        },
    )?
    else {
        anyhow::bail!("the loop fits in the byte budget");
    };
    std::fs::remove_file(&path)?;
    let (mut mixer, renderer) = Mixer::open_pulled(SAMPLE_RATE, MixerVolumes::default())?;
    let mut renderer = renderer.recv()?;
    let looping = SoundSettings {
        looping: true,
        ..SoundSettings::default()
    };
    let (sound, _handle) = effect_sound(EffectSource::Decoded(sound), looping, SAMPLE_RATE);
    mixer.play_effect(sound)?;
    let loop_frame = 21 * SAMPLE_RATE as usize;
    let window_frames = SAMPLE_RATE as usize / 10;

    let output = render(&mut renderer, loop_frame + window_frames / 2);

    let seam = &output[loop_frame - window_frames / 2..];
    let expected = stereo_sine(window_frames, 440.0, 0.25)
        .into_iter()
        .step_by(2)
        .collect::<Vec<_>>();
    let similarity = correlation(seam, &expected);
    assert!(
        similarity > 0.999,
        "the loop seam changed the tone: {similarity}"
    );
    Ok(())
}

#[test]
fn opus_sounds_decode_to_exactly_their_samples() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let chime = directory.path().join("chime.opus");
    // At 1 kHz, the 312-frame encoder delay left in would invert the waveform.
    let original = stereo_sine(48_000, 1_000.0, 0.5);
    write_ogg_opus(&chime, 2, &original)?;

    let LoadedSound::Decoded(sound) = load_sound(&chime, LOAD_OPTIONS)? else {
        anyhow::bail!("a one-second sound streamed");
    };

    assert_eq!(sound.frames().len(), 48_000);
    let decoded_left = sound
        .frames()
        .iter()
        .map(|&frame| Frame::from(frame).left)
        .collect::<Vec<_>>();
    let original_left = original.iter().step_by(2).copied().collect::<Vec<_>>();
    let similarity = correlation(&decoded_left, &original_left);
    assert!(
        similarity > 0.95,
        "the decoded sine matches the original only {similarity}"
    );
    Ok(())
}
