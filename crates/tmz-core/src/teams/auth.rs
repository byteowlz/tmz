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

use crate::CoreError;
use crate::teams::models::TeamsTokens;
use crate::teams::storage::TokenStorage;
use serde_json::Value;
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
        fresh: bool,
    ) -> Result<TeamsTokens, AuthenticationError> {
        let script_path = find_auth_script()?;

        let mut cmd = tokio::process::Command::new("node");
        cmd.arg(&script_path);

        if fresh {
            cmd.arg("--fresh");
        }

        if let Some(t) = timeout_secs {
            cmd.arg("--timeout").arg(t.to_string());
        }

        if headless {
            cmd.arg("--headless");
        }

        // stdout = token JSON, stderr = progress/log messages.
        // Always capture stderr so we can detect Playwright import errors
        // and show actionable hints. In interactive mode, captured stderr
        // is printed after the process completes.
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        log::debug!(
            "running auth script: {} (headless={headless})",
            script_path.display()
        );

        let output = cmd.output().await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AuthenticationError::TokenExtractionError(
                    "node not found - install Node.js to use browser login.\n\
                     Install Node.js from https://nodejs.org/ and ensure it is in your PATH."
                        .to_string(),
                )
            } else {
                AuthenticationError::TokenExtractionError(format!("failed to run auth script: {e}"))
            }
        })?;

        // Print captured stderr in interactive mode so progress is visible
        if !headless {
            let stderr_bytes = &output.stderr;
            if !stderr_bytes.is_empty() {
                let _ = std::io::Write::write_all(&mut std::io::stderr(), stderr_bytes);
            }
        }

        if !output.status.success() {
            let stderr_text = String::from_utf8_lossy(&output.stderr);

            // Detect Playwright not installed (ESM import failure)
            if stderr_text.contains("Cannot find package 'playwright'")
                || stderr_text.contains("ERR_MODULE_NOT_FOUND")
                || stderr_text.contains("Cannot find module")
            {
                let script_dir = script_path.parent().map_or_else(
                    || "the script directory".to_string(),
                    |p| p.display().to_string(),
                );
                return Err(AuthenticationError::TokenExtractionError(format!(
                    "Playwright is not installed. The auth script uses ES modules, \
                     so Playwright must be installed locally (not globally).\n\
                     Fix: cd \"{script_dir}\" && npm install && npx playwright install chromium"
                )));
            }

            return Err(AuthenticationError::TokenExtractionError(if headless {
                "headless token refresh failed - SSO session may have expired. Run 'tmz auth login' to re-authenticate.".to_string()
            } else {
                format!(
                    "browser login failed or timed out.\n{}",
                    if stderr_text.is_empty() {
                        "No additional error details available.".to_string()
                    } else {
                        stderr_text.into_owned()
                    }
                )
            }));
        }

        let stdout = String::from_utf8(output.stdout).map_err(|e| {
            AuthenticationError::TokenExtractionError(format!("invalid UTF-8 output: {e}"))
        })?;

        let output: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| {
            AuthenticationError::TokenExtractionError(format!("parsing token output: {e}"))
        })?;

        // New format: { "skype_token": "...", "chat_token": "...", ... }
        if output.get("skype_token").is_some() {
            return self.store_tokens_from_script_output(&output);
        }

        // Legacy format: raw localStorage HashMap
        let local_storage: std::collections::HashMap<String, String> =
            serde_json::from_value(output).map_err(|e| {
                AuthenticationError::TokenExtractionError(format!("parsing token output: {e}"))
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
        self.browser_login(Some(HEADLESS_TIMEOUT_SECS), true, false)
            .await
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
                            log::warn!(
                                "headless refresh failed but tokens still valid for {}s",
                                tokens.expires_at - now
                            );
                            Ok(tokens)
                        } else {
                            Err(AuthenticationError::TokenExtractionError(
                                "tokens expired and headless refresh failed. Run 'tmz auth login'."
                                    .to_string(),
                            ))
                        }
                    }
                }
            }
            Err(CoreError::SecretNotFound(_)) => Err(AuthenticationError::TokenExtractionError(
                "not authenticated. Run 'tmz auth login' first.".to_string(),
            )),
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

    /// Store tokens from the new script output format.
    ///
    /// The script outputs `{ "skype_token", "chat_token", "graph_token",
    /// "presence_token", "expires_in" }`.
    ///
    /// # Errors
    ///
    /// Returns an error if required fields are missing or JWT parsing fails.
    fn store_tokens_from_script_output(
        &self,
        output: &serde_json::Value,
    ) -> Result<TeamsTokens, AuthenticationError> {
        let get_required = |field: &str| -> Result<String, AuthenticationError> {
            output[field].as_str().map(String::from).ok_or_else(|| {
                AuthenticationError::TokenExtractionError(format!("missing {field} in output"))
            })
        };
        let get_optional = |field: &str| -> String {
            output[field].as_str().map(String::from).unwrap_or_default()
        };

        let skype_token = get_required("skype_token")?;
        let chat_token = get_optional("chat_token");
        let graph_token = get_optional("graph_token");
        let presence_token = get_optional("presence_token");

        let (tenant_id, user_id, upn, expires_at) = derive_identity_from_tokens([
            skype_token.as_str(),
            chat_token.as_str(),
            graph_token.as_str(),
            presence_token.as_str(),
        ]);

        let tokens = TeamsTokens {
            skype_token,
            chat_token,
            graph_token,
            presence_token,
            tenant_id,
            user_id,
            user_principal_name: upn,
            expires_at,
        };

        self.storage.store_tokens(&tokens)?;
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
        let skype_token = Self::extract_resource_token(local_storage, "api.spaces.skype.com")?;
        let chat_token =
            Self::extract_resource_token(local_storage, "chatsvcagg.teams.microsoft.com")?;
        let graph_token = Self::extract_resource_token(local_storage, "graph.microsoft.com")?;
        let presence_token =
            Self::extract_resource_token(local_storage, "presence.teams.microsoft.com")?;

        let (tenant_id, user_id, upn, expires_at) = derive_identity_from_tokens([
            skype_token.as_str(),
            chat_token.as_str(),
            graph_token.as_str(),
            presence_token.as_str(),
        ]);

        let tokens = TeamsTokens {
            skype_token,
            chat_token,
            graph_token,
            presence_token,
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
        let (tenant_id, user_id, upn, expires_at) =
            derive_identity_from_tokens([skype_token, chat_token, graph_token, presence_token]);

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

    fn extract_resource_token(
        local_storage: &std::collections::HashMap<String, String>,
        resource: &str,
    ) -> Result<String, AuthenticationError> {
        let resource_lower = resource.to_lowercase();

        let mut candidates: Vec<(&str, &str)> = local_storage
            .iter()
            .filter_map(|(key, value)| {
                let key_lower = key.to_lowercase();
                (key_lower.contains("accesstoken")
                    && key_lower.contains("login.windows.net")
                    && key_lower.contains(&resource_lower))
                .then_some((key.as_str(), value.as_str()))
            })
            .collect();

        if candidates.is_empty() {
            return Err(AuthenticationError::TokenExtractionError(format!(
                "no token found for resource: {resource}"
            )));
        }

        // Prefer Teams client-id scoped entries first. There can be many stale
        // access-token entries in localStorage and iteration order is undefined.
        candidates.sort_by_key(|(key, _)| {
            let key_lower = key.to_lowercase();
            let client_rank = if key_lower.contains(Self::TEAMS_CLIENT_ID) {
                0usize
            } else {
                1usize
            };
            (client_rank, key.len())
        });

        let mut first_parseable: Option<String> = None;
        let mut parse_errors = Vec::new();

        for (key, value) in candidates {
            match Self::extract_access_token(value) {
                Ok(token) => {
                    // Keep first parseable token as fallback. For JWT-like tokens,
                    // prefer one with decodable claims.
                    if parse_token_claims(&token).is_ok() {
                        return Ok(token);
                    }
                    if first_parseable.is_none() {
                        first_parseable = Some(token);
                    }
                }
                Err(e) => parse_errors.push(format!("{key}: {e}")),
            }
        }

        if let Some(token) = first_parseable {
            return Ok(token);
        }

        Err(AuthenticationError::TokenExtractionError(format!(
            "unable to parse token for resource {resource}. {} candidate(s) found. {}",
            parse_errors.len(),
            if parse_errors.is_empty() {
                "No parse details available.".to_string()
            } else {
                parse_errors.join(" | ")
            }
        )))
    }

    fn extract_access_token(raw_value: &str) -> Result<String, AuthenticationError> {
        extract_jwt_from_str(raw_value, 5).ok_or_else(|| {
            AuthenticationError::TokenExtractionError(
                "missing access token field in token payload".to_string(),
            )
        })
    }
}

fn looks_like_jwt(value: &str) -> bool {
    let value = normalize_token_candidate(value);
    if value.contains(' ') {
        return false;
    }

    let segments: Vec<&str> = value.split('.').collect();
    if !(segments.len() == 3 || segments.len() == 5) {
        return false;
    }

    segments
        .iter()
        .all(|segment| !segment.is_empty() && segment.chars().all(is_token_char))
}

fn normalize_token_candidate(input: &str) -> String {
    let mut s = input
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string();

    // Common wrapper from auth headers.
    if s.to_ascii_lowercase().starts_with("bearer ") {
        s = s[7..].trim().to_string();
    }

    s
}

fn extract_jwt_from_str(input: &str, depth: usize) -> Option<String> {
    let normalized = normalize_token_candidate(input);
    if looks_like_jwt(&normalized) {
        return Some(normalized);
    }

    if let Ok(decoded) = urlencoding::decode(&normalized) {
        let decoded = normalize_token_candidate(decoded.as_ref());
        if looks_like_jwt(&decoded) {
            return Some(decoded);
        }
    }

    if depth == 0 {
        return None;
    }

    let parsed: Value = serde_json::from_str(&normalized).ok()?;
    extract_jwt_from_value(&parsed, depth - 1)
}

fn extract_jwt_from_value(value: &Value, depth: usize) -> Option<String> {
    if depth == 0 {
        return None;
    }

    match value {
        Value::String(s) => extract_jwt_from_str(s, depth - 1),
        Value::Array(items) => items
            .iter()
            .find_map(|item| extract_jwt_from_value(item, depth - 1)),
        Value::Object(map) => {
            for key in preferred_token_fields() {
                if let Some(value) = map.get(*key)
                    && let Some(token) = extract_jwt_from_value(value, depth - 1)
                {
                    return Some(token);
                }
            }

            // Then scan fields that look token-ish.
            for (key, value) in map {
                let key_lower = key.to_lowercase();
                if (key_lower.contains("token")
                    || key_lower.contains("secret")
                    || key_lower.contains("credential"))
                    && let Some(token) = extract_jwt_from_value(value, depth - 1)
                {
                    return Some(token);
                }
            }

            // Last resort: deep scan every value.
            map.values()
                .find_map(|child| extract_jwt_from_value(child, depth - 1))
        }
        _ => None,
    }
}

fn preferred_token_fields() -> &'static [&'static str] {
    &[
        "secret",
        "accesstoken",
        "access_token",
        "token",
        "credential",
        "value",
        "assertion",
    ]
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '~'
}

fn derive_identity_from_tokens<'a>(
    tokens: impl IntoIterator<Item = &'a str>,
) -> (String, String, String, i64) {
    for token in tokens {
        if let Ok(parsed) = parse_token_claims(token) {
            return parsed;
        }
    }

    // Fallback for non-JWT/opaque access tokens.
    (
        "unknown".to_string(),
        "unknown".to_string(),
        "unknown".to_string(),
        now_epoch() + 3600,
    )
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn parse_token_claims(token: &str) -> Result<(String, String, String, i64), AuthenticationError> {
    let token = normalize_token_candidate(token);
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
/// 2. `$XDG_DATA_HOME/tmz/teams-auth.mjs` (installed by `just install-all`)
/// 3. Same directory as the `tmz` binary (Windows zip layout)
/// 4. System install (`/usr/share/tmz/`, e.g. AUR)
/// 5. Development: walk up from binary to find `scripts/` directory
fn find_auth_script() -> Result<std::path::PathBuf, AuthenticationError> {
    const SCRIPT_NAME: &str = "teams-auth.mjs";

    // 1. Explicit env override
    if let Ok(p) = std::env::var("TMZ_AUTH_SCRIPT") {
        let path = std::path::PathBuf::from(p);
        if path.exists() {
            return Ok(path);
        }
    }

    // 2. Installed location: $XDG_DATA_HOME/tmz/teams-auth.mjs
    if let Ok(data_dir) = crate::default_data_dir() {
        let candidate = data_dir.join(SCRIPT_NAME);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    // 3. Same directory as the binary (Windows zip / portable layout)
    if let Ok(exe) = std::env::current_exe()
        && let Some(bin_dir) = exe.parent()
    {
        let candidate = bin_dir.join(SCRIPT_NAME);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    // 4. System install (e.g. AUR: /usr/share/tmz/)
    let system_path = std::path::Path::new("/usr/share/tmz").join(SCRIPT_NAME);
    if system_path.exists() {
        return Ok(system_path);
    }

    // 5. Development: walk up from binary to find scripts/ directory
    if let Ok(exe) = std::env::current_exe()
        && let Some(bin_dir) = exe.parent()
    {
        for ancestor in bin_dir.ancestors().take(6) {
            let candidate = ancestor.join("scripts").join(SCRIPT_NAME);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    Err(AuthenticationError::TokenExtractionError(
        "teams-auth.mjs not found. Run 'just install-all' or set TMZ_AUTH_SCRIPT.".to_string(),
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
