//! SSH transport: thin async wrapper over `russh`.
//!
//! Public surface:
//! - [`SshClient`]: connects + authenticates a remote host
//! - [`SshSession`]: a single PTY-backed shell channel
//! - [`SshConfig`], [`AuthMethod`], [`PtyConfig`], [`HostKeyPolicy`]
//! - [`SshEvent`]: events streamed back from the remote shell
//! - [`SshError`]: error type

mod client;
mod config;
mod error;
mod session;

pub use client::SshClient;
pub use config::{AuthMethod, HostKeyPolicy, PtyConfig, SshConfig, TofuPolicy};
pub use error::SshError;
pub use session::{SshEvent, SshSession};
