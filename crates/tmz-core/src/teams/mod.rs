//! Teams API client and authentication module.
//!
//! This module provides:
//! - Authentication via browser automation
//! - Token extraction and storage
//! - API clients for Teams endpoints

pub mod auth;
pub mod client;
pub mod models;
pub mod storage;

pub use auth::{AuthManager, AuthenticationError};
pub use client::TeamsClient;
pub use models::{
    Attachment, ChannelInfo, ContentType, Conversation, ConversationMember, ConversationType,
    Message, MessageImportance, PresenceStatus, Reaction, TeamInfo, TeamsSession, TeamsTokens,
    UserPresence,
};
pub use storage::TokenStorage;
