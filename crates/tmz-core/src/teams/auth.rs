//! Browser-based authentication for Microsoft Teams.
//!
//! This module provides authentication by:
//! 1. Launching a Playwright-driven browser for user to login
//! 2. Automatically extracting MSAL access tokens from localStorage
//! 3. Storing tokens to `$XDG_STATE_HOME/tmz/tokens.json`
//!
//! After the first interactive login, SSO cookies are cached in a persistent
//! browser profile. Subsequent token refreshes run headlessly - no user
//! interaction required until the SSO session itself expires.

use crate::teams::models::TeamsTokens;
use crate::teams::storage::TokenStorage;
use crate::CoreError;
use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// Errors that can occur during authentication.
#[derive(Debug, Error)]
pub enum AuthenticationError {
    /// Token extraction failed.
    #[error("token extraction failed: {0}")]
    TokenExtractionError(String),

    /// Token storage error.
    #[error("token storage error: {0}")]
    StorageError(#[from] CoreError),

    /// JWT parsing error.
    #[error("JWT parsing error: {0}")]
    JwtError(String),
}

/// Token data structure from MSAL localStorage.
#[derive(Debug, Clone, Deserialize)]
struct MsalToken {
    secret: String,
}

/// Handles Teams authentication and token management.
#[derive(Debug)]
pub struct AuthManager {
    storage: TokenStorage,
}

/// How far before expiry to trigger a refresh (5 minutes).
const REFRESH_BUFFER_SECS: i64 = 300;

/// Timeout for headless refresh (seconds). SSO with cached cookies
/// completes in a few seconds; if it takes longer, the session is stale.
const HEADLESS_TIMEOUT_SECS: u64 = 30;

impl AuthManager {
    /// Teams web client URL.
    pub const TEAMS_URL: &str = "https://teams.microsoft.com/v2";
    /// Client ID for Teams web application.
    pub const TEAMS_CLIENT_ID: &str = "5e3ce6c0-2b1f-4285-8d4b-75ee78787346";

    /// Create a new authentication manager.
    ///
    /// # Errors
    ///
    /// Returns an error if the state directory cannot be determined.
    pub fn new() -> Result<Self, AuthenticationError> {
        Ok(Self {
            storage: TokenStorage::new().map_err(AuthenticationError::StorageError)?,
        })
    }

    /// Check if we have valid cached tokens.
    ///
    /// # Errors
    ///
    /// Returns an error if storage access fails.
    pub fn is_authenticated(&self) -> Result<bool, AuthenticationError> {
        Ok(self.storage.has_valid_tokens()?)
    }

    /// Run the Playwright-based browser login flow.
    ///
    /// # Arguments
    ///
    /// * `timeout_secs` - Maximum time to wait for login completion.
    /// * `headless` - If true, run without a visible browser window.
    ///
    /// # Errors
    ///
    /// Returns an error if the script is not found, fails to execute,
    /// or tokens cannot be extracted.
    pub async fn browser_login(
        &self,
        timeout_secs: Option<u64>,
        headless: bool,
    ) -> Result<TeamsTokens, AuthenticationError> {
        let script_path = find_auth_script()?;

        let mut cmd = tokio::process::Command::new("node");
        cmd.arg(&script_path);

        if let Some(t) = timeout_secs {
            cmd.arg("--timeout").arg(t.to_string());
        }

        if headless {
            cmd.arg("--headless");
        }

        // stdout = token JSON, stderr = progress/log messages
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(if headless {
            std::process::Stdio::null()
        } else {
            std::process::Stdio::inherit()
        });

        log::debug!("running auth script: {} (headless={headless})", script_path.display());

        let output = cmd.output().await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AuthenticationError::TokenExtractionError(
                    "node not found - install Node.js to use browser login".to_string(),
                )
            } else {
                AuthenticationError::TokenExtractionError(format!(
                    "failed to run auth script: {e}"
                ))
            }
        })?;

        if !output.status.success() {
            return Err(AuthenticationError::TokenExtractionError(
                if headless {
                    "headless token refresh failed - SSO session may have expired. Run 'tmz auth login' to re-authenticate.".to_string()
                } else {
                    "browser login failed or timed out - check stderr for details".to_string()
                },
            ));
        }

        let stdout = String::from_utf8(output.stdout).map_err(|e| {
            AuthenticationError::TokenExtractionError(format!("invalid UTF-8 output: {e}"))
        })?;

        let local_storage: std::collections::HashMap<String, String> =
            serde_json::from_str(&stdout).map_err(|e| {
                AuthenticationError::TokenExtractionError(format!(
                    "parsing token output: {e}"
                ))
            })?;

        self.store_tokens_from_browser(&local_storage)
    }

    /// Silently refresh tokens using cached SSO cookies.
    ///
    /// Runs the browser headlessly with a short timeout. If the SSO session
    /// is still valid, fresh tokens are extracted without user interaction.
    ///
    /// # Errors
    ///
    /// Returns an error if headless refresh fails (SSO session expired).
    pub async fn refresh_tokens(&self) -> Result<TeamsTokens, AuthenticationError> {
        log::debug!("attempting headless token refresh");
        self.browser_login(Some(HEADLESS_TIMEOUT_SECS), true).await
    }

    /// Get valid tokens, auto-refreshing if expired or about to expire.
    ///
    /// Resolution order:
    /// 1. Return cached tokens if still valid (with buffer)
    /// 2. Attempt headless refresh via cached SSO cookies
    /// 3. Fail with a message to run `tmz auth login`
    ///
    /// # Errors
    ///
    /// Returns an error if no valid tokens are available and refresh fails.
    pub async fn get_tokens_or_refresh(&self) -> Result<TeamsTokens, AuthenticationError> {
        match self.storage.load_tokens() {
            Ok(tokens) => {
                let now = now_epoch();
                if tokens.expires_at > now + REFRESH_BUFFER_SECS {
                    return Ok(tokens);
                }
                // Tokens expired or expiring soon - try headless refresh
                log::info!("tokens expired or expiring soon, refreshing...");
                match self.refresh_tokens().await {
                    Ok(fresh) => Ok(fresh),
                    Err(_) => {
                        // If tokens haven't fully expired yet, use them anyway
                        if tokens.expires_at > now {
                            log::warn!("headless refresh failed but tokens still valid for {}s", tokens.expires_at - now);
                            Ok(tokens)
                        } else {
                            Err(AuthenticationError::TokenExtractionError(
                                "tokens expired and headless refresh failed. Run 'tmz auth login'.".to_string(),
                            ))
                        }
                    }
                }
            }
            Err(CoreError::SecretNotFound(_)) => {
                Err(AuthenticationError::TokenExtractionError(
                    "not authenticated. Run 'tmz auth login' first.".to_string(),
                ))
            }
            Err(e) => Err(AuthenticationError::StorageError(e)),
        }
    }

    /// Get cached tokens without auto-refresh. Returns error if expired.
    ///
    /// # Errors
    ///
    /// Returns an error if tokens are not available or expired.
    pub fn get_tokens(&self) -> Result<TeamsTokens, AuthenticationError> {
        let tokens = self.storage.load_tokens()?;
        let now = now_epoch();

        if tokens.expires_at < now {
            return Err(AuthenticationError::TokenExtractionError(
                "tokens expired. Run 'tmz auth login' or any command to auto-refresh.".to_string(),
            ));
        }

        Ok(tokens)
    }

    /// Store tokens extracted from browser localStorage.
    ///
    /// # Errors
    ///
    /// Returns an error if tokens cannot be parsed or stored.
    pub fn store_tokens_from_browser(
        &self,
        local_storage: &std::collections::HashMap<String, String>,
    ) -> Result<TeamsTokens, AuthenticationError> {
        let skype_token_json = Self::find_token(local_storage, "api.spaces.skype.com")?;
        let chat_token_json = Self::find_token(local_storage, "chatsvcagg.teams.microsoft.com")?;
        let graph_token_json = Self::find_token(local_storage, "graph.microsoft.com")?;
        let presence_token_json = Self::find_token(local_storage, "presence.teams.microsoft.com")?;

        let skype_token: MsalToken = serde_json::from_str(&skype_token_json)
            .map_err(|e| AuthenticationError::TokenExtractionError(format!("parsing skype token: {e}")))?;
        let chat_token: MsalToken = serde_json::from_str(&chat_token_json)
            .map_err(|e| AuthenticationError::TokenExtractionError(format!("parsing chat token: {e}")))?;
        let graph_token: MsalToken = serde_json::from_str(&graph_token_json)
            .map_err(|e| AuthenticationError::TokenExtractionError(format!("parsing graph token: {e}")))?;
        let presence_token: MsalToken = serde_json::from_str(&presence_token_json)
            .map_err(|e| AuthenticationError::TokenExtractionError(format!("parsing presence token: {e}")))?;

        let (tenant_id, user_id, upn, expires_at) = parse_token_claims(&skype_token.secret)?;

        let tokens = TeamsTokens {
            skype_token: skype_token.secret,
            chat_token: chat_token.secret,
            graph_token: graph_token.secret,
            presence_token: presence_token.secret,
            tenant_id,
            user_id,
            user_principal_name: upn,
            expires_at,
        };

        self.storage.store_tokens(&tokens)?;
        Ok(tokens)
    }

    /// Store tokens directly from manual extraction.
    ///
    /// # Errors
    ///
    /// Returns an error if tokens cannot be parsed or stored.
    pub fn store_tokens(
        &self,
        skype_token: &str,
        chat_token: &str,
        graph_token: &str,
        presence_token: &str,
    ) -> Result<TeamsTokens, AuthenticationError> {
        let (tenant_id, user_id, upn, expires_at) = parse_token_claims(skype_token)?;

        let tokens = TeamsTokens {
            skype_token: skype_token.to_string(),
            chat_token: chat_token.to_string(),
            graph_token: graph_token.to_string(),
            presence_token: presence_token.to_string(),
            tenant_id,
            user_id,
            user_principal_name: upn,
            expires_at,
        };

        self.storage.store_tokens(&tokens)?;
        Ok(tokens)
    }

    /// Logout - clear stored tokens.
    ///
    /// # Errors
    ///
    /// Returns an error if storage access fails.
    pub fn logout(&self) -> Result<(), AuthenticationError> {
        self.storage.clear_tokens()?;
        Ok(())
    }

    fn find_token(
        local_storage: &std::collections::HashMap<String, String>,
        resource: &str,
    ) -> Result<String, AuthenticationError> {
        local_storage
            .iter()
            .find(|(k, _)| {
                k.contains("accesstoken")
                    && k.contains("login.windows.net")
                    && k.to_lowercase().contains(&resource.to_lowercase())
            })
            .map(|(_, v)| v.clone())
            .ok_or_else(|| {
                AuthenticationError::TokenExtractionError(format!(
                    "no token found for resource: {resource}"
                ))
            })
    }
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn parse_token_claims(token: &str) -> Result<(String, String, String, i64), AuthenticationError> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(AuthenticationError::JwtError(
            "invalid JWT format - expected 3 parts".to_string(),
        ));
    }

    let payload = base64_decode(parts[1])
        .map_err(|e| AuthenticationError::JwtError(format!("base64 decode: {e}")))?;

    let claims: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|e| AuthenticationError::JwtError(format!("parsing claims: {e}")))?;

    let tenant_id = claims["tid"]
        .as_str()
        .ok_or_else(|| AuthenticationError::JwtError("missing tid claim".to_string()))?
        .to_string();

    let user_id = claims["oid"]
        .as_str()
        .ok_or_else(|| AuthenticationError::JwtError("missing oid claim".to_string()))?
        .to_string();

    let upn = claims["upn"]
        .as_str()
        .unwrap_or_else(|| claims["unique_name"].as_str().unwrap_or("unknown"))
        .to_string();

    let exp = claims["exp"]
        .as_i64()
        .ok_or_else(|| AuthenticationError::JwtError("missing exp claim".to_string()))?;

    Ok((tenant_id, user_id, upn, exp))
}

/// Locate the `teams-auth.mjs` script.
///
/// Search order:
/// 1. `$TMZ_AUTH_SCRIPT` environment variable
/// 2. Next to the current executable (`../scripts/teams-auth.mjs`)
/// 3. Relative to the workspace root (for development)
fn find_auth_script() -> Result<std::path::PathBuf, AuthenticationError> {
    if let Ok(p) = std::env::var("TMZ_AUTH_SCRIPT") {
        let path = std::path::PathBuf::from(p);
        if path.exists() {
            return Ok(path);
        }
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(bin_dir) = exe.parent()
    {
        for ancestor in bin_dir.ancestors().take(6) {
            let candidate = ancestor.join("scripts").join("teams-auth.mjs");
            if candidate.exists() {
                return Ok(candidate);
            }
        }

        let share_candidate = bin_dir
            .parent()
            .map(|prefix| prefix.join("share").join("tmz").join("teams-auth.mjs"));
        if let Some(ref p) = share_candidate
            && p.exists()
        {
            return Ok(p.clone());
        }
    }

    Err(AuthenticationError::TokenExtractionError(
        "teams-auth.mjs not found. Set TMZ_AUTH_SCRIPT or run from the project directory.\n\
         Install with: cd scripts && npm install && npx playwright install chromium"
            .to_string(),
    ))
}

fn base64_decode(input: &str) -> Result<String, Box<dyn std::error::Error>> {
    use base64::Engine;

    let padded = match input.len() % 4 {
        0 => input.to_string(),
        n => format!("{}{}", input, "=".repeat(4 - n)),
    };

    let decoded = base64::engine::general_purpose::STANDARD.decode(padded)?;
    Ok(String::from_utf8(decoded)?)
}
