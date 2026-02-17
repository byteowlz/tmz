//! Background daemon for token refresh and conversation sync.
//!
//! The daemon runs two periodic tasks:
//! - **Token refresh**: headless Playwright every ~50 minutes
//! - **Conversation sync**: pull conversations + messages into `SQLite` cache
//!
//! State files:
//! - `$XDG_STATE_HOME/tmz/tmz.pid` - daemon PID
//! - `$XDG_STATE_HOME/tmz/tmz.log` - daemon log output

use crate::cache::{parse_conversation, parse_message, Cache};
use crate::teams::auth::AuthManager;
use crate::teams::client::TeamsClient;
use crate::CoreError;
use std::path::PathBuf;
use std::time::Duration;

/// Default interval between token refreshes (50 minutes).
/// Tokens typically expire after 60 minutes, so this provides a 10-minute buffer.
const TOKEN_REFRESH_INTERVAL: Duration = Duration::from_secs(50 * 60);

/// Default interval between sync runs (5 minutes).
const SYNC_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Number of top conversations to sync messages for.
const SYNC_TOP_CHATS: i64 = 30;

/// Number of messages per conversation to sync.
const SYNC_MESSAGES_PER_CHAT: i32 = 50;

// ─── PID management ──────────────────────────────────────────────────

/// Get the PID file path.
///
/// # Errors
///
/// Returns an error if the state directory cannot be determined.
pub fn pid_file_path() -> Result<PathBuf, CoreError> {
    let state_dir = crate::default_state_dir()
        .map_err(|e| CoreError::Path(format!("resolving state dir: {e}")))?;
    Ok(state_dir.join("tmz.pid"))
}

/// Get the log file path.
///
/// # Errors
///
/// Returns an error if the state directory cannot be determined.
pub fn log_file_path() -> Result<PathBuf, CoreError> {
    let state_dir = crate::default_state_dir()
        .map_err(|e| CoreError::Path(format!("resolving state dir: {e}")))?;
    Ok(state_dir.join("tmz.log"))
}

/// Read the daemon PID from the PID file. Returns `None` if no file or invalid.
///
/// # Errors
///
/// Returns an error if the state directory cannot be determined.
pub fn read_pid() -> Result<Option<u32>, CoreError> {
    let path = pid_file_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path).map_err(CoreError::Io)?;
    Ok(content.trim().parse().ok())
}

/// Write the current process PID to the PID file.
///
/// # Errors
///
/// Returns an error on I/O failure.
pub fn write_pid() -> Result<(), CoreError> {
    let path = pid_file_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(CoreError::Io)?;
    }
    std::fs::write(&path, std::process::id().to_string()).map_err(CoreError::Io)
}

/// Remove the PID file.
///
/// # Errors
///
/// Returns an error on I/O failure.
pub fn remove_pid() -> Result<(), CoreError> {
    let path = pid_file_path()?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(CoreError::Io)?;
    }
    Ok(())
}

/// Check if a process with the given PID exists.
fn process_exists(pid: u32) -> bool {
    // Use `kill -0` semantics via std::process::Command
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Send a signal to a process. Returns true if the signal was delivered.
fn send_signal(pid: u32, signal: &str) -> bool {
    std::process::Command::new("kill")
        .args([signal, &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Check if the daemon process is running.
///
/// # Errors
///
/// Returns an error if the state directory cannot be determined.
pub fn is_running() -> Result<bool, CoreError> {
    let Some(pid) = read_pid()? else {
        return Ok(false);
    };
    Ok(process_exists(pid))
}

/// Stop the running daemon by sending SIGTERM.
///
/// # Errors
///
/// Returns an error if no daemon is running or signal fails.
pub fn stop_daemon() -> Result<(), CoreError> {
    let Some(pid) = read_pid()? else {
        return Err(CoreError::Other("daemon is not running".to_string()));
    };

    if !send_signal(pid, "-TERM") {
        // Process doesn't exist, clean up stale PID file
        remove_pid()?;
        return Err(CoreError::Other(
            "daemon process not found (stale PID file cleaned up)".to_string(),
        ));
    }

    // Wait for process to exit
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(100));
        if !process_exists(pid) {
            remove_pid()?;
            return Ok(());
        }
    }

    // Force kill if still running
    send_signal(pid, "-KILL");
    std::thread::sleep(Duration::from_millis(200));
    remove_pid()?;
    Ok(())
}

// ─── Daemon loop ─────────────────────────────────────────────────────

/// Run the daemon loop (foreground). Call this after daemonizing.
///
/// # Errors
///
/// Returns an error if initialization fails.
pub async fn run_daemon() -> Result<(), CoreError> {
    write_pid()?;

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = shutdown_tx.send(true);
    });

    log::info!("daemon started (pid={})", std::process::id());

    let mut token_interval = tokio::time::interval(TOKEN_REFRESH_INTERVAL);
    let mut sync_interval = tokio::time::interval(SYNC_INTERVAL);

    // Consume the first immediate tick, then run initial tasks
    token_interval.tick().await;
    sync_interval.tick().await;
    do_token_refresh().await;
    do_sync().await;

    loop {
        tokio::select! {
            _ = token_interval.tick() => {
                do_token_refresh().await;
            }
            _ = sync_interval.tick() => {
                do_sync().await;
            }
            _ = shutdown_rx.changed() => {
                log::info!("shutdown signal received");
                break;
            }
        }
    }

    remove_pid()?;
    log::info!("daemon stopped");
    Ok(())
}

// ─── Periodic tasks ──────────────────────────────────────────────────

async fn do_token_refresh() {
    log::info!("refreshing tokens...");
    let auth = match AuthManager::new() {
        Ok(a) => a,
        Err(e) => {
            log::error!("failed to create auth manager: {e}");
            return;
        }
    };

    match auth.refresh_tokens().await {
        Ok(tokens) => {
            let remaining = tokens.expires_at - chrono::Utc::now().timestamp();
            log::info!("tokens refreshed (expires in {remaining}s)");
        }
        Err(e) => {
            log::error!("token refresh failed: {e}");
        }
    }
}

async fn do_sync() {
    log::info!("syncing conversations...");

    let client = match TeamsClient::new() {
        Ok(c) => c,
        Err(e) => {
            log::error!("failed to create client: {e}");
            return;
        }
    };

    let cache_dir: PathBuf = match crate::default_data_dir() {
        Ok(d) => d,
        Err(e) => {
            log::error!("failed to resolve data dir: {e}");
            return;
        }
    };

    let cache = match Cache::open(&cache_dir.join("cache.db")).await {
        Ok(c) => c,
        Err(e) => {
            log::error!("failed to open cache: {e}");
            return;
        }
    };

    // Fetch conversations
    let conversations: serde_json::Value = match client.list_chats().await {
        Ok(c) => c,
        Err(e) => {
            log::error!("failed to list conversations: {e}");
            return;
        }
    };

    let empty_arr = Vec::new();
    let convs = conversations.as_array().unwrap_or(&empty_arr);
    let mut synced_convs = 0;

    for conv_json in convs {
        let conv = parse_conversation(conv_json);
        if let Err(e) = cache.upsert_conversation(&conv).await {
            log::error!("failed to upsert conversation: {e}");
        } else {
            synced_convs += 1;
        }
    }

    log::info!("synced {synced_convs} conversations");

    // Fetch messages for top N recent conversations
    let top = match cache.list_conversations(SYNC_TOP_CHATS).await {
        Ok(c) => c,
        Err(e) => {
            log::error!("failed to list cached conversations: {e}");
            return;
        }
    };

    let mut synced_msgs = 0;
    for conv in &top {
        match client
            .get_chat_messages(&conv.id, Some(SYNC_MESSAGES_PER_CHAT))
            .await
        {
            Ok(data) => {
                let empty_msgs = Vec::new();
                let msgs = data.as_array().unwrap_or(&empty_msgs);
                for msg_json in msgs {
                    if let Some(msg) = parse_message(msg_json, &conv.id) {
                        if let Err(e) = cache.upsert_message(&msg).await {
                            log::error!("failed to upsert message: {e}");
                        } else {
                            synced_msgs += 1;
                        }
                    }
                }
            }
            Err(e) => {
                log::warn!("failed to sync messages for {}: {e}", conv.display_name);
            }
        }
    }

    log::info!("synced {synced_msgs} messages across {} chats", top.len());
}

// ─── Service file generators ─────────────────────────────────────────

/// Generate a launchd plist for macOS auto-start.
#[must_use]
pub fn launchd_plist(binary_path: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>de.byteowlz.tmz</string>
    <key>ProgramArguments</key>
    <array>
        <string>{binary_path}</string>
        <string>service</string>
        <string>run</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/tmz.out.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/tmz.err.log</string>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>"#
    )
}

/// Generate a systemd user unit file for Linux auto-start.
#[must_use]
pub fn systemd_unit(binary_path: &str) -> String {
    format!(
        r"[Unit]
Description=tmz - Microsoft Teams background daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={binary_path} service run
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
"
    )
}
