use std::str::FromStr;

use anyhow::Context as _;
use clap::Subcommand;
use serde::Deserialize;
use tufo_client::desktop::{
    DesktopSettingsUpdate, Language, QuitBehavior, SettingsFile, SidebarPage, SidebarPages,
    SidebarPagesPatch, TourVisibility, autostart,
};

use crate::input::deserialize_present;

#[derive(Subcommand)]
pub(crate) enum DesktopCommand {
    /// Read local language, quit behavior, tours, hidden sidebar pages, and autostart without
    /// contacting the server.
    Get,
    /// Update supplied desktop settings without contacting the server.
    #[command(after_long_help = r#"Examples:
  tufo desktop update '{"language":"pt-BR","keep_playing_after_quit":true}'
  tufo desktop update '{"language":"system","autostart":false}'
  tufo desktop update '{"tours":"always-hide"}'
  tufo desktop update '{"sidebar_hidden_pages":["songs","browser"]}'

Omitted fields keep their current values. Languages: system, en, pt-BR.
Tours: always-show, show-unseen (default), always-hide.
sidebar_hidden_pages replaces the hidden set; pages: home, songs, browser, playlists, soundboards.
Use "system" to restore system locale selection, false to disable a boolean setting, and []
to show every sidebar page.
Null values and unknown fields are errors.
The GUI loads desktop settings at startup; restart it to apply external changes.
Autostart registers tufo-gui --at-login; enabling it requires tufo-gui beside tufo or on PATH."#)]
    Update {
        /// One JSON object with `language`, `keep_playing_after_quit`, `tours`,
        /// `sidebar_hidden_pages`, or `autostart`.
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
    tours: Option<TourVisibility>,
    #[serde(default, deserialize_with = "deserialize_present")]
    keep_playing_after_quit: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_present")]
    sidebar_hidden_pages: Option<SidebarPages>,
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
                tours: document.tours,
                quit_behavior: document.keep_playing_after_quit.map(|keep_playing| {
                    if keep_playing {
                        QuitBehavior::KeepPlaying
                    } else {
                        QuitBehavior::StandBy
                    }
                }),
                sidebar_hidden_pages: document
                    .sidebar_hidden_pages
                    .map(SidebarPagesPatch::replace)
                    .unwrap_or_default(),
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
        .context("this system names no config directory for the desktop settings")
}

fn get() -> anyhow::Result<serde_json::Value> {
    let settings = settings_file()?.load()?;
    let autostart = autostart::read_enabled()?;
    Ok(serde_json::json!({
        "language": settings.language.config_name(),
        "tours": settings.tours.config_name(),
        "keep_playing_after_quit": settings.quit_behavior == QuitBehavior::KeepPlaying,
        "sidebar_hidden_pages": settings
            .sidebar_hidden_pages
            .iter()
            .map(SidebarPage::config_name)
            .collect::<Vec<_>>(),
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
            r#"{"language":"pt-BR","keep_playing_after_quit":true,"tours":"always-hide","sidebar_hidden_pages":["browser"],"autostart":false}"#,
        ])?;
        let DesktopCommand::Update { payload } = cli.command else {
            anyhow::bail!("desktop update selected a different command");
        };

        assert_eq!(payload.settings.language, Some(Language::PortugueseBrazil));
        assert_eq!(payload.settings.tours, Some(TourVisibility::AlwaysHide));
        assert_eq!(
            payload.settings.quit_behavior,
            Some(QuitBehavior::KeepPlaying)
        );
        assert_eq!(
            payload.settings.sidebar_hidden_pages,
            SidebarPagesPatch::replace(SidebarPages::default().with(SidebarPage::Browser, true))
        );
        assert_eq!(payload.autostart, Some(false));
        Ok(())
    }

    #[test]
    fn omitted_fields_and_explicit_defaults_are_distinct() -> anyhow::Result<()> {
        let omitted: DesktopUpdatePayload = "{}".parse().map_err(anyhow::Error::msg)?;
        let reset: DesktopUpdatePayload = r#"{"language":"system","keep_playing_after_quit":false,"tours":"show-unseen","sidebar_hidden_pages":[]}"#
            .parse()
            .map_err(anyhow::Error::msg)?;

        assert_eq!(omitted.settings, DesktopSettingsUpdate::default());
        assert_eq!(omitted.autostart, None);
        assert_eq!(reset.settings.language, Some(Language::System));
        assert_eq!(reset.settings.tours, Some(TourVisibility::ShowUnseen));
        assert_eq!(reset.settings.quit_behavior, Some(QuitBehavior::StandBy));
        assert_eq!(
            reset.settings.sidebar_hidden_pages,
            SidebarPagesPatch::replace(SidebarPages::default())
        );
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
            r#"{"tours":null}"#,
            r#"{"tours":"sometimes"}"#,
            r#"{"keep_playing_after_quit":null}"#,
            r#"{"autostart":null}"#,
            r#"{"keep_playing_after_quit":"true"}"#,
            r#"{"autostart":1}"#,
            r#"{"sidebar_hidden_pages":null}"#,
            r#"{"sidebar_hidden_pages":"songs"}"#,
            r#"{"sidebar_hidden_pages":["search"]}"#,
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
