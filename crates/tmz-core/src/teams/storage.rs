//! Token storage using a plain JSON file.
//!
//! Tokens are short-lived JWTs (typically 1 hour) so heavyweight encryption
//! is unnecessary. They are stored at `$XDG_STATE_HOME/tmz/tokens.json` with
//! `0600` permissions (owner-only read/write).

use crate::teams::models::TeamsTokens;
use crate::CoreError;
use std::path::PathBuf;

/// Storage for Teams authentication tokens.
#[derive(Debug)]
pub struct TokenStorage {
    path: PathBuf,
}

impl TokenStorage {
    /// Token file name.
    const FILENAME: &str = "tokens.json";

    /// Create a new token storage instance using the default state directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the state directory cannot be determined.
    pub fn new() -> Result<Self, CoreError> {
        let state_dir = crate::default_state_dir()
            .map_err(|e| CoreError::Path(format!("resolving state dir: {e}")))?;
        Ok(Self {
            path: state_dir.join(Self::FILENAME),
        })
    }

    /// Store tokens to disk.
    ///
    /// Creates parent directories and sets file permissions to `0600`.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or file I/O fails.
    pub fn store_tokens(&self, tokens: &TeamsTokens) -> Result<(), CoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(CoreError::Io)?;
        }

        let json = serde_json::to_string_pretty(tokens)
            .map_err(|e| CoreError::Serialization(format!("serializing tokens: {e}")))?;

        std::fs::write(&self.path, json.as_bytes()).map_err(CoreError::Io)?;

        // Restrict to owner read/write
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))
                .map_err(CoreError::Io)?;
        }

        Ok(())
    }

    /// Load tokens from disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the file does not exist, or parsing fails.
    pub fn load_tokens(&self) -> Result<TeamsTokens, CoreError> {
        if !self.path.exists() {
            return Err(CoreError::SecretNotFound(
                "no stored tokens. Run 'tmz auth login' first.".to_string(),
            ));
        }

        let json = std::fs::read_to_string(&self.path).map_err(CoreError::Io)?;

        serde_json::from_str(&json)
            .map_err(|e| CoreError::Serialization(format!("deserializing tokens: {e}")))
    }

    /// Check if tokens are stored and not expired.
    ///
    /// # Errors
    ///
    /// Returns an error if file I/O fails (missing file returns `Ok(false)`).
    pub fn has_valid_tokens(&self) -> Result<bool, CoreError> {
        match self.load_tokens() {
            Ok(tokens) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64);
                Ok(tokens.expires_at > now)
            }
            Err(CoreError::SecretNotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Delete stored tokens.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be removed.
    pub fn clear_tokens(&self) -> Result<(), CoreError> {
        if self.path.exists() {
            std::fs::remove_file(&self.path).map_err(CoreError::Io)?;
        }
        Ok(())
    }
}
