//! tiny-ssh-core: cross-platform SSH/DB client core.
//!
//! Layered modules:
//! - [`transport`]: protocol-level clients (SSH today, DB later)
//! - [`session`]: lifecycle and event flow for an active connection
//! - [`history`]: persistent command history backed by SQLite
//! - [`suggest`]: layered suggestion engine
//! - [`secrets`]: credential storage abstraction (placeholder)

pub mod history;
pub mod secrets;
pub mod session;
pub mod suggest;
pub mod transport;

pub use history::{History, HistoryEntry, HistoryError, HistoryId, HistorySource, Suggestion, SuggestContext};
pub use session::{
    spawn as spawn_session, HostFingerprint, SessionError, SessionEvent, SessionHandle, SessionId,
    SessionMeta, SessionState,
};
pub use suggest::{EngineSuggestion, Provenance, SuggestEngine};
pub use transport::ssh::{
    AuthMethod, HostKeyPolicy, PtyConfig, SshClient, SshConfig, SshError, SshEvent, SshSession,
};
