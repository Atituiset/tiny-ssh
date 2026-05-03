use std::sync::Arc;

use russh::client::{self, Handle, Handler};
use russh_keys::key::PublicKey;
use tokio::time::timeout;
use tracing::{debug, info};

use super::config::{AuthMethod, HostKeyPolicy, PtyConfig, SshConfig};
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
        let handler = ClientHandler::new(host_key_policy);
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
                .authenticate_password(user, pw)
                .await
                .map_err(SshError::Auth)?,
            AuthMethod::PublicKey { path, passphrase } => {
                let key = russh_keys::load_secret_key(&path, passphrase.as_deref()).map_err(
                    |source| SshError::KeyLoad {
                        path: path.clone(),
                        source,
                    },
                )?;
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
}

impl ClientHandler {
    fn new(policy: HostKeyPolicy) -> Self {
        Self { policy }
    }
}

#[async_trait::async_trait]
impl Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        match self.policy {
            HostKeyPolicy::AcceptAny => Ok(true),
        }
    }
}
