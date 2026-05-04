use std::sync::Arc;

use russh::client::{self, Handle, Handler};
use russh_keys::key::PublicKey;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

use super::config::{AuthMethod, HostKeyPolicy, PtyConfig, SshConfig, TofuPolicy};
use super::error::SshError;
use super::session::SshSession;

/// Authenticated SSH connection. Drop or call [`SshClient::disconnect`] to close.
pub struct SshClient {
    handle: Handle<ClientHandler>,
}

impl SshClient {
    /// Establish a TCP connection, authenticate, and return a usable client.
    pub async fn connect(config: SshConfig) -> Result<Self, SshError> {
        let SshConfig {
            host,
            port,
            user,
            auth,
            host_key_policy,
            connect_timeout,
        } = config;

        info!(target: "tiny_ssh::transport::ssh", host, port, user, "connecting");

        let russh_config = Arc::new(client::Config::default());
        let handler = ClientHandler::new(host_key_policy, host.clone(), port);
        let addr = (host.as_str(), port);

        let connect_fut = client::connect(russh_config, addr, handler);
        let mut handle = match connect_timeout {
            Some(d) => timeout(d, connect_fut)
                .await
                .map_err(|_| SshError::ConnectTimeout(d))?
                .map_err(SshError::Connect)?,
            None => connect_fut.await.map_err(SshError::Connect)?,
        };

        Self::authenticate(&mut handle, &user, auth).await?;
        info!(target: "tiny_ssh::transport::ssh", "authenticated");

        Ok(Self { handle })
    }

    async fn authenticate(
        handle: &mut Handle<ClientHandler>,
        user: &str,
        auth: AuthMethod,
    ) -> Result<(), SshError> {
        let accepted = match auth {
            AuthMethod::Password(pw) => handle
                .authenticate_password(user, (*pw).clone())
                .await
                .map_err(SshError::Auth)?,
            AuthMethod::PublicKey { path, passphrase } => {
                let key = russh_keys::load_secret_key(
                    &path,
                    passphrase.as_ref().map(|z| z.as_str()),
                )
                .map_err(|source| SshError::KeyLoad {
                    path: path.clone(),
                    source,
                })?;
                handle
                    .authenticate_publickey(user, Arc::new(key))
                    .await
                    .map_err(SshError::Auth)?
            }
        };

        if accepted {
            Ok(())
        } else {
            Err(SshError::AuthRejected)
        }
    }

    /// Open an interactive shell channel with a PTY.
    pub async fn open_shell(&self, pty: PtyConfig) -> Result<SshSession, SshError> {
        let channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(SshError::ChannelOpen)?;

        channel
            .request_pty(
                false,
                &pty.term,
                u32::from(pty.cols),
                u32::from(pty.rows),
                0,
                0,
                &[],
            )
            .await
            .map_err(SshError::Pty)?;

        channel.request_shell(true).await.map_err(SshError::Shell)?;
        debug!(target: "tiny_ssh::transport::ssh", term = %pty.term, cols = pty.cols, rows = pty.rows, "shell opened");

        Ok(SshSession::new(channel))
    }

    /// Cleanly close the SSH transport.
    pub async fn disconnect(self) -> Result<(), SshError> {
        self.handle
            .disconnect(russh::Disconnect::ByApplication, "bye", "")
            .await?;
        Ok(())
    }
}

struct ClientHandler {
    policy: HostKeyPolicy,
    host: String,
    port: u16,
}

impl ClientHandler {
    fn new(policy: HostKeyPolicy, host: String, port: u16) -> Self {
        Self { policy, host, port }
    }
}

#[async_trait::async_trait]
impl Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        match &self.policy {
            HostKeyPolicy::AcceptAny => {
                warn!(
                    target: "tiny_ssh::transport::ssh",
                    host = %self.host,
                    port = self.port,
                    "host key policy is AcceptAny — connection is NOT authenticated against a known_hosts file"
                );
                Ok(true)
            }
            HostKeyPolicy::KnownHosts { path, on_unknown } => {
                // Pass 1: read-only check against the user's OpenSSH known_hosts.
                // A match here is sufficient to trust the host. A KeyChanged
                // error here is *also* fatal — the user trusts that file with
                // OpenSSH, so a mismatch is just as serious. Other errors
                // (e.g. file missing) just mean "no opinion", continue.
                match russh_keys::check_known_hosts(&self.host, self.port, server_public_key) {
                    Ok(true) => {
                        debug!(
                            target: "tiny_ssh::transport::ssh",
                            host = %self.host,
                            "host key matched ~/.ssh/known_hosts"
                        );
                        return Ok(true);
                    }
                    Err(russh_keys::Error::KeyChanged { line }) => {
                        error!(
                            target: "tiny_ssh::transport::ssh",
                            host = %self.host,
                            port = self.port,
                            line,
                            "host key MISMATCH against ~/.ssh/known_hosts — possible MITM"
                        );
                        return Err(russh::Error::KeyChanged { line });
                    }
                    Ok(false) | Err(_) => {} // not present / unreadable; fall through
                }

                // Pass 2: tssh's own known_hosts.
                match russh_keys::check_known_hosts_path(
                    &self.host,
                    self.port,
                    server_public_key,
                    path,
                ) {
                    Ok(true) => {
                        debug!(
                            target: "tiny_ssh::transport::ssh",
                            host = %self.host,
                            path = %path.display(),
                            "host key matched tssh known_hosts"
                        );
                        Ok(true)
                    }
                    Err(russh_keys::Error::KeyChanged { line }) => {
                        error!(
                            target: "tiny_ssh::transport::ssh",
                            host = %self.host,
                            port = self.port,
                            path = %path.display(),
                            line,
                            "host key MISMATCH against tssh known_hosts — possible MITM"
                        );
                        Err(russh::Error::KeyChanged { line })
                    }
                    Ok(false) | Err(_) => {
                        // Unknown host. Either learn it or refuse.
                        match on_unknown {
                            TofuPolicy::AcceptOnFirst => {
                                if let Err(e) = russh_keys::learn_known_hosts_path(
                                    &self.host,
                                    self.port,
                                    server_public_key,
                                    path,
                                ) {
                                    warn!(
                                        target: "tiny_ssh::transport::ssh",
                                        host = %self.host,
                                        path = %path.display(),
                                        error = %e,
                                        "failed to record new host key — continuing without persisting"
                                    );
                                } else {
                                    info!(
                                        target: "tiny_ssh::transport::ssh",
                                        host = %self.host,
                                        port = self.port,
                                        path = %path.display(),
                                        "trusted new host on first contact (TOFU)"
                                    );
                                }
                                Ok(true)
                            }
                            TofuPolicy::RejectUnknown => {
                                error!(
                                    target: "tiny_ssh::transport::ssh",
                                    host = %self.host,
                                    port = self.port,
                                    "host not in known_hosts and policy is RejectUnknown"
                                );
                                Err(russh::Error::UnknownKey)
                            }
                        }
                    }
                }
            }
        }
    }
}
