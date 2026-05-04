//! CLI-level integration tests for the VT emulation + App state machine.
//!
//! These tests drive the `App` end-to-end (session events + key input) and
//! inspect the resulting VT grid, cursor position, and OSC-derived state.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tiny_ssh_cli::*;
use tiny_ssh_core::{History, SessionEvent};

fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: mods,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }
}

/// Feed raw PTY bytes into the app and return the queued OSC events.
fn feed(app: &mut App, bytes: &[u8]) -> Vec<OscEvent> {
    app.on_session_event(SessionEvent::Output(bytes.to_vec()));
    app.terminal.take_osc_events()
}

/// Simulates typing a string with remote echo so the VT cursor stays in sync
/// with the shadow buffer.
fn type_str(app: &mut App, h: &History, s: &str) {
    for c in s.chars() {
        let action = app.on_key(key(KeyCode::Char(c), KeyModifiers::NONE), h);
        if let Action::Send(bytes) = action {
            app.terminal.feed(&bytes);
        }
    }
}

#[test]
fn vt_renders_color_and_clears_screen() {
    let mut app = App::new("h1".into(), "u".into(), 40, 10);
    // Clear screen + home cursor + green "ready" + reset + newline
    app.terminal.feed(b"\x1b[2J\x1b[H\x1b[32mready\x1b[0m\r\n");

    // After clearing and printing "ready", cursor should be at the start of
    // line 1 (after the \r\n).
    assert_eq!(app.terminal.cursor(), (0, 1));

    // Check that "ready" is on row 0.
    let content = app.terminal.renderable_content();
    let mut row0 = String::new();
    for indexed in content.display_iter {
        if indexed.point.line.0 == 0 {
            row0.push(indexed.cell.c);
        }
    }
    assert!(row0.trim_start().starts_with("ready"));
}

#[test]
fn alt_screen_toggle_is_tracked() {
    let mut app = App::new("h1".into(), "u".into(), 20, 5);
    assert!(!app.terminal.is_alt_screen());
    app.terminal.feed(b"\x1b[?1049h");
    assert!(app.terminal.is_alt_screen());
    app.terminal.feed(b"\x1b[?1049l");
    assert!(!app.terminal.is_alt_screen());
}

#[test]
fn osc_7_cwd_round_trip() {
    let mut app = App::new("h1".into(), "u".into(), 20, 5);
    feed(&mut app, b"\x1b]7;file:///home/user/work\x07");
    assert_eq!(app.cwd.as_deref(), Some(std::path::Path::new("/home/user/work")));
}

#[test]
fn osc_133_prompt_lifecycle() {
    let mut app = App::new("h1".into(), "u".into(), 20, 5);
    // PromptEnd sets prompt_pos and clears shadow.
    feed(&mut app, b"\x1b]133;B\x07");
    assert!(app.prompt_pos.is_some());
    assert_eq!(app.shadow_input, "");

    // CommandStart clears prompt_pos.
    feed(&mut app, b"\x1b]133;C\x07");
    assert!(app.prompt_pos.is_none());
}

#[test]
fn five_condition_gate_blocks_ghost_when_prompt_unknown() {
    let h = History::open_in_memory().unwrap();
    h.record(tiny_ssh_core::HistoryEntry {
        id: None,
        host: "h1".into(),
        user: "u".into(),
        cwd: None,
        command: "git status".into(),
        timestamp: 100,
        exit_code: None,
        duration_ms: None,
        source: tiny_ssh_core::HistorySource::User,
    })
    .unwrap();

    let mut app = App::new("h1".into(), "u".into(), 40, 10);
    // No prompt has been established.
    type_str(&mut app, &h, "gi");
    assert!(app.suggestion.is_some(), "suggestion computed");
    assert!(!app.can_show_suggestion(), "but ghost-text is gated off");
}

#[test]
fn five_condition_gate_allows_ghost_after_prompt() {
    let h = History::open_in_memory().unwrap();
    h.record(tiny_ssh_core::HistoryEntry {
        id: None,
        host: "h1".into(),
        user: "u".into(),
        cwd: None,
        command: "git status".into(),
        timestamp: 100,
        exit_code: None,
        duration_ms: None,
        source: tiny_ssh_core::HistorySource::User,
    })
    .unwrap();

    let mut app = App::new("h1".into(), "u".into(), 40, 10);
    feed(&mut app, b"\x1b]133;B\x07"); // establish prompt
    type_str(&mut app, &h, "gi");
    assert!(app.can_show_suggestion());
}

#[test]
fn heuristic_prompt_after_enter() {
    let h = History::open_in_memory().unwrap();
    let mut app = App::new("h1".into(), "u".into(), 40, 10);

    type_str(&mut app, &h, "echo");
    app.on_key(key(KeyCode::Enter, KeyModifiers::NONE), &h);
    assert!(app.last_input_was_enter);

    // Remote sends a prompt on a new line.
    app.on_session_event(SessionEvent::Output(b"\r\n$ ".to_vec()));
    assert!(app.prompt_pos.is_some(), "heuristic detected prompt");
}

#[test]
fn raw_passthrough_encoding() {
    let h = History::open_in_memory().unwrap();
    let mut app = App::new("h1".into(), "u".into(), 40, 10);

    let action = app.on_key(key(KeyCode::Char('a'), KeyModifiers::CONTROL), &h);
    assert_eq!(action, Action::Send(vec![0x01]));

    let action = app.on_key(key(KeyCode::Char('a'), KeyModifiers::ALT), &h);
    assert_eq!(action, Action::Send(b"\x1ba".to_vec()));

    let action = app.on_key(key(KeyCode::Enter, KeyModifiers::NONE), &h);
    assert_eq!(action, Action::Send(b"\r".to_vec()));

    let action = app.on_key(key(KeyCode::Up, KeyModifiers::NONE), &h);
    assert_eq!(action, Action::Send(b"\x1b[A".to_vec()));
}
