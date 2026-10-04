use std::path::Path;

use serde_json::{Value, json};
use tufo_infra::kenku::{PLAYER_STATE_KEY, write_file_origin_items};

pub(super) fn write_wav(path: &Path) -> anyhow::Result<Vec<u8>> {
    write_wav_frames(path, 8_000, 32)
}

pub(super) fn write_long_wav(path: &Path, period_frames: u32) -> anyhow::Result<()> {
    write_wav_frames(path, 240_000, period_frames)?;
    Ok(())
}

fn write_wav_frames(path: &Path, frame_count: u32, period_frames: u32) -> anyhow::Result<Vec<u8>> {
    const SAMPLE_RATE: u32 = 8_000;
    let data_bytes = frame_count * 2;
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    for frame in 0..frame_count {
        let sample: i16 = if frame % period_frames < period_frames / 2 {
            128
        } else {
            -128
        };
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, &wav)?;
    Ok(wav)
}

pub(super) fn write_kenku(
    folder: &Path,
    title: &str,
    track: Option<(&Path, &str)>,
) -> anyhow::Result<()> {
    let tracks = track.map_or_else(|| json!([]), |_| json!(["track"]));
    let track_records = track.map_or_else(
        || json!({}),
        |(path, title)| {
            let path = path
                .to_string_lossy()
                .replace('\\', "/")
                .replace(' ', "%20");
            json!({"track": {"id": "track", "title": title, "url": format!("file://{path}")}})
        },
    );
    let document = json!({
        "playlists": {
            "playlists": {
                "byId": {"playlist": {
                    "id": "playlist", "title": title, "background": "war", "tracks": tracks
                }},
                "allIds": ["playlist"]
            },
            "tracks": track_records
        },
        "soundboards": {
            "soundboards": {"byId": {}, "allIds": []},
            "sounds": {}
        }
    });
    let persisted = document
        .as_object()
        .into_iter()
        .flatten()
        .map(|(key, value)| (key.clone(), json!(value.to_string())))
        .collect::<serde_json::Map<_, _>>();
    write_file_origin_items(
        folder,
        &[(PLAYER_STATE_KEY, &Value::Object(persisted).to_string())],
    )
}
