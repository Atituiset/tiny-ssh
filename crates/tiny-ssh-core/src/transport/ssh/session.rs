use russh::{client::Msg, Channel, ChannelMsg};
use tracing::trace;

use super::error::SshError;

/// Events emitted by the remote shell.
#[derive(Debug)]
pub enum SshEvent {
    /// Stdout bytes (already past russh window-management).
    Data(Vec<u8>),
    /// Stderr bytes (`extended_data` with `ext == 1`).
    Stderr(Vec<u8>),
    /// Remote sent EOF on stdout.
    Eof,
    /// Remote process exited with the given status.
    ExitStatus(u32),
    /// Channel was closed by the peer.
    Closed,
}

/// One interactive shell channel — write user input, drain remote events.
///
/// Reads (`next_event`) take `&mut self`; writes (`write`/`resize`) take
/// `&self`. This lets a `tokio::select!` loop interleave both halves on a
/// single owned `SshSession`.
pub struct SshSession {
    channel: Channel<Msg>,
}

impl SshSession {
    pub(crate) fn new(channel: Channel<Msg>) -> Self {
        Self { channel }
    }

    /// Write user input bytes to the remote shell.
    pub async fn write(&self, data: &[u8]) -> Result<(), SshError> {
        self.channel.data(data).await.map_err(SshError::Write)
    }

    /// Notify the remote of a new terminal size.
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), SshError> {
        self.channel
            .window_change(u32::from(cols), u32::from(rows), 0, 0)
            .await
            .map_err(SshError::Resize)
    }

    /// Wait for the next remote event.
    ///
    /// Returns `None` once the channel has been closed and drained.
    pub async fn next_event(&mut self) -> Option<SshEvent> {
        loop {
            let msg = self.channel.wait().await?;
            trace!(target: "tiny_ssh::transport::ssh", ?msg, "channel msg");
            match msg {
                ChannelMsg::Data { data } => return Some(SshEvent::Data(data.to_vec())),
                ChannelMsg::ExtendedData { data, ext } if ext == 1 => {
                    return Some(SshEvent::Stderr(data.to_vec()));
                }
                ChannelMsg::Eof => return Some(SshEvent::Eof),
                ChannelMsg::ExitStatus { exit_status } => {
                    return Some(SshEvent::ExitStatus(exit_status));
                }
                ChannelMsg::Close => return Some(SshEvent::Closed),
                // Ignore window-adjust, request replies, exit-signal, etc.
                _ => continue,
            }
        }
    }

    /// Half-close the channel from our side.
    pub async fn close(self) -> Result<(), SshError> {
        self.channel.close().await.map_err(SshError::Other)
    }
}
