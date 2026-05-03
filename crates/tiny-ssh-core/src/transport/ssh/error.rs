use thiserror::Error;

#[derive(Debug, Error)]
pub enum SshError {
    #[error("connection failed: {0}")]
    Connect(#[source] russh::Error),

    #[error("authentication rejected by server")]
    AuthRejected,

    #[error("authentication error: {0}")]
    Auth(#[source] russh::Error),

    #[error("failed to load private key from {path}: {source}")]
    KeyLoad {
        path: std::path::PathBuf,
        #[source]
        source: russh_keys::Error,
    },

    #[error("failed to open channel: {0}")]
    ChannelOpen(#[source] russh::Error),

    #[error("PTY request failed: {0}")]
    Pty(#[source] russh::Error),

    #[error("shell request failed: {0}")]
    Shell(#[source] russh::Error),

    #[error("write failed: {0}")]
    Write(#[source] russh::Error),

    #[error("resize failed: {0}")]
    Resize(#[source] russh::Error),

    #[error("connect timed out after {0:?}")]
    ConnectTimeout(std::time::Duration),

    #[error("session is already closed")]
    Closed,

    #[error("ssh error: {0}")]
    Other(#[from] russh::Error),
}
