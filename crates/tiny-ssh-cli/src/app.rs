//! TUI application state.
//!
//! The App owns:
//! - rolling output buffer (line-mode, ANSI-stripped)
//! - current input line + cursor + autosuggestion
//! - last-seen session state
//!
//! It is driven by two event sources (crossterm + session) wired up in
//! `main::run`.

use std::collections::VecDeque;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tiny_ssh_core::{
    EngineSuggestion, History, HistoryEntry, HistorySource, SessionEvent, SessionState,
    SuggestContext, SuggestEngine,
};

use crate::ansi;

const MAX_OUTPUT_LINES: usize = 5_000;

/// What [`App::on_key`] wants the driver to do.
#[derive(Debug)]
pub enum Action {
    /// Send these bytes to the remote shell.
    Send(Vec<u8>),
    /// Disconnect cleanly and exit.
    Quit,
    /// No-op (UI-only state change).
    None,
}

pub struct App {
    pub host: String,
    pub user: String,
    pub state: SessionState,

    /// ANSI-stripped output, one line per entry. Newest at the back.
    pub output: VecDeque<String>,
    /// Whether the driver task has fully exited.
    pub closed: bool,
    /// Last error message, if any.
    pub last_error: Option<String>,

    /// Current input buffer (what the user has typed since last Enter).
    pub input: String,
    /// Cursor position within `input` (in chars, not bytes).
    pub cursor: usize,
    /// Cached autosuggestion (just the suffix beyond `input`).
    pub suggestion: Option<String>,

    /// Did the user accept a suggestion since last keystroke?
    /// Used to tag the next history record's source.
    last_action_was_suggestion: bool,
}

impl App {
    pub fn new(host: String, user: String) -> Self {
        Self {
            host,
            user,
            state: SessionState::Connecting,
            output: VecDeque::new(),
            closed: false,
            last_error: None,
            input: String::new(),
            cursor: 0,
            suggestion: None,
            last_action_was_suggestion: false,
        }
    }

    /// Apply an event from the session driver.
    pub fn on_session_event(&mut self, ev: SessionEvent) {
        match ev {
            SessionEvent::StateChanged(s) => self.state = s,
            SessionEvent::Output(bytes) | SessionEvent::Stderr(bytes) => {
                self.append_output(&bytes);
            }
            SessionEvent::ExitStatus(_) => {}
            SessionEvent::Error(msg) => self.last_error = Some(msg),
            SessionEvent::Closed => self.closed = true,
        }
    }

    /// Recompute the autosuggestion against the persistent history.
    pub fn refresh_suggestion(&mut self, history: &History) {
        if self.input.is_empty() {
            self.suggestion = None;
            return;
        }
        let engine = SuggestEngine::new(history);
        let ctx = SuggestContext {
            host: &self.host,
            cwd: None,
            prefix: &self.input,
        };
        match engine.suggest(&ctx) {
            Ok(Some(EngineSuggestion { command, .. })) => {
                if command.starts_with(&self.input) && command.len() > self.input.len() {
                    self.suggestion = Some(command[self.input.len()..].to_string());
                } else {
                    self.suggestion = None;
                }
            }
            _ => self.suggestion = None,
        }
    }

    /// Apply a key event. Returns the [`Action`] to take.
    pub fn on_key(&mut self, key: KeyEvent, history: &History) -> Action {
        // Ctrl+Q quits the app even if remote is unresponsive.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
            return Action::Quit;
        }

        match key.code {
            KeyCode::Enter => self.submit_line(history),
            KeyCode::Tab | KeyCode::Right => self.accept_suggestion(history),
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    let byte_idx = self.char_to_byte(self.cursor);
                    self.input.remove(byte_idx);
                    self.last_action_was_suggestion = false;
                    self.refresh_suggestion(history);
                }
                Action::None
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                Action::None
            }
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.handle_ctrl_char(c)
                } else {
                    self.insert_char(c);
                    self.refresh_suggestion(history);
                    Action::None
                }
            }
            KeyCode::Home => {
                self.cursor = 0;
                Action::None
            }
            KeyCode::End => {
                self.cursor = self.input.chars().count();
                Action::None
            }
            _ => Action::None,
        }
    }

    fn submit_line(&mut self, history: &History) -> Action {
        let line = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.suggestion = None;

        let was_from_suggestion = self.last_action_was_suggestion;
        self.last_action_was_suggestion = false;

        if !line.is_empty() {
            let _ = history.record(HistoryEntry {
                id: None,
                host: self.host.clone(),
                user: self.user.clone(),
                cwd: None,
                command: line.clone(),
                timestamp: 0,
                exit_code: None,
                duration_ms: None,
                source: if was_from_suggestion {
                    HistorySource::SuggestHistory
                } else {
                    HistorySource::User
                },
            });
        }

        let mut bytes = line.into_bytes();
        bytes.push(b'\n');
        Action::Send(bytes)
    }

    fn accept_suggestion(&mut self, history: &History) -> Action {
        let Some(rest) = self.suggestion.take() else {
            return Action::None;
        };
        self.input.push_str(&rest);
        self.cursor = self.input.chars().count();
        self.last_action_was_suggestion = true;
        self.refresh_suggestion(history);
        Action::None
    }

    fn handle_ctrl_char(&mut self, c: char) -> Action {
        match c.to_ascii_lowercase() {
            // Ctrl+C: interrupt the running remote process.
            'c' => Action::Send(vec![0x03]),
            // Ctrl+D on empty line: EOF (closes remote shell). With content: clear input.
            'd' => {
                if self.input.is_empty() {
                    Action::Send(vec![0x04])
                } else {
                    self.input.clear();
                    self.cursor = 0;
                    self.suggestion = None;
                    Action::None
                }
            }
            // Ctrl+L: clear local scrollback (does not touch remote).
            'l' => {
                self.output.clear();
                Action::None
            }
            // Ctrl+U: kill line.
            'u' => {
                self.input.clear();
                self.cursor = 0;
                self.suggestion = None;
                Action::None
            }
            _ => Action::None,
        }
    }

    fn insert_char(&mut self, c: char) {
        let byte_idx = self.char_to_byte(self.cursor);
        self.input.insert(byte_idx, c);
        self.cursor += 1;
        self.last_action_was_suggestion = false;
    }

    fn char_to_byte(&self, char_pos: usize) -> usize {
        self.input
            .char_indices()
            .nth(char_pos)
            .map(|(b, _)| b)
            .unwrap_or(self.input.len())
    }

    fn append_output(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let stripped = ansi::strip(&text);

        // Continue the last line, then push remaining ones.
        let mut iter = stripped.split('\n');
        if let Some(first) = iter.next() {
            if let Some(last) = self.output.back_mut() {
                last.push_str(first);
            } else {
                self.output.push_back(first.to_string());
            }
        }
        for line in iter {
            self.output.push_back(line.to_string());
            while self.output.len() > MAX_OUTPUT_LINES {
                self.output.pop_front();
            }
        }
    }
}
