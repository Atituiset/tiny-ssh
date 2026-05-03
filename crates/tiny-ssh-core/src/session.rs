//! Session lifecycle manager.
//!
//! A [`SessionHandle`] is the user-facing object. Internally the manager spawns
//! a driver task that owns the underlying [`SshSession`] and bridges it to two
//! channels:
//!
//! - input: user → driver (typed bytes, resizes, disconnect requests)
//! - events: driver → user (state transitions, output bytes, errors)

use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info};

use crate::transport::ssh::{PtyConfig, SshClient, SshConfig, SshError, SshEvent, SshSession};

/// Process-unique identifier for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub u64);

impl SessionId {
    fn next() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "S{}", self.0)
    }
}

/// Coarse-grained session state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Connecting,
    Authenticated,
    ShellOpen,
    Closed,
    Failed(String),
}

/// Information about the connected host.
///
/// Populated lazily by Layer 1 probes (planned, not yet implemented).
#[derive(Debug, Clone, Default)]
pub struct HostFingerprint {
    pub uname: Option<String>,
    pub os_release: Option<String>,
    pub shell: Option<String>,
}

/// Static metadata about a session.
#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub id: SessionId,
    pub host: String,
    pub user: String,
    pub fingerprint: HostFingerprint,
}

/// Events emitted by a [`SessionHandle`] to the UI.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Lifecycle transition.
    StateChanged(SessionState),
    /// Bytes from the remote shell's stdout.
    Output(Vec<u8>),
    /// Bytes from the remote shell's stderr.
    Stderr(Vec<u8>),
    /// Remote process exited with this status.
    ExitStatus(u32),
    /// Non-fatal error reported by the driver.
    Error(String),
    /// Final event; the driver task has exited.
    Closed,
}

/// User → driver commands.
#[derive(Debug)]
enum SessionInput {
    Bytes(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Disconnect,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session driver has terminated")]
    Detached,
    #[error("ssh error: {0}")]
    Ssh(#[from] SshError),
    #[error("driver task panicked: {0}")]
    Panic(String),
}

/// Handle to a running session. Drop to abort the driver.
pub struct SessionHandle {
    meta: SessionMeta,
    input: mpsc::Sender<SessionInput>,
    events: mpsc::Receiver<SessionEvent>,
    driver: Option<JoinHandle<()>>,
}

impl SessionHandle {
    pub fn meta(&self) -> &SessionMeta {
        &self.meta
    }

    /// Send raw input bytes to the remote shell.
    pub async fn send_bytes(&self, data: Vec<u8>) -> Result<(), SessionError> {
        self.input
            .send(SessionInput::Bytes(data))
            .await
            .map_err(|_| SessionError::Detached)
    }

    /// Notify the remote of a new terminal size.
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), SessionError> {
        self.input
            .send(SessionInput::Resize { cols, rows })
            .await
            .map_err(|_| SessionError::Detached)
    }

    /// Ask the driver to half-close and exit gracefully.
    pub async fn disconnect(&self) -> Result<(), SessionError> {
        self.input
            .send(SessionInput::Disconnect)
            .await
            .map_err(|_| SessionError::Detached)
    }

    /// Receive the next event, or `None` if the driver has exited.
    pub async fn next_event(&mut self) -> Option<SessionEvent> {
        self.events.recv().await
    }

    /// Wait for the driver task to finish.
    pub async fn wait(&mut self) -> Result<(), SessionError> {
        if let Some(driver) = self.driver.take() {
            driver
                .await
                .map_err(|e| SessionError::Panic(e.to_string()))?;
        }
        Ok(())
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.driver.take() {
            handle.abort();
        }
    }
}

/// Spawn a new session driver and return a handle.
///
/// The driver task starts immediately and emits its first
/// [`SessionState::Connecting`] event before returning.
pub fn spawn(config: SshConfig, pty: PtyConfig) -> SessionHandle {
    let id = SessionId::next();
    let host = config.host.clone();
    let user = config.user.clone();
    let (input_tx, input_rx) = mpsc::channel(64);
    let (event_tx, event_rx) = mpsc::channel(256);

    let driver = tokio::spawn(driver_task(config, pty, input_rx, event_tx));

    let meta = SessionMeta {
        id,
        host,
        user,
        fingerprint: HostFingerprint::default(),
    };

    SessionHandle {
        meta,
        input: input_tx,
        events: event_rx,
        driver: Some(driver),
    }
}

async fn driver_task(
    config: SshConfig,
    pty: PtyConfig,
    mut input_rx: mpsc::Receiver<SessionInput>,
    event_tx: mpsc::Sender<SessionEvent>,
) {
    let host = config.host.clone();
    debug!(target: "tiny_ssh::session", %host, "driver starting");

    let _ = event_tx
        .send(SessionEvent::StateChanged(SessionState::Connecting))
        .await;

    let client = match SshClient::connect(config).await {
        Ok(c) => c,
        Err(e) => {
            let msg = e.to_string();
            let _ = event_tx.send(SessionEvent::Error(msg.clone())).await;
            let _ = event_tx
                .send(SessionEvent::StateChanged(SessionState::Failed(msg)))
                .await;
            let _ = event_tx.send(SessionEvent::Closed).await;
            return;
        }
    };
    let _ = event_tx
        .send(SessionEvent::StateChanged(SessionState::Authenticated))
        .await;

    let session = match client.open_shell(pty).await {
        Ok(s) => s,
        Err(e) => {
            let msg = e.to_string();
            let _ = event_tx.send(SessionEvent::Error(msg.clone())).await;
            let _ = event_tx
                .send(SessionEvent::StateChanged(SessionState::Failed(msg)))
                .await;
            let _ = client.disconnect().await;
            let _ = event_tx.send(SessionEvent::Closed).await;
            return;
        }
    };
    let _ = event_tx
        .send(SessionEvent::StateChanged(SessionState::ShellOpen))
        .await;

    let user_disconnected = io_loop(session, &mut input_rx, &event_tx).await;
    info!(target: "tiny_ssh::session", %host, user_disconnected, "io loop ended");

    let _ = client.disconnect().await;
    let _ = event_tx
        .send(SessionEvent::StateChanged(SessionState::Closed))
        .await;
    let _ = event_tx.send(SessionEvent::Closed).await;
}

/// Returns `true` iff the user explicitly requested disconnect.
async fn io_loop(
    mut session: SshSession,
    input_rx: &mut mpsc::Receiver<SessionInput>,
    event_tx: &mpsc::Sender<SessionEvent>,
) -> bool {
    loop {
        tokio::select! {
            cmd = input_rx.recv() => {
                match cmd {
                    None => return false,
                    Some(SessionInput::Bytes(data)) => {
                        if let Err(e) = session.write(&data).await {
                            let _ = event_tx
                                .send(SessionEvent::Error(e.to_string()))
                                .await;
                            return false;
                        }
                    }
                    Some(SessionInput::Resize { cols, rows }) => {
                        if let Err(e) = session.resize(cols, rows).await {
                            // resize failure is non-fatal; surface and continue
                            let _ = event_tx
                                .send(SessionEvent::Error(e.to_string()))
                                .await;
                        }
                    }
                    Some(SessionInput::Disconnect) => {
                        let _ = session.close().await;
                        return true;
                    }
                }
            }
            ev = session.next_event() => {
                match ev {
                    None => return false,
                    Some(SshEvent::Data(d)) => {
                        if event_tx.send(SessionEvent::Output(d)).await.is_err() {
                            return false;
                        }
                    }
                    Some(SshEvent::Stderr(d)) => {
                        if event_tx.send(SessionEvent::Stderr(d)).await.is_err() {
                            return false;
                        }
                    }
                    Some(SshEvent::Eof) => continue,
                    Some(SshEvent::ExitStatus(s)) => {
                        if event_tx.send(SessionEvent::ExitStatus(s)).await.is_err() {
                            return false;
                        }
                    }
                    Some(SshEvent::Closed) => return false,
                }
            }
        }
    }
}
