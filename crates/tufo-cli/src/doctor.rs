use tufo_client::{
    ClientError, Code, LocalEndpoint,
    server::{DiscordBotTokenStatus, DiscordConnectionStatus, ServerStatus},
};

use crate::{
    render::{Console, single_line},
    server,
};

const LATEST_VERSION_ACTION: &str = concat!("Get the latest version at ", releases_url!());

#[derive(serde::Serialize)]
struct Report {
    cli_version: &'static str,
    endpoint: Option<String>,
    server: Option<ServerDetails>,
    checks: Vec<Check>,
}

#[derive(serde::Serialize)]
struct ServerDetails {
    version: Option<String>,
    process_id: Option<u32>,
    uptime_seconds: Option<u64>,
    serving: bool,
    supports_library_patches: Option<bool>,
}

#[derive(serde::Serialize)]
struct Check {
    name: &'static str,
    status: CheckStatus,
    detail: String,
    action: Option<&'static str>,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Pass,
    Warning,
    Fail,
}

impl Check {
    fn new(
        name: &'static str,
        status: CheckStatus,
        detail: impl Into<String>,
        action: Option<&'static str>,
    ) -> Self {
        Self {
            name,
            status,
            detail: detail.into(),
            action,
        }
    }
}

pub(crate) async fn run(
    console: Console,
    title: &str,
    json: bool,
) -> anyhow::Result<(String, bool)> {
    let report = inspect().await;
    let output = if json {
        serde_json::to_string_pretty(&report)?
    } else {
        report.render(console, title)
    };
    Ok((output, report.success()))
}

async fn inspect() -> Report {
    let mut report = Report {
        cli_version: env!("CARGO_PKG_VERSION"),
        endpoint: None,
        server: None,
        checks: Vec::new(),
    };
    let endpoint = match LocalEndpoint::from_environment() {
        Ok(endpoint) => endpoint,
        Err(error) => {
            report.checks.push(Check::new(
                "Endpoint",
                CheckStatus::Fail,
                error.to_string(),
                Some("Set TUFO_RUNTIME_DIR to a valid absolute path."),
            ));
            return report;
        }
    };
    report.endpoint = Some(endpoint.path().display().to_string());
    match server::inspect(&endpoint).await {
        Ok(Some(snapshot)) => {
            if snapshot.serving {
                report.checks.push(local_output_check(&endpoint).await);
            }
            report.record_server(snapshot);
        }
        Ok(None) => {
            report.checks.push(Check::new(
                "Server",
                CheckStatus::Fail,
                "Not running",
                Some("Run `tufo server start`."),
            ));
            report.checks.push(token_unknown());
        }
        Err(error) => {
            report.checks.push(Check::new("Server", CheckStatus::Fail, format!("Cannot inspect tufo-server: {error:#}"), Some("Check the runtime directory permissions and tufo-server.log in the server's state directory.")));
            report.checks.push(token_unknown());
        }
    }
    report
}

async fn local_output_check(endpoint: &LocalEndpoint) -> Check {
    use tufo_models::playback::local_output::LocalOutputState;
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let client = tufo_client::TufoClient::connect_lazy(endpoint)?;
        crate::api::player_state(&client).await
    })
    .await;
    let Ok(Ok(state)) = result else {
        return Check::new(
            "Local audio",
            CheckStatus::Warning,
            "Current output health is unavailable",
            None,
        );
    };
    let (status, action) = match state.local_output {
        LocalOutputState::Failed(_) => (
            CheckStatus::Fail,
            Some("Check the audio device, then run `tufo output recover`."),
        ),
        LocalOutputState::Recovering { .. } => (CheckStatus::Warning, None),
        LocalOutputState::Ready(_) | LocalOutputState::Released => (CheckStatus::Pass, None),
    };
    Check::new(
        "Local audio",
        status,
        crate::record::local_output(&state.local_output).to_string(),
        action,
    )
}

fn token_unknown() -> Check {
    Check::new(
        "Bot token",
        CheckStatus::Warning,
        "Unknown until server status is available",
        None,
    )
}

fn discord_checks(status: &ServerStatus) -> [Check; 2] {
    let token = match status.discord_bot_token {
        DiscordBotTokenStatus::Configured => Check::new(
            "Bot token",
            CheckStatus::Pass,
            "Configured in the server's private credentials file",
            None,
        ),
        DiscordBotTokenStatus::Missing => Check::new(
            "Bot token",
            CheckStatus::Warning,
            "Not configured; local playback is available",
            Some("Save a bot token in the GUI's Settings to enable Discord."),
        ),
        DiscordBotTokenStatus::Invalid => Check::new(
            "Bot token",
            CheckStatus::Fail,
            "The saved credentials cannot be read safely or contain an invalid token entry",
            Some(
                "Check the credentials file's permissions and save a valid token in the GUI's Settings.",
            ),
        ),
    };
    let connection = match status.discord_connection {
        DiscordConnectionStatus::Ready => Check::new(
            "Discord",
            CheckStatus::Pass,
            "Bot logged in; voice channel directory ready",
            None,
        ),
        DiscordConnectionStatus::Connecting => Check::new(
            "Discord",
            CheckStatus::Warning,
            "Bot login or voice channel discovery pending",
            Some("Check network access, the bot token, and tufo-server.log if this persists."),
        ),
        DiscordConnectionStatus::NotConfigured => Check::new(
            "Discord",
            CheckStatus::Warning,
            "Disabled in this server session",
            (status.discord_bot_token == DiscordBotTokenStatus::Configured)
                .then_some("Run `tufo server restart` to load the saved token."),
        ),
        DiscordConnectionStatus::Disabled => Check::new(
            "Discord",
            CheckStatus::Warning,
            "Connection disabled; the bot is not logged in",
            Some("Enable the Discord connection in the GUI's Settings to use voice output."),
        ),
    };
    [token, connection]
}

impl Report {
    fn record_server(&mut self, snapshot: server::Snapshot) {
        self.checks.push(Check::new(
            "Server",
            if snapshot.serving {
                CheckStatus::Pass
            } else {
                CheckStatus::Fail
            },
            if snapshot.serving {
                "Healthy and answering gRPC"
            } else {
                "Answering gRPC but not serving"
            },
            None,
        ));
        let mut details = ServerDetails {
            version: None,
            process_id: None,
            uptime_seconds: None,
            serving: snapshot.serving,
            supports_library_patches: None,
        };
        match snapshot.status {
            Ok(status) => {
                self.checks.push(Check::new(
                    "Versions",
                    if status.version == self.cli_version {
                        CheckStatus::Pass
                    } else {
                        CheckStatus::Fail
                    },
                    format!("CLI {} / server {}", self.cli_version, status.version),
                    (status.version != self.cli_version).then_some(LATEST_VERSION_ACTION),
                ));
                self.checks.extend(discord_checks(&status));
                self.checks.push(Check::new(
                    "Library API",
                    if status.supports_library_patches {
                        CheckStatus::Pass
                    } else {
                        CheckStatus::Warning
                    },
                    if status.supports_library_patches {
                        "Atomic partial library updates are supported"
                    } else {
                        "The running server cannot apply nested partial library updates"
                    },
                    (!status.supports_library_patches)
                        .then_some("Install matching binaries and run `tufo server restart`."),
                ));
                details.supports_library_patches = Some(status.supports_library_patches);
                details.version = Some(status.version);
                details.process_id = Some(status.process_id.get());
                details.uptime_seconds = Some(status.uptime.as_secs());
            }
            Err(ClientError::Status(status)) if status.code() == Code::Unimplemented => {
                self.checks.push(Check::new(
                    "Server API",
                    CheckStatus::Fail,
                    "The running server does not support status diagnostics",
                    Some(LATEST_VERSION_ACTION),
                ));
                self.checks.push(token_unknown());
            }
            Err(error) => {
                self.checks.push(Check::new(
                    "Server status",
                    CheckStatus::Fail,
                    error.to_string(),
                    Some("Run `tufo server restart`, then `tufo doctor`."),
                ));
                self.checks.push(token_unknown());
            }
        }
        self.server = Some(details);
    }

    fn success(&self) -> bool {
        self.checks
            .iter()
            .all(|check| check.status != CheckStatus::Fail)
    }

    fn render(&self, console: Console, title: &str) -> String {
        let mut lines = vec![console.heading(title)];
        if let Some(endpoint) = &self.endpoint {
            lines.push(console.row("INFO", "Endpoint", endpoint));
        }
        if let Some(server) = &self.server
            && let (Some(process_id), Some(seconds)) = (server.process_id, server.uptime_seconds)
        {
            lines.push(console.row(
                "INFO",
                "Process",
                &format!(
                    "{} / up {}h {:02}m {:02}s",
                    process_id,
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60
                ),
            ));
        }
        for check in &self.checks {
            let badge = match check.status {
                CheckStatus::Pass => "OK",
                CheckStatus::Warning => "WARN",
                CheckStatus::Fail => "FAIL",
            };
            lines.push(console.row(badge, check.name, &check.detail));
            if let Some(action) = check.action {
                lines.push(format!("                        {}", single_line(action)));
            }
        }
        lines.join("\n")
    }
}
