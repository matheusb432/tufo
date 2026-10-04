use std::{
    io::{Seek as _, Write as _},
    process::{Output, Stdio},
};

use serde_json::{Value, json};
use tufo_client::server::{DiscordBotTokenStatus, DiscordConnectionStatus};
use tufo_infra::discord::DiscordCredentials;

use super::support::{Session, output};

const FAKE_TOKEN: &str = "TUF0039_FAKE_STDIN_TOKEN.NEVER_USE.THIS_IS_A_TEST";
const SECRET_MARKER: &[u8] = b"TUF0039_FAKE_STDIN_TOKEN";

async fn token_stdin(session: &Session, bytes: &[u8]) -> anyhow::Result<Output> {
    let mut input = tempfile::tempfile()?;
    input.write_all(bytes)?;
    input.rewind()?;
    let mut command = session.command(&["--json", "discord", "token", "--stdin"]);
    command.stdin(Stdio::from(input));
    output(command).await
}

fn assert_redacted(output: &Output) {
    for bytes in [&output.stdout, &output.stderr] {
        assert!(
            !bytes
                .windows(SECRET_MARKER.len())
                .any(|window| window == SECRET_MARKER),
            "CLI output disclosed the token input"
        );
    }
}

#[tokio::test]
async fn discord_token_stdin_is_stored_and_redacted_without_starting_discord() -> anyhow::Result<()>
{
    let mut session = Session::new()?;
    // Start without credentials, then save a fake token for the next start. Never restart it.
    session.start().await?;
    let client = session.client()?;
    let initial = client.server_status().await?;
    assert_eq!(initial.discord_bot_token, DiscordBotTokenStatus::Missing);
    assert_eq!(
        initial.discord_connection,
        DiscordConnectionStatus::NotConfigured
    );
    let stored = token_stdin(&session, format!("  {FAKE_TOKEN}\r\n").as_bytes()).await?;
    assert_redacted(&stored);
    assert_eq!(stored.status.code(), Some(0));
    assert_eq!(stored.stderr.as_slice(), b"");
    let record: Value = serde_json::from_slice(&stored.stdout)?;
    assert_eq!(
        record,
        json!({
            "action": "set", "resource": "discord_token", "id": "bot",
            "restart_required": true, "next_command": "tufo server restart"
        })
    );
    let credentials_path = session.path("discord.toml");
    let saved_bytes = std::fs::read(&credentials_path)?;
    assert_eq!(
        String::from_utf8(saved_bytes.clone())?.trim(),
        format!("bot_token = \"{FAKE_TOKEN}\"")
    );
    let credentials = DiscordCredentials::load(&credentials_path)?;
    assert!(!format!("{credentials:?}").contains("TUF0039_FAKE_STDIN_TOKEN"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        assert_eq!(
            std::fs::metadata(&credentials_path)?.permissions().mode() & 0o777,
            0o600
        );
    }
    let configured = client.server_status().await?;
    assert_eq!(configured.process_id, initial.process_id);
    assert_eq!(
        configured.discord_bot_token,
        DiscordBotTokenStatus::Configured
    );
    assert_eq!(
        configured.discord_connection,
        DiscordConnectionStatus::NotConfigured
    );
    let status = session.json(&["discord", "status"]).await?;
    assert_eq!(status["bot_token"], "configured");
    assert_eq!(status["connection"], "not_configured");
    for arguments in [
        ["--json", "discord", "status"].as_slice(),
        &["discord", "status"],
        &["--json", "server", "status"],
        &["server", "status"],
        &["--json", "doctor"],
        &["doctor"],
    ] {
        assert_redacted(&session.success(arguments).await?);
    }

    let mut invalid_utf8 = FAKE_TOKEN.as_bytes().to_vec();
    invalid_utf8.push(0xff);
    for input in [
        format!("{FAKE_TOKEN} {FAKE_TOKEN}").into_bytes(),
        format!("{FAKE_TOKEN}\n{FAKE_TOKEN}").into_bytes(),
        format!("{FAKE_TOKEN}\0").into_bytes(),
        invalid_utf8,
        FAKE_TOKEN.repeat(100).into_bytes(),
        b" \r\n\t ".to_vec(),
    ] {
        let rejected = token_stdin(&session, &input).await?;
        assert_redacted(&rejected);
        assert_eq!(rejected.status.code(), Some(1));
        assert_eq!(rejected.stdout.as_slice(), b"");
        assert_ne!(rejected.stderr.as_slice(), b"");
        assert_eq!(std::fs::read(&credentials_path)?, saved_bytes);
    }

    // A private directory at the destination forces a save failure after input validation.
    std::fs::remove_file(&credentials_path)?;
    std::fs::create_dir(&credentials_path)?;
    let failed_save = token_stdin(&session, FAKE_TOKEN.as_bytes()).await?;
    assert_redacted(&failed_save);
    assert_eq!(failed_save.status.code(), Some(1));
    assert_eq!(failed_save.stdout.as_slice(), b"");
    assert!(
        String::from_utf8(failed_save.stderr)?.contains("could not save the Discord bot token")
    );
    assert!(credentials_path.is_dir());
    let still_running = client.server_status().await?;
    assert_eq!(still_running.process_id, initial.process_id);
    assert_eq!(
        still_running.discord_connection,
        DiscordConnectionStatus::NotConfigured
    );
    assert!(client.server_is_serving().await?);
    Ok(())
}
