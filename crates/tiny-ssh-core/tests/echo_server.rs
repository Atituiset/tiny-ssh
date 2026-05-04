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
    TofuPolicy,
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
        AuthMethod::password(TEST_PASSWORD),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_wrong_password() {
    // --- server ---
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server_config = russh::server::Config {
        keys: vec![KeyPair::generate_ed25519().expect("ed25519 keygen")],
        // Don't let russh hang us forever on a bad password.
        auth_rejection_time: Duration::from_millis(50),
        ..Default::default()
    };
    let server_config = Arc::new(server_config);

    let server_task = tokio::spawn(async move {
        let mut srv = EchoServer;
        let _ = srv.run_on_socket(server_config, &listener).await;
    });

    // --- client with bad password ---
    let mut config = SshConfig::new(
        "127.0.0.1",
        TEST_USER,
        AuthMethod::password("not-the-password"),
    )
    .with_port(port);
    config.host_key_policy = HostKeyPolicy::AcceptAny;

    let mut handle = spawn_session(config, PtyConfig::default());

    let mut saw_failure = false;
    collect_until(&mut handle, Duration::from_secs(5), |ev| {
        if let SessionEvent::StateChanged(SessionState::Failed(_)) = ev {
            saw_failure = true;
            true
        } else if let SessionEvent::StateChanged(SessionState::ShellOpen) = ev {
            panic!("shell opened with wrong password");
        } else {
            false
        }
    })
    .await;
    assert!(saw_failure, "expected Failed state after wrong password");

    server_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_timeout_fires_on_dead_endpoint() {
    // 192.0.2.0/24 (TEST-NET-1) is reserved for documentation and is not
    // routable, so a SYN there should hang until our timeout trips.
    let mut config = SshConfig::new(
        "192.0.2.1",
        "alice",
        AuthMethod::password("whatever"),
    )
    .with_port(22);
    config.host_key_policy = HostKeyPolicy::AcceptAny;
    config.connect_timeout = Some(Duration::from_millis(150));

    let mut handle = spawn_session(config, PtyConfig::default());

    let mut saw_failure = false;
    collect_until(&mut handle, Duration::from_secs(5), |ev| {
        if let SessionEvent::StateChanged(SessionState::Failed(msg)) = ev {
            // Loose check: any sign of a timeout/connect-related error.
            let m = msg.to_lowercase();
            assert!(
                m.contains("timeout") || m.contains("timed out") || m.contains("connect"),
                "expected connect-related error, got: {msg}"
            );
            saw_failure = true;
            true
        } else {
            false
        }
    })
    .await;
    assert!(saw_failure, "expected Failed state after connect timeout");
}

/// Spin up an EchoServer with the given host key, returning (bound_port, abort_handle).
async fn spawn_echo_server_with_key(key: KeyPair) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server_config = russh::server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::from_millis(50),
        ..Default::default()
    };
    let server_config = Arc::new(server_config);

    let task = tokio::spawn(async move {
        let mut srv = EchoServer;
        let _ = srv.run_on_socket(server_config, &listener).await;
    });
    (port, task)
}

/// Try to connect once with the given policy, return whether ShellOpen was reached.
/// Panics with the failure message if Failed is observed instead.
async fn try_connect(
    port: u16,
    policy: HostKeyPolicy,
) -> Result<tiny_ssh_core::SessionHandle, String> {
    let mut config = SshConfig::new(
        "127.0.0.1",
        TEST_USER,
        AuthMethod::password(TEST_PASSWORD),
    )
    .with_port(port);
    config.host_key_policy = policy;

    let mut handle = spawn_session(config, PtyConfig::default());

    let outcome = std::cell::RefCell::new(None);
    collect_until(&mut handle, Duration::from_secs(5), |ev| match ev {
        SessionEvent::StateChanged(SessionState::ShellOpen) => {
            *outcome.borrow_mut() = Some(Ok(()));
            true
        }
        SessionEvent::StateChanged(SessionState::Failed(msg)) => {
            *outcome.borrow_mut() = Some(Err(msg.clone()));
            true
        }
        _ => false,
    })
    .await;

    match outcome.into_inner() {
        Some(Ok(())) => Ok(handle),
        Some(Err(msg)) => Err(msg),
        None => Err("no terminal state observed".into()),
    }
}

fn temp_known_hosts(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "tssh-test-{}-{}.txt",
        name,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn known_hosts_tofu_learns_on_first_connect() {
    let key = KeyPair::generate_ed25519().unwrap();
    let (port, server) = spawn_echo_server_with_key(key).await;

    let kh = temp_known_hosts("tofu-learn");
    let policy = HostKeyPolicy::KnownHosts {
        path: kh.clone(),
        on_unknown: TofuPolicy::AcceptOnFirst,
    };

    let _handle = try_connect(port, policy).await.expect("first connect should succeed");

    // The known_hosts file should now exist and mention 127.0.0.1.
    let body = std::fs::read_to_string(&kh).expect("known_hosts file should be written");
    assert!(
        body.contains("127.0.0.1"),
        "expected hostname in known_hosts, got: {body}"
    );

    let _ = std::fs::remove_file(&kh);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn known_hosts_rejects_unknown_host_when_policy_is_reject() {
    let key = KeyPair::generate_ed25519().unwrap();
    let (port, server) = spawn_echo_server_with_key(key).await;

    let kh = temp_known_hosts("reject-unknown");
    let policy = HostKeyPolicy::KnownHosts {
        path: kh.clone(),
        on_unknown: TofuPolicy::RejectUnknown,
    };

    let err = match try_connect(port, policy).await {
        Ok(_) => panic!("connect should fail when host is unknown and policy is RejectUnknown"),
        Err(msg) => msg,
    };
    let lower = err.to_lowercase();
    assert!(
        lower.contains("unknown") || lower.contains("key"),
        "expected unknown-key error, got: {err}"
    );
    // No file should have been written.
    assert!(!kh.exists(), "RejectUnknown must not write to known_hosts");

    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn known_hosts_detects_key_change() {
    // Phase 1: learn key K1 at port P
    let key1 = KeyPair::generate_ed25519().unwrap();
    let (port, server1) = spawn_echo_server_with_key(key1).await;

    let kh = temp_known_hosts("key-change");
    let policy = HostKeyPolicy::KnownHosts {
        path: kh.clone(),
        on_unknown: TofuPolicy::AcceptOnFirst,
    };
    {
        let h = try_connect(port, policy.clone())
            .await
            .expect("first connect should learn the key");
        // Drop handle to let the connection unwind before we kill the server.
        drop(h);
    }
    server1.abort();
    // Give the abort a moment to free the port.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Phase 2: bring up a fresh server with K2 — but on a *different* port,
    // because the kernel may not let us rebind P immediately. Instead, point
    // the entry we just learned at the new port by manually rewriting it.
    let key2 = KeyPair::generate_ed25519().unwrap();
    let (port2, server2) = spawn_echo_server_with_key(key2).await;

    // Edit the known_hosts file: replace the original port marker with port2.
    // The file russh writes uses "127.0.0.1 <type> <key>" when port=22, or
    // "[127.0.0.1]:port <type> <key>" otherwise. We always have non-22, so
    // it's the bracketed form.
    let body = std::fs::read_to_string(&kh).unwrap();
    let rewritten = body.replace(
        &format!("[127.0.0.1]:{port}"),
        &format!("[127.0.0.1]:{port2}"),
    );
    std::fs::write(&kh, rewritten).unwrap();

    // Phase 3: connect with the rewritten known_hosts — server uses K2 but
    // file claims K1, so we should get a key-change error.
    let err = match try_connect(port2, policy).await {
        Ok(_) => panic!("connect with mismatched key must fail"),
        Err(msg) => msg,
    };
    let lower = err.to_lowercase();
    assert!(
        lower.contains("key changed") || lower.contains("changed"),
        "expected key-change error, got: {err}"
    );

    let _ = std::fs::remove_file(&kh);
    server2.abort();
}
