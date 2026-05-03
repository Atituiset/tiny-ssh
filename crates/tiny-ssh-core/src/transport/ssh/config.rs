use std::path::PathBuf;

/// How to verify the remote server's host key.
///
/// MVP only supports [`HostKeyPolicy::AcceptAny`] — a real `known_hosts`
/// implementation lands together with the secrets vault.
#[derive(Debug, Clone)]
pub enum HostKeyPolicy {
    /// Accept any host key. **Insecure**; intended for development.
    AcceptAny,
}

impl Default for HostKeyPolicy {
    fn default() -> Self {
        Self::AcceptAny
    }
}

/// How to authenticate to the remote.
#[derive(Debug, Clone)]
pub enum AuthMethod {
    Password(String),
    PublicKey {
        /// Path to a private key file (OpenSSH format).
        path: PathBuf,
        /// Optional passphrase for the key.
        passphrase: Option<String>,
    },
}

/// Connection parameters for a single SSH session.
#[derive(Debug, Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
    pub host_key_policy: HostKeyPolicy,
    /// Connection timeout. `None` = use the underlying default.
    pub connect_timeout: Option<std::time::Duration>,
}

impl SshConfig {
    pub fn new(host: impl Into<String>, user: impl Into<String>, auth: AuthMethod) -> Self {
        Self {
            host: host.into(),
            port: 22,
            user: user.into(),
            auth,
            host_key_policy: HostKeyPolicy::default(),
            connect_timeout: Some(std::time::Duration::from_secs(15)),
        }
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }
}

/// PTY parameters for an interactive shell channel.
#[derive(Debug, Clone)]
pub struct PtyConfig {
    pub term: String,
    pub cols: u16,
    pub rows: u16,
}

impl Default for PtyConfig {
    fn default() -> Self {
        Self {
            term: "xterm-256color".to_string(),
            cols: 80,
            rows: 24,
        }
    }
}
