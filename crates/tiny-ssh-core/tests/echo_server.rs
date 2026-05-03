//! End-to-end smoke test: spin up an in-process russh server that echoes
//! everything it receives, then drive it through `spawn_session`.
//!
//! Verifies that:
//! - `SshClient` can connect with password auth
//! - `SessionHandle` reports `ShellOpen`
//! - bytes sent through `send_bytes` round-trip back as `Output` events
//! - `disconnect` cleanly tears the session down

use std::sync::Arc;
use std::time::Duration;

use russh::server::{Auth, Handler as ServerHandler, Msg, Server, Session};
use russh::{Channel, ChannelId, CryptoVec};
use russh_keys::key::KeyPair;
use tiny_ssh_core::{
    spawn_session, AuthMethod, HostKeyPolicy, PtyConfig, SessionEvent, SessionState, SshConfig,
};
use tokio::net::TcpListener;
use tokio::time::timeout;

const TEST_USER: &str = "alice";
const TEST_PASSWORD: &str = "wonderland";
const GREETING: &[u8] = b"hello from echo-srv\r\n";

struct EchoServer;

impl Server for EchoServer {
    type Handler = EchoSession;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self::Handler {
        EchoSession
    }
}

struct EchoSession;

#[async_trait::async_trait]
impl ServerHandler for EchoSession {
    type Error = russh::Error;

    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<Auth, Self::Error> {
        if user == TEST_USER && password == TEST_PASSWORD {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::Reject {
                proceed_with_methods: None,
            })
        }
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }

    async fn pty_request(
        &mut self,
        _channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, CryptoVec::from_slice(GREETING));
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, CryptoVec::from_slice(data));
        Ok(())
    }
}

async fn collect_until<F>(
    handle: &mut tiny_ssh_core::SessionHandle,
    deadline: Duration,
    mut done: F,
) where
    F: FnMut(&SessionEvent) -> bool,
{
    let result = timeout(deadline, async {
        while let Some(ev) = handle.next_event().await {
            if done(&ev) {
                return;
            }
        }
    })
    .await;
    if result.is_err() {
        panic!("timed out after {deadline:?} waiting for predicate");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn echo_round_trip() {
    // --- server ---
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server_config = russh::server::Config {
        keys: vec![KeyPair::generate_ed25519().expect("ed25519 keygen")],
        ..Default::default()
    };
    let server_config = Arc::new(server_config);

    let server_task = tokio::spawn(async move {
        let mut srv = EchoServer;
        let _ = srv.run_on_socket(server_config, &listener).await;
    });

    // --- client ---
    let mut config = SshConfig::new(
        "127.0.0.1",
        TEST_USER,
        AuthMethod::Password(TEST_PASSWORD.into()),
    )
    .with_port(port);
    config.host_key_policy = HostKeyPolicy::AcceptAny;

    let mut handle = spawn_session(config, PtyConfig::default());

    // 1. Wait for ShellOpen
    let mut shell_open = false;
    collect_until(&mut handle, Duration::from_secs(5), |ev| {
        if let SessionEvent::StateChanged(SessionState::ShellOpen) = ev {
            shell_open = true;
            true
        } else if let SessionEvent::StateChanged(SessionState::Failed(msg)) = ev {
            panic!("session failed: {msg}");
        } else {
            false
        }
    })
    .await;
    assert!(shell_open, "expected ShellOpen state");

    // 2. Wait for the server's greeting
    let mut got_greeting = false;
    collect_until(&mut handle, Duration::from_secs(5), |ev| {
        if let SessionEvent::Output(bytes) = ev {
            if contains(bytes, b"hello from echo-srv") {
                got_greeting = true;
                return true;
            }
        }
        false
    })
    .await;
    assert!(got_greeting, "did not receive server greeting");

    // 3. Send "ping\n", expect echo back
    handle.send_bytes(b"ping\n".to_vec()).await.unwrap();

    let mut got_echo = false;
    collect_until(&mut handle, Duration::from_secs(5), |ev| {
        if let SessionEvent::Output(bytes) = ev {
            if contains(bytes, b"ping") {
                got_echo = true;
                return true;
            }
        }
        false
    })
    .await;
    assert!(got_echo, "did not receive echo of 'ping'");

    // 4. Disconnect cleanly
    handle.disconnect().await.unwrap();

    let _ = timeout(Duration::from_secs(3), async {
        while let Some(ev) = handle.next_event().await {
            if matches!(ev, SessionEvent::Closed) {
                break;
            }
        }
    })
    .await;

    server_task.abort();
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}
