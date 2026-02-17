//! Browser-based authentication for Microsoft Teams.
//!
//! This module provides authentication by:
//! 1. Launching a Playwright-driven browser for user to login
//! 2. Automatically extracting MSAL access tokens from localStorage
//! 3. Storing tokens securely using vault
//!
//! The login flow uses a Node.js script (`scripts/teams-auth.mjs`) that
//! opens Chromium, lets the user complete SSO/MFA, then extracts tokens
//! and outputs them as JSON.

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

impl AuthManager {
    /// Teams web client URL.
    pub const TEAMS_URL: &str = "https://teams.microsoft.com/v2";
    /// Client ID for Teams web application.
    pub const TEAMS_CLIENT_ID: &str = "5e3ce6c0-2b1f-4285-8d4b-75ee78787346";

    /// Create a new authentication manager.
    #[must_use]
    pub fn new() -> Self {
        Self {
            storage: TokenStorage::new(),
        }
    }

    /// Check if we have valid cached tokens.
    ///
    /// # Errors
    ///
    /// Returns an error if vault access fails.
    pub fn is_authenticated(&self) -> Result<bool, AuthenticationError> {
        Ok(self.storage.has_valid_tokens()?)
    }

    /// Run the Playwright-based browser login flow.
    ///
    /// Launches `scripts/teams-auth.mjs` which opens Chromium, lets the user
    /// complete SSO/MFA, extracts tokens from localStorage, and returns them.
    ///
    /// # Errors
    ///
    /// Returns an error if the script is not found, fails to execute,
    /// or tokens cannot be extracted.
    pub async fn browser_login(
        &self,
        timeout_secs: Option<u64>,
    ) -> Result<TeamsTokens, AuthenticationError> {
        let script_path = find_auth_script()?;

        let mut cmd = tokio::process::Command::new("node");
        cmd.arg(&script_path);

        if let Some(t) = timeout_secs {
            cmd.arg("--timeout").arg(t.to_string());
        }

        // stdout = token JSON, stderr = progress/log messages
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::inherit());

        log::debug!("running auth script: {}", script_path.display());

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
                "browser login failed or timed out - check stderr for details".to_string(),
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

    /// Store tokens extracted from browser localStorage.
    ///
    /// Takes a map of token keys to their JSON values from localStorage,
    /// extracts the specific tokens needed, and stores them.
    ///
    /// # Errors
    ///
    /// Returns an error if tokens cannot be parsed or stored.
    pub fn store_tokens_from_browser(
        &self,
        local_storage: &std::collections::HashMap<String, String>,
    ) -> Result<TeamsTokens, AuthenticationError> {
        // Find tokens for specific resources
        let skype_token_json = Self::find_token(local_storage, "api.spaces.skype.com")?;
        let chat_token_json = Self::find_token(local_storage, "chatsvcagg.teams.microsoft.com")?;
        let graph_token_json = Self::find_token(local_storage, "graph.microsoft.com")?;
        let presence_token_json = Self::find_token(local_storage, "presence.teams.microsoft.com")?;

        // Parse MSAL token structures
        let skype_token: MsalToken = serde_json::from_str(&skype_token_json)
            .map_err(|e| AuthenticationError::TokenExtractionError(format!("parsing skype token: {e}")))?;
        let chat_token: MsalToken = serde_json::from_str(&chat_token_json)
            .map_err(|e| AuthenticationError::TokenExtractionError(format!("parsing chat token: {e}")))?;
        let graph_token: MsalToken = serde_json::from_str(&graph_token_json)
            .map_err(|e| AuthenticationError::TokenExtractionError(format!("parsing graph token: {e}")))?;
        let presence_token: MsalToken = serde_json::from_str(&presence_token_json)
            .map_err(|e| AuthenticationError::TokenExtractionError(format!("parsing presence token: {e}")))?;

        // Extract claims from skype token
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

    /// Get cached tokens if available and valid.
    ///
    /// # Errors
    ///
    /// Returns an error if tokens are not available or storage access fails.
    pub fn get_tokens(&self) -> Result<TeamsTokens, AuthenticationError> {
        let tokens = self.storage.load_tokens()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);

        if tokens.expires_at < now {
            return Err(AuthenticationError::TokenExtractionError(
                "Token expired - run 'tmz auth login' to refresh".to_string(),
            ));
        }

        Ok(tokens)
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

impl Default for AuthManager {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_token_claims(token: &str) -> Result<(String, String, String, i64), AuthenticationError> {
    // Parse JWT to extract claims
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
    // 1. Explicit env override
    if let Ok(p) = std::env::var("TMZ_AUTH_SCRIPT") {
        let path = std::path::PathBuf::from(p);
        if path.exists() {
            return Ok(path);
        }
    }

    // 2. Next to installed binary: <prefix>/bin/tmz-cli -> <prefix>/share/tmz/teams-auth.mjs
    if let Ok(exe) = std::env::current_exe()
        && let Some(bin_dir) = exe.parent()
    {
        // dev layout: target/debug/tmz-cli -> scripts/teams-auth.mjs
        // check several levels up for the workspace root
        for ancestor in bin_dir.ancestors().take(6) {
            let candidate = ancestor.join("scripts").join("teams-auth.mjs");
            if candidate.exists() {
                return Ok(candidate);
            }
        }

        // installed layout: bin/tmz-cli -> share/tmz/teams-auth.mjs
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
