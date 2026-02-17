//! Token storage using vault for secure credential management.

use crate::teams::models::TeamsTokens;
use crate::CoreError;
use vault_core::store::{SecretEntry, SecretStore, VaultStore};
use std::collections::BTreeMap;

/// Storage for Teams authentication tokens using secure storage.
#[derive(Debug)]
pub struct TokenStorage {
    service_name: String,
}

impl TokenStorage {
    /// Service name used for vault storage.
    pub const SERVICE_NAME: &str = "tmz";
    /// Key name for the main tokens entry.
    pub const TOKEN_KEY: &str = "teams_tokens";

    /// Create a new token storage instance.
    #[must_use]
pub fn new() -> Self {
        Self {
            service_name: Self::SERVICE_NAME.to_string(),
        }
    }

    /// Store tokens in the secure storage.
    ///
    /// # Errors
    ///
    /// Returns an error if the vault is locked or write fails.
    pub fn store_tokens(&self, tokens: &TeamsTokens) -> Result<(), CoreError> {
        let vault = Self::get_vault()?;
        let json = serde_json::to_string(tokens)
            .map_err(|e| CoreError::Serialization(format!("serializing tokens: {e}")))?;

        let mut fields = BTreeMap::new();
        fields.insert("value".to_string(), json);

        let entry = SecretEntry::new(
            &self.service_name,
            Self::TOKEN_KEY,
            fields,
        );

        vault.set(&self.service_name, Self::TOKEN_KEY, &entry)?;
        Ok(())
    }

    /// Load tokens from the secure storage.
    ///
    /// # Errors
    ///
    /// Returns an error if tokens are not found, vault is locked, or read fails.
    pub fn load_tokens(&self) -> Result<TeamsTokens, CoreError> {
        let vault = Self::get_vault()?;
        let entry = vault.get(&self.service_name, Self::TOKEN_KEY)?;
        let json = entry
            .field("value")
            .ok_or_else(|| CoreError::Auth("token field not found".to_string()))?;

        serde_json::from_str(json)
            .map_err(|e| CoreError::Serialization(format!("deserializing tokens: {e}")))
    }

    /// Check if tokens are stored and not expired.
    ///
    /// # Errors
    ///
    /// Returns an error if vault access fails.
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
    /// Returns an error if vault is locked or delete fails.
    pub fn clear_tokens(&self) -> Result<(), CoreError> {
        let vault = Self::get_vault()?;
        vault
            .delete(&self.service_name, Self::TOKEN_KEY)
            .or_else(|e| match e {
                vault_core::CoreError::SecretNotFound(_) => Ok(()),
                _ => Err(e),
            })?;
        Ok(())
    }

    fn get_vault() -> Result<VaultStore, CoreError> {
        let vault_path = vault_core::store::central_vault_path()?;
        Ok(VaultStore::new(vault_path))
    }
}

impl Default for TokenStorage {
    fn default() -> Self {
        Self::new()
    }
}
