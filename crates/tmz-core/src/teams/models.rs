//! Data models for Microsoft Teams.

use serde::{Deserialize, Serialize};

/// A Teams conversation (chat, channel, or group chat).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    /// Unique conversation ID (format: 19:{id}@thread.tacv2 or 19:{id}@thread.v2).
    pub id: String,
    /// Conversation type (chat, channel, or meeting).
    pub conversation_type: ConversationType,
    /// Display title/name of the conversation.
    pub title: Option<String>,
    /// Topic for group chats.
    pub topic: Option<String>,
    /// List of conversation members.
    pub members: Vec<ConversationMember>,
    /// Last message in the conversation.
    pub last_message: Option<Message>,
    /// Timestamp of last activity (Unix milliseconds).
    pub last_activity: Option<i64>,
    /// Number of unread messages.
    pub unread_count: Option<u32>,
    /// Teams context for channels.
    pub team: Option<TeamInfo>,
    /// Channel context for channel conversations.
    pub channel: Option<ChannelInfo>,
}

/// Type of conversation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ConversationType {
    /// One-on-one chat.
    Chat,
    /// Group chat.
    Group,
    /// Channel conversation.
    Channel,
    /// Meeting conversation.
    Meeting,
}

/// A member of a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMember {
    /// User ID.
    pub id: String,
    /// Display name.
    pub display_name: String,
    /// Email address.
    pub email: Option<String>,
    /// User principal name (UPN).
    pub upn: Option<String>,
    /// Tenant ID.
    pub tenant_id: Option<String>,
}

/// A message in a Teams conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Unique message ID.
    pub id: String,
    /// Conversation ID this message belongs to.
    pub conversation_id: String,
    /// Sender information.
    pub from: Option<ConversationMember>,
    /// Message content (HTML or text).
    pub content: String,
    /// Message content type.
    pub content_type: ContentType,
    /// Timestamp when message was sent (Unix milliseconds).
    pub timestamp: i64,
    /// Message importance.
    pub importance: Option<MessageImportance>,
    /// Reactions to the message.
    pub reactions: Vec<Reaction>,
    /// Attachments in the message.
    pub attachments: Vec<Attachment>,
    /// Reply thread ID for channel messages.
    pub reply_to_id: Option<String>,
}

/// Message content type.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ContentType {
    /// HTML formatted content.
    #[default]
    Html,
    /// Plain text content.
    Text,
}

/// Message importance level.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageImportance {
    /// Normal importance.
    Normal,
    /// High importance.
    High,
    /// Urgent importance.
    Urgent,
}

/// A reaction to a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reaction {
    /// Reaction type (like, heart, laugh, etc.).
    pub reaction_type: String,
    /// User who reacted.
    pub user_id: String,
    /// Timestamp of reaction.
    pub timestamp: i64,
}

/// A file attachment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    /// Attachment ID.
    pub id: String,
    /// File name.
    pub name: String,
    /// MIME type.
    pub content_type: String,
    /// File size in bytes.
    pub size: Option<u64>,
    /// Download URL.
    pub url: Option<String>,
}

/// Information about a Team.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamInfo {
    /// Team ID.
    pub id: String,
    /// Team name.
    pub name: String,
    /// Team description.
    pub description: Option<String>,
    /// Display name for the team.
    pub display_name: String,
}

/// Information about a channel within a team.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {
    /// Channel ID.
    pub id: String,
    /// Channel name.
    pub name: String,
    /// Channel description.
    pub description: Option<String>,
    /// Parent team ID.
    pub team_id: String,
    /// Whether this is the default General channel.
    pub is_general: bool,
}

/// User presence status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PresenceStatus {
    /// Available.
    Available,
    /// Busy.
    Busy,
    /// Do not disturb.
    DoNotDisturb,
    /// Away.
    Away,
    /// Offline.
    Offline,
    /// Unknown/unspecified.
    Unknown,
}

/// User presence information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPresence {
    /// User ID.
    pub user_id: String,
    /// Current availability.
    pub availability: PresenceStatus,
    /// Activity status.
    pub activity: Option<String>,
    /// Status message/note.
    pub status_message: Option<String>,
    /// Last active timestamp.
    pub last_active: Option<i64>,
}

/// Authentication tokens for Teams APIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamsTokens {
    /// Access token for Skype/Chat APIs (api.spaces.skype.com).
    pub skype_token: String,
    /// Access token for Teams Chat Service Aggregation (chatsvcagg.teams.microsoft.com).
    pub chat_token: String,
    /// Access token for Microsoft Graph API.
    pub graph_token: String,
    /// Access token for Presence API.
    pub presence_token: String,
    /// Tenant ID.
    pub tenant_id: String,
    /// User object ID.
    pub user_id: String,
    /// User principal name (email).
    pub user_principal_name: String,
    /// Token expiry timestamp.
    pub expires_at: i64,
}

/// Session data from the Teams authz endpoint.
///
/// Obtained by exchanging the MSAL skype access token via
/// `POST https://teams.microsoft.com/api/authsvc/v1.0/authz`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamsSession {
    /// The skypeToken used for chat API authentication.
    pub skype_token: String,
    /// Skype ID of the authenticated user.
    pub skype_id: String,
    /// Chat service base URL (e.g., `https://emea.ng.msg.teams.microsoft.com`).
    pub chat_service_url: String,
    /// Teams and channels service base URL.
    pub teams_and_channels_service_url: String,
    /// Token issue timestamp.
    pub issued_at: i64,
    /// Token expiry timestamp.
    pub expires_at: i64,
    /// Raw authz response for accessing other region-specific URLs.
    pub raw_settings: serde_json::Value,
}
