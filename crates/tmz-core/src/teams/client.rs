//! Teams API client using the native (internal) Teams endpoints.
//!
//! The Teams web client uses undocumented Skype-based APIs for chat operations,
//! not the Microsoft Graph API. This client implements those same endpoints:
//!
//! 1. Exchange the MSAL skype access token for a skypeToken via the authz endpoint
//! 2. Use the skypeToken with the chat service URL for all chat operations
//!
//! Graph API is still used for operations where it has sufficient scopes
//! (e.g., listing joined teams, channels).

use crate::teams::auth::AuthManager;
use crate::teams::models::{PresenceStatus, TeamsSession, UserPresence};
use crate::CoreError;
use reqwest::Client;

/// Teams API client.
#[derive(Debug)]
pub struct TeamsClient {
    http_client: Client,
    auth: AuthManager,
}

/// Authz endpoint for exchanging MSAL token for skypeToken.
const AUTHZ_URL: &str = "https://teams.microsoft.com/api/authsvc/v1.0/authz";

impl TeamsClient {
    /// Create a new Teams client.
    ///
    /// # Errors
    ///
    /// Returns an error if HTTP client creation fails.
    pub fn new() -> Result<Self, CoreError> {
        let http_client = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| CoreError::Other(format!("creating HTTP client: {e}")))?;

        let auth = AuthManager::new()
            .map_err(|e| CoreError::Other(format!("creating auth manager: {e}")))?;

        Ok(Self {
            http_client,
            auth,
        })
    }

    /// Check if authenticated and tokens are valid.
    ///
    /// # Errors
    ///
    /// Returns an error if auth check fails.
    pub fn is_authenticated(&self) -> Result<bool, CoreError> {
        self.auth
            .is_authenticated()
            .map_err(|e| CoreError::Auth(format!("auth check: {e}")))
    }

    /// Exchange the MSAL skype access token for a Teams session.
    ///
    /// Calls `POST /api/authsvc/v1.0/authz` to get a skypeToken and
    /// region-specific service URLs.
    ///
    /// # Errors
    ///
    /// Returns an error if not authenticated or the authz call fails.
    pub async fn get_session(&self) -> Result<TeamsSession, CoreError> {
        let tokens = self
            .auth
            .get_tokens()
            .map_err(|e| CoreError::Auth(format!("not authenticated: {e}")))?;

        let response = self
            .http_client
            .post(AUTHZ_URL)
            .bearer_auth(&tokens.skype_token)
            .header("Content-Length", "0")
            .send()
            .await
            .map_err(|e| CoreError::Api(format!("authz request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(CoreError::Api(format!(
                "authz failed: {status} - {text}"
            )));
        }

        let settings: serde_json::Value = response
            .json()
            .await
            .map_err(|e| CoreError::Serialization(format!("parsing authz response: {e}")))?;

        let skype_token = settings["tokens"]["skypeToken"]
            .as_str()
            .ok_or_else(|| CoreError::Api("missing skypeToken in authz response".to_string()))?
            .to_string();

        let chat_service_url = settings["regionGtms"]["chatService"]
            .as_str()
            .ok_or_else(|| CoreError::Api("missing chatService URL in authz response".to_string()))?
            .to_string();

        let teams_and_channels_service_url = settings["regionGtms"]["teamsAndChannelsService"]
            .as_str()
            .unwrap_or("")
            .to_string();

        // Decode skypeToken JWT for metadata
        let (skype_id, issued_at, expires_at) = decode_skype_token(&skype_token)?;

        Ok(TeamsSession {
            skype_token,
            skype_id,
            chat_service_url,
            teams_and_channels_service_url,
            issued_at,
            expires_at,
            raw_settings: settings,
        })
    }

    /// List user's conversations (chats, group chats, channels).
    ///
    /// Uses the native chat service API with skypeToken authentication.
    ///
    /// # Errors
    ///
    /// Returns an error if not authenticated or request fails.
    pub async fn list_chats(&self) -> Result<serde_json::Value, CoreError> {
        let session = self.get_session().await?;
        let url = format!(
            "{}/v1/users/ME/conversations?view=msnp24Equivalent&pageSize=500",
            session.chat_service_url
        );

        let response = self
            .http_client
            .get(&url)
            .header("Authentication", format!("skypetoken={}", session.skype_token))
            .send()
            .await
            .map_err(|e| CoreError::Api(format!("request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(CoreError::Api(format!(
                "list chats failed: {status} - {text}"
            )));
        }

        response
            .json::<serde_json::Value>()
            .await
            .map_err(|e| CoreError::Serialization(format!("parsing response: {e}")))
    }

    /// Get messages from a conversation.
    ///
    /// # Arguments
    ///
    /// * `conversation_id` - The conversation thread ID (e.g., `19:xxx@thread.v2`)
    /// * `page_size` - Number of messages to fetch (default: 200)
    ///
    /// # Errors
    ///
    /// Returns an error if not authenticated or request fails.
    pub async fn get_chat_messages(
        &self,
        conversation_id: &str,
        page_size: Option<i32>,
    ) -> Result<serde_json::Value, CoreError> {
        let session = self.get_session().await?;
        let size = page_size.unwrap_or(200);
        let url = format!(
            "{}/v1/users/ME/conversations/{}/messages?startTime=0&view=msnp24Equivalent&pageSize={size}",
            session.chat_service_url,
            urlencoding::encode(conversation_id)
        );

        let response = self
            .http_client
            .get(&url)
            .header("Authentication", format!("skypetoken={}", session.skype_token))
            .send()
            .await
            .map_err(|e| CoreError::Api(format!("request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(CoreError::Api(format!(
                "get messages failed: {status} - {text}"
            )));
        }

        let mut data: serde_json::Value = response
            .json()
            .await
            .map_err(|e| CoreError::Serialization(format!("parsing response: {e}")))?;

        // Mark messages from the current user
        if let Some(messages) = data.get_mut("messages").and_then(serde_json::Value::as_array_mut) {
            for msg in messages {
                let is_from_me = msg["from"]
                    .as_str()
                    .is_some_and(|from| from.ends_with(&session.skype_id));
                msg["isFromMe"] = serde_json::Value::Bool(is_from_me);
            }
        }

        Ok(data)
    }

    /// Send a message to a conversation.
    ///
    /// # Errors
    ///
    /// Returns an error if not authenticated or request fails.
    pub async fn send_message(
        &self,
        conversation_id: &str,
        content: &str,
    ) -> Result<serde_json::Value, CoreError> {
        let session = self.get_session().await?;
        let url = format!(
            "{}/v1/users/ME/conversations/{}/messages",
            session.chat_service_url,
            urlencoding::encode(conversation_id)
        );

        let body = serde_json::json!({
            "messagetype": "RichText/Html",
            "content": content
        });

        let response = self
            .http_client
            .post(&url)
            .header("Authentication", format!("skypetoken={}", session.skype_token))
            .json(&body)
            .send()
            .await
            .map_err(|e| CoreError::Api(format!("request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(CoreError::Api(format!(
                "send message failed: {status} - {text}"
            )));
        }

        response
            .json::<serde_json::Value>()
            .await
            .map_err(|e| CoreError::Serialization(format!("parsing response: {e}")))
    }

    /// Send a file to a conversation.
    ///
    /// Uploads the file to the ASM (Azure Service Manager) blob store, then
    /// sends a message referencing the uploaded object.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read, upload fails, or message send fails.
    pub async fn send_file(
        &self,
        conversation_id: &str,
        file_path: &std::path::Path,
    ) -> Result<serde_json::Value, CoreError> {
        let session = self.get_session().await?;

        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| CoreError::Other("invalid file name".to_string()))?
            .to_string();

        let file_bytes = tokio::fs::read(file_path).await.map_err(CoreError::Io)?;
        let file_size = file_bytes.len();

        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let is_image = matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp");

        // 1. Create ASM object + upload content
        let obj_id = self
            .upload_to_asm(&session, conversation_id, &file_name, &file_bytes, &ext, is_image)
            .await?;

        let obj_url = format!("https://api.asm.skype.com/v1/objects/{obj_id}");

        // 2. Build and send the file message
        let (msg_type, msg_content) = build_file_message(
            &obj_id, &obj_url, &file_name, file_size, is_image,
        );

        self.send_raw_message(conversation_id, &session, &msg_type, &msg_content)
            .await
    }

    async fn upload_to_asm(
        &self,
        session: &TeamsSession,
        conversation_id: &str,
        file_name: &str,
        file_bytes: &[u8],
        ext: &str,
        is_image: bool,
    ) -> Result<String, CoreError> {
        let obj_type = if is_image { "pish/image" } else { "sharing/file" };

        let mut meta = serde_json::json!({
            "type": obj_type,
            "permissions": {
                conversation_id: ["read"]
            }
        });
        if !is_image {
            meta["filename"] = serde_json::Value::String(file_name.to_string());
        }

        let resp = self
            .http_client
            .post("https://api.asm.skype.com/v1/objects")
            .header("Authorization", format!("skype_token {}", session.skype_token))
            .header("X-Client-Version", "0/0.0.0.0")
            .json(&meta)
            .send()
            .await
            .map_err(|e| CoreError::Api(format!("creating ASM object: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(CoreError::Api(format!("ASM create failed: {status} - {text}")));
        }

        let obj_data: serde_json::Value = resp.json().await
            .map_err(|e| CoreError::Serialization(format!("parsing ASM response: {e}")))?;

        let obj_id = obj_data["id"]
            .as_str()
            .ok_or_else(|| CoreError::Api("missing object id".to_string()))?
            .to_string();

        // Upload binary content
        let content_path = if is_image { "imgpsh" } else { "original" };
        let upload_url = format!("https://api.asm.skype.com/v1/objects/{obj_id}/content/{content_path}");

        let upload_resp = self
            .http_client
            .put(&upload_url)
            .header("Authorization", format!("skype_token {}", session.skype_token))
            .header("Content-Type", mime_for_ext(ext))
            .body(file_bytes.to_vec())
            .send()
            .await
            .map_err(|e| CoreError::Api(format!("uploading content: {e}")))?;

        if !upload_resp.status().is_success() {
            let status = upload_resp.status();
            let text = upload_resp.text().await.unwrap_or_default();
            return Err(CoreError::Api(format!("upload failed: {status} - {text}")));
        }

        Ok(obj_id)
    }

    async fn send_raw_message(
        &self,
        conversation_id: &str,
        session: &TeamsSession,
        msg_type: &str,
        content: &str,
    ) -> Result<serde_json::Value, CoreError> {
        let url = format!(
            "{}/v1/users/ME/conversations/{}/messages",
            session.chat_service_url,
            urlencoding::encode(conversation_id)
        );

        let body = serde_json::json!({
            "messagetype": msg_type,
            "content": content
        });

        let response = self
            .http_client
            .post(&url)
            .header("Authentication", format!("skypetoken={}", session.skype_token))
            .json(&body)
            .send()
            .await
            .map_err(|e| CoreError::Api(format!("request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(CoreError::Api(format!("send failed: {status} - {text}")));
        }

        response
            .json::<serde_json::Value>()
            .await
            .map_err(|e| CoreError::Serialization(format!("parsing response: {e}")))
    }

    /// List user's joined teams via Graph API.
    ///
    /// Uses the Graph token which has `Team.ReadBasic.All` scope.
    ///
    /// # Errors
    ///
    /// Returns an error if not authenticated or request fails.
    pub async fn list_teams(&self) -> Result<Vec<serde_json::Value>, CoreError> {
        let tokens = self
            .auth
            .get_tokens()
            .map_err(|e| CoreError::Auth(format!("not authenticated: {e}")))?;

        let url = "https://graph.microsoft.com/v1.0/me/joinedTeams";

        let response = self
            .http_client
            .get(url)
            .bearer_auth(&tokens.graph_token)
            .send()
            .await
            .map_err(|e| CoreError::Api(format!("request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(CoreError::Api(format!(
                "list teams failed: {status} - {text}"
            )));
        }

        let data: serde_json::Value = response
            .json()
            .await
            .map_err(|e| CoreError::Serialization(format!("parsing response: {e}")))?;

        Ok(data["value"]
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    /// List channels in a team via Graph API.
    ///
    /// Uses the Graph token which has `Channel.ReadBasic.All` scope.
    ///
    /// # Errors
    ///
    /// Returns an error if not authenticated or request fails.
    pub async fn list_channels(&self, team_id: &str) -> Result<Vec<serde_json::Value>, CoreError> {
        let tokens = self
            .auth
            .get_tokens()
            .map_err(|e| CoreError::Auth(format!("not authenticated: {e}")))?;

        let url = format!(
            "https://graph.microsoft.com/v1.0/teams/{}/channels",
            urlencoding::encode(team_id)
        );

        let response = self
            .http_client
            .get(&url)
            .bearer_auth(&tokens.graph_token)
            .send()
            .await
            .map_err(|e| CoreError::Api(format!("request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(CoreError::Api(format!(
                "list channels failed: {status} - {text}"
            )));
        }

        let data: serde_json::Value = response
            .json()
            .await
            .map_err(|e| CoreError::Serialization(format!("parsing response: {e}")))?;

        Ok(data["value"]
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    /// Get messages from a channel conversation via the native chat API.
    ///
    /// Channel messages use the same chat service endpoint but with
    /// the channel thread ID.
    ///
    /// # Errors
    ///
    /// Returns an error if not authenticated or request fails.
    pub async fn get_channel_messages(
        &self,
        _team_id: &str,
        channel_id: &str,
        page_size: Option<i32>,
    ) -> Result<serde_json::Value, CoreError> {
        // Channel conversations use the same native API with the channel thread ID
        self.get_chat_messages(channel_id, page_size).await
    }

    /// Get user presence status.
    ///
    /// # Errors
    ///
    /// Returns an error if not authenticated or request fails.
    pub async fn get_user_presence(&self, user_id: &str) -> Result<UserPresence, CoreError> {
        let tokens = self
            .auth
            .get_tokens()
            .map_err(|e| CoreError::Auth(format!("not authenticated: {e}")))?;

        let url = format!(
            "https://presence.teams.microsoft.com/v1/presence/{}",
            urlencoding::encode(user_id)
        );

        let response = self
            .http_client
            .get(&url)
            .bearer_auth(&tokens.presence_token)
            .send()
            .await
            .map_err(|e| CoreError::Api(format!("request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(CoreError::Api(format!(
                "presence failed: {status} - {text}"
            )));
        }

        let data: serde_json::Value = response
            .json()
            .await
            .map_err(|e| CoreError::Serialization(format!("parsing response: {e}")))?;

        Ok(UserPresence {
            user_id: data["id"].as_str().unwrap_or(user_id).to_string(),
            availability: match data["availability"].as_str() {
                Some("Available") => PresenceStatus::Available,
                Some("Busy") => PresenceStatus::Busy,
                Some("DoNotDisturb") => PresenceStatus::DoNotDisturb,
                Some("Away") => PresenceStatus::Away,
                Some("Offline") => PresenceStatus::Offline,
                _ => PresenceStatus::Unknown,
            },
            activity: data["activity"].as_str().map(String::from),
            status_message: None,
            last_active: None,
        })
    }

    /// Get current user info via Graph API.
    ///
    /// # Errors
    ///
    /// Returns an error if not authenticated or request fails.
    pub async fn get_me(&self) -> Result<serde_json::Value, CoreError> {
        let tokens = self
            .auth
            .get_tokens()
            .map_err(|e| CoreError::Auth(format!("not authenticated: {e}")))?;

        let response = self
            .http_client
            .get("https://graph.microsoft.com/v1.0/me")
            .bearer_auth(&tokens.graph_token)
            .send()
            .await
            .map_err(|e| CoreError::Api(format!("request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(CoreError::Api(format!(
                "get me failed: {status} - {text}"
            )));
        }

        response
            .json::<serde_json::Value>()
            .await
            .map_err(|e| CoreError::Serialization(format!("parsing response: {e}")))
    }
}

/// Decode a skypeToken JWT to extract skype ID and expiry.
/// Build the XML message body for a file or image upload.
fn build_file_message(
    obj_id: &str,
    obj_url: &str,
    file_name: &str,
    file_size: usize,
    is_image: bool,
) -> (String, String) {
    if is_image {
        let view_link = format!("https://api.asm.skype.com/s/i?{obj_id}");
        let content = format!(
            r#"<URIObject type="Picture.1" uri="{obj_url}" url_thumbnail="{obj_url}/views/imgt1"><a href="{view_link}">{view_link}</a><meta type="photo" originalName="{file_name}"/></URIObject>"#,
        );
        ("RichText/UriObject".to_string(), content)
    } else {
        let view_link = format!(
            "https://login.skype.com/login/sso?go=webclient.xmm&docid={obj_id}"
        );
        let content = format!(
            r#"<URIObject type="File.1" uri="{obj_url}" url_thumbnail="{obj_url}/views/thumbnail"><FileSize v="{file_size}"/><OriginalName v="{file_name}"/><a href="{view_link}">{view_link}</a></URIObject>"#,
        );
        ("RichText/Media_GenericFile".to_string(), content)
    }
}

/// Map a file extension to a MIME type.
fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "json" => "application/json",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "csv" => "text/csv",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

fn decode_skype_token(token: &str) -> Result<(String, i64, i64), CoreError> {
    use base64::Engine;

    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return Err(CoreError::Auth("invalid skypeToken JWT format".to_string()));
    }

    let padded = match parts[1].len() % 4 {
        0 => parts[1].to_string(),
        n => format!("{}{}", parts[1], "=".repeat(4 - n)),
    };

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(padded)
        .map_err(|e| CoreError::Auth(format!("base64 decode skypeToken: {e}")))?;

    let payload = String::from_utf8(decoded)
        .map_err(|e| CoreError::Auth(format!("invalid UTF-8 in skypeToken: {e}")))?;

    let claims: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|e| CoreError::Auth(format!("parsing skypeToken claims: {e}")))?;

    let skype_id = claims["skypeid"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();

    let iat = claims["iat"].as_i64().unwrap_or(0);
    let exp = claims["exp"].as_i64().unwrap_or(0);

    Ok((skype_id, iat, exp))
}
