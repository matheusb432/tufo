use std::str::FromStr;

use anyhow::Context as _;
use clap::Subcommand;
use serde::Deserialize;
use tufo_client::desktop::{
    DesktopSettingsUpdate, Language, QuitBehavior, SettingsFile, autostart,
};

use crate::input::deserialize_present;

#[derive(Subcommand)]
pub(crate) enum DesktopCommand {
    /// Read local language, quit behavior, and autostart without contacting the server.
    Get,
    /// Update supplied desktop settings without contacting the server.
    #[command(after_long_help = r#"Examples:
  tufo desktop update '{"language":"pt-BR","keep_playing_after_quit":true}'
  tufo desktop update '{"language":"system","autostart":false}'

Omitted fields keep their current values. Languages: system, en, pt-BR.
Use "system" to restore system locale selection and false to disable a boolean setting.
Null values and unknown fields are errors.
The GUI loads desktop settings at startup; restart it to apply external changes.
Autostart registers tufo-gui --at-login; enabling it requires tufo-gui beside tufo or on PATH."#)]
    Update {
        /// One JSON object with `language`, `keep_playing_after_quit`, or `autostart`.
        #[arg(value_name = "JSON_PAYLOAD")]
        payload: DesktopUpdatePayload,
    },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DesktopUpdatePayload {
    settings: DesktopSettingsUpdate,
    autostart: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DesktopUpdateJson {
    #[serde(default, deserialize_with = "deserialize_present")]
    language: Option<Language>,
    #[serde(default, deserialize_with = "deserialize_present")]
    keep_playing_after_quit: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_present")]
    autostart: Option<bool>,
}

impl FromStr for DesktopUpdatePayload {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let document: DesktopUpdateJson = crate::input::parse_json(text)?;
        Ok(Self {
            settings: DesktopSettingsUpdate {
                language: document.language,
                quit_behavior: document.keep_playing_after_quit.map(|keep_playing| {
                    if keep_playing {
                        QuitBehavior::KeepPlaying
                    } else {
                        QuitBehavior::StandBy
                    }
                }),
            },
            autostart: document.autostart,
        })
    }
}

pub(crate) async fn run(command: DesktopCommand, json: bool) -> anyhow::Result<bool> {
    match command {
        DesktopCommand::Get => {
            let record = tokio::task::spawn_blocking(get).await??;
            crate::api::emit(&record, json).await?;
        }
        DesktopCommand::Update { payload } => {
            tokio::task::spawn_blocking(move || update(payload)).await??;
            crate::api::ack("update", "desktop", "local", json).await?;
        }
    }
    Ok(true)
}

fn settings_file() -> anyhow::Result<SettingsFile> {
    SettingsFile::from_environment()
        .context("this system names no config directory for Tufo's settings")
}

fn get() -> anyhow::Result<serde_json::Value> {
    let settings = settings_file()?.load()?;
    let autostart = autostart::read_enabled()?;
    Ok(serde_json::json!({
        "language": settings.language.config_name(),
        "keep_playing_after_quit": settings.quit_behavior == QuitBehavior::KeepPlaying,
        "autostart": autostart,
    }))
}

fn update(payload: DesktopUpdatePayload) -> anyhow::Result<()> {
    let file = settings_file()?;
    file.load()?;
    if let Some(enabled) = payload.autostart {
        autostart::set_enabled(enabled)?;
    }
    file.update(payload.settings)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::{Parser, error::ErrorKind};

    use super::*;

    #[derive(Parser)]
    struct DesktopCli {
        #[command(subcommand)]
        command: DesktopCommand,
    }

    #[test]
    fn positional_json_becomes_domain_settings() -> anyhow::Result<()> {
        let cli = DesktopCli::try_parse_from([
            "tufo",
            "update",
            r#"{"language":"pt-BR","keep_playing_after_quit":true,"autostart":false}"#,
        ])?;
        let DesktopCommand::Update { payload } = cli.command else {
            anyhow::bail!("desktop update selected a different command");
        };

        assert_eq!(payload.settings.language, Some(Language::PortugueseBrazil));
        assert_eq!(
            payload.settings.quit_behavior,
            Some(QuitBehavior::KeepPlaying)
        );
        assert_eq!(payload.autostart, Some(false));
        Ok(())
    }

    #[test]
    fn omitted_fields_and_explicit_defaults_are_distinct() -> anyhow::Result<()> {
        let omitted: DesktopUpdatePayload = "{}".parse().map_err(anyhow::Error::msg)?;
        let reset: DesktopUpdatePayload =
            r#"{"language":"system","keep_playing_after_quit":false}"#
                .parse()
                .map_err(anyhow::Error::msg)?;

        assert_eq!(omitted.settings, DesktopSettingsUpdate::default());
        assert_eq!(omitted.autostart, None);
        assert_eq!(reset.settings.language, Some(Language::System));
        assert_eq!(reset.settings.quit_behavior, Some(QuitBehavior::StandBy));
        assert_eq!(reset.autostart, None);
        Ok(())
    }

    #[test]
    fn invalid_objects_fail_clap_validation_before_execution() -> anyhow::Result<()> {
        for payload in [
            "[]",
            "null",
            "{} {}",
            r#"{"language":"pt-BRR"}"#,
            r#"{"language":null}"#,
            r#"{"keep_playing_after_quit":null}"#,
            r#"{"autostart":null}"#,
            r#"{"keep_playing_after_quit":"true"}"#,
            r#"{"autostart":1}"#,
            r#"{"unknown":true}"#,
            r#"{"autostart":{"enabled":true,"unknown":true}}"#,
            r#"{"language":{"code":"en","unknown":true}}"#,
        ] {
            let result = DesktopCli::try_parse_from(["tufo", "update", payload]);
            let Err(error) = result else {
                anyhow::bail!("invalid desktop payload was accepted: {payload}");
            };
            assert_eq!(error.kind(), ErrorKind::ValueValidation, "{payload}");
        }
        Ok(())
    }
}
