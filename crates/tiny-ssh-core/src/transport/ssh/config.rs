use std::fmt;
use std::path::PathBuf;

use directories::ProjectDirs;
use zeroize::Zeroizing;

/// What to do when we see a server we've never seen before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TofuPolicy {
    /// Trust on first use: accept the new host's key and append it to the
    /// known_hosts file. Subsequent connects must match.
    AcceptOnFirst,
    /// Refuse to connect to any host that isn't already trusted.
    RejectUnknown,
}

/// How to verify the remote server's host key.
#[derive(Debug, Clone)]
pub enum HostKeyPolicy {
    /// Verify the server key against a known_hosts-format file. The user's
    /// `~/.ssh/known_hosts` is also consulted (read-only) so hosts already
    /// trusted by OpenSSH need no re-trust.
    KnownHosts {
        path: PathBuf,
        on_unknown: TofuPolicy,
    },
    /// Accept any host key. **Insecure**; intended only for tests / dev.
    AcceptAny,
}

impl HostKeyPolicy {
    /// Default policy for production use: TOFU against the per-user tssh
    /// known_hosts file, with `~/.ssh/known_hosts` as a read-only fallback.
    /// If we can't determine a data dir, falls back to [`HostKeyPolicy::AcceptAny`].
    pub fn default_known_hosts() -> Self {
        match default_known_hosts_path() {
            Some(path) => Self::KnownHosts {
                path,
                on_unknown: TofuPolicy::AcceptOnFirst,
            },
            None => Self::AcceptAny,
        }
    }
}

impl Default for HostKeyPolicy {
    fn default() -> Self {
        Self::default_known_hosts()
    }
}

fn default_known_hosts_path() -> Option<PathBuf> {
    ProjectDirs::from("io", "tinyssh", "tssh").map(|d| d.data_dir().join("known_hosts"))
}

/// How to authenticate to the remote.
///
/// Secret material is wrapped in [`Zeroizing`] so the in-memory buffer is
/// scrubbed on drop. Note that handing the secret to the underlying SSH
/// implementation will cause it to be copied into buffers we don't control;
/// zeroization is best-effort at our boundary, not end-to-end.
#[derive(Clone)]
pub enum AuthMethod {
    Password(Zeroizing<String>),
    PublicKey {
        /// Path to a private key file (OpenSSH format).
        path: PathBuf,
        /// Optional passphrase for the key.
        passphrase: Option<Zeroizing<String>>,
    },
}

impl fmt::Debug for AuthMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password(_) => f.debug_tuple("Password").field(&"<redacted>").finish(),
            Self::PublicKey { path, passphrase } => f
                .debug_struct("PublicKey")
                .field("path", path)
                .field(
                    "passphrase",
                    &passphrase.as_ref().map(|_| "<redacted>"),
                )
                .finish(),
        }
    }
}

impl AuthMethod {
    /// Construct a password auth method. The password is wrapped in
    /// [`Zeroizing`] internally and scrubbed when this value is dropped.
    pub fn password(pw: impl Into<String>) -> Self {
        Self::Password(Zeroizing::new(pw.into()))
    }

    /// Construct a public-key auth method. If supplied, the passphrase is
    /// wrapped in [`Zeroizing`] internally.
    pub fn public_key(path: impl Into<PathBuf>, passphrase: Option<String>) -> Self {
        Self::PublicKey {
            path: path.into(),
            passphrase: passphrase.map(Zeroizing::new),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "hunter2-very-secret";

    #[test]
    fn debug_password_does_not_leak_plaintext() {
        let auth = AuthMethod::password(SECRET);
        let s = format!("{auth:?}");
        assert!(!s.contains(SECRET), "Debug leaked secret: {s}");
        assert!(s.contains("redacted"), "expected redaction marker, got: {s}");
    }

    #[test]
    fn debug_passphrase_does_not_leak_plaintext() {
        let auth = AuthMethod::public_key(PathBuf::from("/tmp/key"), Some(SECRET.into()));
        let s = format!("{auth:?}");
        assert!(!s.contains(SECRET), "Debug leaked passphrase: {s}");
        assert!(s.contains("redacted"), "expected redaction marker, got: {s}");
    }

    #[test]
    fn debug_via_ssh_config_does_not_leak_plaintext() {
        // Catches regressions where Debug on SshConfig might bypass our impl.
        let cfg = SshConfig::new("example.com", "u", AuthMethod::password(SECRET));
        let s = format!("{cfg:?}");
        assert!(!s.contains(SECRET), "SshConfig Debug leaked secret: {s}");
    }

    #[test]
    fn public_key_without_passphrase_debug() {
        let auth = AuthMethod::public_key(PathBuf::from("/tmp/k"), None);
        let s = format!("{auth:?}");
        assert!(s.contains("None"), "expected None marker for absent passphrase: {s}");
    }
}
