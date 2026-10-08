use std::{str::FromStr, time::Duration};

use clap::Subcommand;
use serde::Deserialize;
use tufo_client::{
    TufoClient,
    player::{UpdateMusicLayer, UpdatePlayerSettings},
};
use tufo_models::{
    library::Volume,
    playback::{player_state::CrossfadeDuration, queue::RepeatMode},
};

use crate::input::deserialize_present;

#[derive(Debug, Subcommand)]
pub(crate) enum SettingsCommand {
    /// Read music/effects/browser volume, mutes, shuffle, repeat, and crossfade.
    Get,
    /// Update the supplied settings; omitted fields keep their current values.
    #[command(after_long_help = r#"Examples:
  tufo player settings update '{"volume":0.7,"effects_volume":0.5,"muted":false}'
  tufo music settings update '{"shuffle":true,"repeat":"playlist","crossfade_ms":2000}'
  tufo player settings update '{"browser_volume":0.4,"browser_muted":false}'

Volume, effects_volume, and browser_volume are linear gains from 0 to 1.
Repeat accepts off, playlist, or track. Crossfade is 0 to 10000 milliseconds.
Omit fields to preserve them. Use 0, false, or repeat:off to reset a setting."#)]
    Update {
        /// One JSON object with the settings to change.
        #[arg(value_name = "JSON_PAYLOAD")]
        payload: SettingsPayload,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "SettingsJson")]
pub(crate) struct SettingsPayload(UpdatePlayerSettings);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsJson {
    #[serde(default, deserialize_with = "deserialize_present")]
    volume: Option<f32>,
    #[serde(default, deserialize_with = "deserialize_present")]
    effects_volume: Option<f32>,
    #[serde(default, deserialize_with = "deserialize_present")]
    muted: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_present")]
    shuffle: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_present")]
    repeat: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present")]
    crossfade_ms: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_present")]
    browser_volume: Option<f32>,
    #[serde(default, deserialize_with = "deserialize_present")]
    browser_muted: Option<bool>,
}

impl TryFrom<SettingsJson> for SettingsPayload {
    type Error = String;

    fn try_from(json: SettingsJson) -> Result<Self, Self::Error> {
        let volume = json
            .volume
            .map(Volume::try_new)
            .transpose()
            .map_err(|error| format!("invalid volume: {error}"))?;
        let effects_volume = json
            .effects_volume
            .map(Volume::try_new)
            .transpose()
            .map_err(|error| format!("invalid effects_volume: {error}"))?;
        let browser_volume = json
            .browser_volume
            .map(Volume::try_new)
            .transpose()
            .map_err(|error| format!("invalid browser_volume: {error}"))?;
        let crossfade = json
            .crossfade_ms
            .map(|milliseconds| CrossfadeDuration::try_new(Duration::from_millis(milliseconds)))
            .transpose()
            .map_err(|error| format!("invalid crossfade_ms: {error}"))?;
        Ok(Self(UpdatePlayerSettings {
            volume,
            effects_volume,
            muted: json.muted,
            shuffle: json.shuffle,
            repeat: json.repeat.as_deref().map(parse_repeat).transpose()?,
            crossfade,
            browser_volume,
            browser_muted: json.browser_muted,
        }))
    }
}

impl FromStr for SettingsPayload {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        crate::input::parse_json(text)
    }
}

fn parse_repeat(text: &str) -> Result<RepeatMode, String> {
    match text {
        "off" => Ok(RepeatMode::Off),
        "playlist" => Ok(RepeatMode::Playlist),
        "track" => Ok(RepeatMode::Track),
        _ => Err("repeat must be off, playlist, or track".to_owned()),
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "LayerUpdateJson")]
pub(crate) struct LayerUpdatePayload(UpdateMusicLayer);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerUpdateJson {
    #[serde(default, deserialize_with = "deserialize_present")]
    volume: Option<f32>,
    #[serde(default, deserialize_with = "deserialize_present")]
    looping: Option<bool>,
}

impl TryFrom<LayerUpdateJson> for LayerUpdatePayload {
    type Error = String;

    fn try_from(json: LayerUpdateJson) -> Result<Self, Self::Error> {
        let volume = json
            .volume
            .map(Volume::try_new)
            .transpose()
            .map_err(|error| format!("invalid volume: {error}"))?;
        Ok(Self(UpdateMusicLayer {
            volume,
            looping: json.looping,
        }))
    }
}

impl FromStr for LayerUpdatePayload {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        crate::input::parse_json(text)
    }
}

impl LayerUpdatePayload {
    pub(super) fn into_inner(self) -> UpdateMusicLayer {
        self.0
    }
}

pub(super) async fn run(
    client: &TufoClient,
    command: SettingsCommand,
    json: bool,
) -> anyhow::Result<()> {
    match command {
        SettingsCommand::Get => {
            let state = crate::api::player_state(client).await?;
            crate::api::emit(&crate::record::player_settings(&state), json).await?;
        }
        SettingsCommand::Update { payload } => {
            client.update_player_settings(&payload.0).await?;
            crate::api::ack("update", "player_settings", "current", json).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_settings_preserve_values_and_explicit_resets_survive_parsing() -> Result<(), String>
    {
        let SettingsPayload(update) =
            r#"{"muted":false,"repeat":"off","crossfade_ms":0}"#.parse()?;

        assert_eq!(update.volume, None);
        assert_eq!(update.effects_volume, None);
        assert_eq!(update.shuffle, None);
        assert_eq!(update.muted, Some(false));
        assert_eq!(update.repeat, Some(RepeatMode::Off));
        assert_eq!(
            update.crossfade.map(CrossfadeDuration::into_inner),
            Some(Duration::ZERO)
        );
        Ok(())
    }

    #[test]
    fn settings_reject_invalid_shapes_unknown_fields_and_domain_values() {
        for text in [
            "[]",
            "null",
            r#"{"volume":0.5,"unknown":true}"#,
            r#"{"volume":1.1}"#,
            r#"{"effects_volume":-0.1}"#,
            r#"{"repeat":"forever"}"#,
            r#"{"repeat":{"off":null}}"#,
            r#"{"crossfade_ms":10001}"#,
            r#"{"crossfade_ms":-1}"#,
            r#"{"muted":null}"#,
            r#"{"effects_volume":null}"#,
            r#"{"volume":{"value":0.5,"unknown":true}}"#,
        ] {
            assert!(text.parse::<SettingsPayload>().is_err(), "accepted {text}");
        }
    }

    #[test]
    fn layer_updates_preserve_omissions_and_accept_false_and_zero() -> Result<(), String> {
        let update = r#"{"looping":false}"#.parse::<LayerUpdatePayload>()?.into_inner();
        assert_eq!(update.volume, None);
        assert_eq!(update.looping, Some(false));

        let update = r#"{"volume":0}"#.parse::<LayerUpdatePayload>()?.into_inner();
        assert_eq!(update.volume.map(Volume::into_inner), Some(0.0));
        assert_eq!(update.looping, None);
        assert!(r#"{"looping":true,"unknown":0}"#.parse::<LayerUpdatePayload>().is_err());
        assert!(r#"{"volume":2}"#.parse::<LayerUpdatePayload>().is_err());
        assert!(r#"{"looping":null}"#.parse::<LayerUpdatePayload>().is_err());
        Ok(())
    }
}
