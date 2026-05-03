//! tiny-ssh CLI/TUI entry point.

mod ansi;
mod app;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use crossterm::{
    event::{Event, EventStream, KeyEvent, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use ratatui::{prelude::CrosstermBackend, Terminal};
use tiny_ssh_core::{
    spawn_session, AuthMethod, History, PtyConfig, SessionEvent, SessionHandle, SshConfig,
};
use tracing::{debug, error};

use crate::app::{Action, App};

/// tiny-ssh: a tiny cross-platform SSH client with history-based autosuggest.
#[derive(Parser, Debug)]
#[command(name = "tssh", version, about)]
struct Cli {
    /// Target as `user@host[:port]`.
    target: String,

    /// Override port (1..=65535).
    #[arg(short = 'p', long)]
    port: Option<u16>,

    /// Path to a private key file. If omitted, password auth is used.
    #[arg(short = 'i', long)]
    key: Option<PathBuf>,

    /// Read the SSH password from this environment variable instead of prompting.
    #[arg(long, value_name = "VAR")]
    password_env: Option<String>,

    /// Read a key passphrase from this environment variable instead of prompting.
    #[arg(long, value_name = "VAR")]
    passphrase_env: Option<String>,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let (user, host, port_from_target) = parse_target(&cli.target)?;
    let port = cli.port.or(port_from_target).unwrap_or(22);

    let auth = build_auth(&user, &host, &cli)?;

    let history =
        History::open_default().context("failed to open history database")?;

    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let pty = PtyConfig {
        term: "xterm-256color".to_string(),
        cols,
        rows,
    };

    let config = SshConfig::new(host.clone(), user.clone(), auth).with_port(port);
    let handle = spawn_session(config, pty);
    debug!("session spawned");

    run_tui(handle, history, host, user).await
}

fn init_tracing() {
    // Logs go to stderr only; the TUI owns stdout. Default to off unless RUST_LOG is set.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .init();
}

fn parse_target(s: &str) -> Result<(String, String, Option<u16>)> {
    let (user, rest) = s
        .split_once('@')
        .with_context(|| format!("target must be `user@host[:port]`, got `{s}`"))?;
    if user.is_empty() {
        bail!("user portion of target cannot be empty");
    }

    if let Some((host, port_s)) = rest.rsplit_once(':') {
        // Allow IPv6 in brackets. `[::1]:22`
        if rest.starts_with('[') {
            // bracketed IPv6 literal
            let end = rest
                .find(']')
                .with_context(|| "unbalanced [ in IPv6 host")?;
            let host = &rest[1..end];
            let port_s = rest[end + 1..].strip_prefix(':');
            let port = match port_s {
                Some(p) => Some(p.parse::<u16>().context("invalid port")?),
                None => None,
            };
            return Ok((user.to_string(), host.to_string(), port));
        }
        let port = port_s.parse::<u16>().context("invalid port")?;
        Ok((user.to_string(), host.to_string(), Some(port)))
    } else {
        Ok((user.to_string(), rest.to_string(), None))
    }
}

fn build_auth(user: &str, host: &str, cli: &Cli) -> Result<AuthMethod> {
    if let Some(path) = &cli.key {
        let passphrase = match &cli.passphrase_env {
            Some(var) => std::env::var(var).ok().filter(|s| !s.is_empty()),
            None => None,
        };
        return Ok(AuthMethod::PublicKey {
            path: path.clone(),
            passphrase,
        });
    }
    let pw = match &cli.password_env {
        Some(var) => std::env::var(var).with_context(|| format!("env var `{var}` is not set"))?,
        None => rpassword::prompt_password(format!("password for {user}@{host}: "))
            .context("failed to read password")?,
    };
    Ok(AuthMethod::Password(pw))
}

async fn run_tui(
    mut handle: SessionHandle,
    history: History,
    host: String,
    user: String,
) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let result = drive(&mut terminal, &mut handle, &history, host, user).await;
    teardown_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend).context("init terminal")?;
    Ok(terminal)
}

fn teardown_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    Ok(())
}

async fn drive(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    handle: &mut SessionHandle,
    history: &History,
    host: String,
    user: String,
) -> Result<()> {
    let mut app = App::new(host, user);
    let mut events = EventStream::new();
    let mut redraw = true;

    loop {
        if redraw {
            terminal.draw(|f| ui::render(f, &app))?;
            redraw = false;
        }

        tokio::select! {
            biased;
            maybe_term_ev = events.next() => match maybe_term_ev {
                Some(Ok(ev)) => {
                    if handle_terminal_event(ev, &mut app, history, handle).await? {
                        return Ok(());
                    }
                    redraw = true;
                }
                Some(Err(e)) => {
                    error!(error = %e, "terminal event error");
                }
                None => {
                    debug!("terminal event stream ended");
                    return Ok(());
                }
            },
            maybe_session_ev = handle.next_event() => match maybe_session_ev {
                Some(ev) => {
                    let was_closed = matches!(ev, SessionEvent::Closed);
                    app.on_session_event(ev);
                    redraw = true;
                    if was_closed {
                        // Drain remaining events, then exit.
                        finalize_after_close(terminal, &mut app)?;
                        // brief pause so the user can read the final state
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        return Ok(());
                    }
                }
                None => {
                    debug!("session event stream ended");
                    return Ok(());
                }
            },
        }
    }
}

async fn handle_terminal_event(
    ev: Event,
    app: &mut App,
    history: &History,
    handle: &SessionHandle,
) -> Result<bool> {
    match ev {
        Event::Key(KeyEvent {
            kind: KeyEventKind::Release,
            ..
        }) => Ok(false),
        Event::Key(key) => {
            let action = app.on_key(key, history);
            match action {
                Action::Send(bytes) => {
                    if let Err(e) = handle.send_bytes(bytes).await {
                        app.last_error = Some(e.to_string());
                    }
                    Ok(false)
                }
                Action::Quit => {
                    let _ = handle.disconnect().await;
                    Ok(true)
                }
                Action::None => Ok(false),
            }
        }
        Event::Resize(cols, rows) => {
            let _ = handle.resize(cols, rows).await;
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn finalize_after_close(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    terminal.draw(|f| ui::render(f, app))?;
    Ok(())
}
