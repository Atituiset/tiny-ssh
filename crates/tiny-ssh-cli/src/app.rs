//! TUI application state.
//!
//! The App owns:
//! - the VT grid driven by remote PTY output
//! - a shadow input buffer that mirrors the remote shell's command line
//! - last-seen session state and connection metadata
//!
//! Input is **raw passthrough**: every key the user presses is encoded by
//! [`crate::keys::encode`] and shipped to the remote PTY immediately. We never
//! buffer a line locally; the remote shell is the source of truth and echoes
//! its own state back through the VT.
//!
//! The "shadow buffer" is a parallel best-effort model of the current command
//! line, used solely to look up history-based autosuggestions. It is updated
//! by [`App::apply_to_shadow`] in lockstep with each keystroke that gets
//! sent. It can drift (Tab completion, Ctrl-R, paste, custom keymaps) — the
//! five-condition gate around `refresh_suggestion` (alt-screen, prompt
//! marker, etc.) hides ghost-text whenever drift is plausible.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tiny_ssh_core::{
    EngineSuggestion, History, HistoryEntry, HistorySource, SessionEvent, SessionState,
    SuggestContext, SuggestEngine,
};

use crate::keys;
use crate::term::Terminal;

/// What [`App::on_key`] wants the driver to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Send these bytes to the remote shell.
    Send(Vec<u8>),
    /// Disconnect cleanly and exit.
    Quit,
    /// Toggle local mouse capture (Ctrl+Shift+M).
    ToggleMouseCapture,
    /// No-op (UI-only state change, or a key we don't translate).
    None,
}

pub struct App {
    pub host: String,
    pub user: String,
    pub state: SessionState,

    /// VT grid driven by remote PTY bytes.
    pub terminal: Terminal,
    /// Whether the driver task has fully exited.
    pub closed: bool,
    /// Last error message, if any.
    pub last_error: Option<String>,

    /// Best-effort mirror of the remote command line, in characters typed by
    /// the user since the last shell prompt. Used only for autosuggest lookup.
    pub shadow_input: String,
    /// Cursor position within `shadow_input` (in chars, not bytes).
    pub shadow_cursor: usize,
    /// Cached autosuggestion (just the suffix beyond `shadow_input`).
    pub suggestion: Option<String>,

    /// Last `OSC 133;B` position. Set by OSC scanner or the heuristic fallback.
    pub prompt_pos: Option<(u16, u16)>,
    /// Last `OSC 7` cwd report from the remote shell.
    pub cwd: Option<PathBuf>,

    /// Did the user accept a suggestion since last keystroke?
    /// Used to tag the next history record's source.
    last_action_was_suggestion: bool,
    /// True after Enter is pressed until we see the next prompt heuristic.
    pub last_input_was_enter: bool,

    /// Whether crossterm mouse capture is enabled.
    /// When enabled, mouse events are forwarded to the remote PTY.
    /// When disabled, the terminal emulator handles mouse natively
    /// (text selection, right-click paste, etc.).
    pub mouse_capture: bool,
    /// Set to `true` when `mouse_capture` changes so the driver can sync
    /// the crossterm state (EnableMouseCapture / DisableMouseCapture).
    pub mouse_capture_changed: bool,
}

impl App {
    pub fn new(host: String, user: String, cols: u16, rows: u16) -> Self {
        Self {
            host,
            user,
            state: SessionState::Connecting,
            terminal: Terminal::new(cols, rows),
            closed: false,
            last_error: None,
            shadow_input: String::new(),
            shadow_cursor: 0,
            suggestion: None,
            prompt_pos: None,
            cwd: None,
            last_action_was_suggestion: false,
            last_input_was_enter: false,
            mouse_capture: true,
            mouse_capture_changed: false,
        }
    }

    /// Resize the underlying VT grid. Caller is responsible for also resizing
    /// the remote PTY.
    pub fn on_resize(&mut self, cols: u16, rows: u16) {
        self.terminal.resize(cols, rows);
    }

    /// Apply an event from the session driver.
    pub fn on_session_event(&mut self, ev: SessionEvent) {
        match ev {
            SessionEvent::StateChanged(s) => self.state = s,
            SessionEvent::Output(bytes) | SessionEvent::Stderr(bytes) => {
                self.terminal.feed(&bytes);
                for osc in self.terminal.take_osc_events() {
                    self.on_osc_event(osc);
                }
                self.handle_prompt_heuristic(&bytes);
            }
            SessionEvent::ExitStatus(_) => {}
            SessionEvent::Error(msg) => self.last_error = Some(msg),
            SessionEvent::Closed => self.closed = true,
        }
    }

    fn on_osc_event(&mut self, ev: crate::term::OscEvent) {
        use crate::term::OscEvent;
        match ev {
            OscEvent::PromptStart => {
                // Shell is about to draw a prompt; state reset is done at
                // PromptEnd once the prompt is actually positioned.
            }
            OscEvent::PromptEnd => {
                self.prompt_pos = Some(self.terminal.cursor());
                self.shadow_input.clear();
                self.shadow_cursor = 0;
                self.suggestion = None;
            }
            OscEvent::CommandStart => {
                // User pressed Enter; prompt is no longer active.
                self.prompt_pos = None;
            }
            OscEvent::CommandEnd => {
                // Could record exit code here in v0.3+.
            }
            OscEvent::Cwd(p) => self.cwd = Some(p),
        }
    }

    /// Lazy fallback: if the last key was Enter and the new output contains
    /// a newline and the cursor ended up on a row with col > 0, guess that
    /// the cursor position is a prompt.
    fn handle_prompt_heuristic(&mut self, bytes: &[u8]) {
        if !self.last_input_was_enter {
            return;
        }
        if !bytes.contains(&b'\n') {
            return;
        }
        let (col, _row) = self.terminal.cursor();
        if col > 0 {
            self.prompt_pos = Some(self.terminal.cursor());
        }
        self.last_input_was_enter = false;
    }

    /// Recompute the autosuggestion against the persistent history.
    ///
    /// This always computes the suggestion based on the current shadow prefix;
    /// it does **not** gate on VT cursor position or prompt markers. The
    /// five-condition gate lives in [`App::can_show_suggestion`] and is
    /// evaluated at render time, after the VT has caught up with remote echo.
    pub fn refresh_suggestion(&mut self, history: &History) {
        if self.shadow_input.is_empty() {
            self.suggestion = None;
            return;
        }
        let engine = SuggestEngine::new(history);
        let cwd_str = self.cwd.as_ref().and_then(|p| p.to_str());
        let ctx = SuggestContext {
            host: &self.host,
            cwd: cwd_str,
            prefix: &self.shadow_input,
        };
        match engine.suggest(&ctx) {
            Ok(Some(EngineSuggestion { command, .. })) => {
                if command.starts_with(&self.shadow_input)
                    && command.len() > self.shadow_input.len()
                {
                    self.suggestion =
                        Some(command[self.shadow_input.len()..].to_string());
                } else {
                    self.suggestion = None;
                }
            }
            _ => self.suggestion = None,
        }
    }

    /// Five-condition gate: is it safe to display the ghost-text right now?
    ///
    /// Evaluated at render time so the VT cursor is guaranteed to be in sync
    /// with any remote echo that arrived since the last keystroke.
    pub fn can_show_suggestion(&self) -> bool {
        self.suggestion.is_some() && {
            let Some((prompt_col, prompt_row)) = self.prompt_pos else {
                return false;
            };
            let (cur_col, cur_row) = self.terminal.cursor();
            if self.terminal.is_alt_screen()
                || cur_row != prompt_row
                || cur_col < prompt_col
            {
                return false;
            }
            let typed_len = self.shadow_input.chars().count();
            let grid_distance = (cur_col - prompt_col) as usize;
            typed_len == grid_distance
        }
    }

    /// Apply a key event. Encodes the key, updates the shadow buffer, and
    /// asks the driver to forward the encoded bytes to the remote.
    pub fn on_key(&mut self, key: KeyEvent, history: &History) -> Action {
        // Local capture: Ctrl-Q for quit, Ctrl+Shift+M toggles mouse capture.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
            return Action::Quit;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
            && matches!(key.code, KeyCode::Char('m' | 'M'))
        {
            self.mouse_capture = !self.mouse_capture;
            self.mouse_capture_changed = true;
            return Action::ToggleMouseCapture;
        }

        // Right at end-of-shadow with an active suggestion: accept the ghost.
        if matches!(key.code, KeyCode::Right)
            && key.modifiers.is_empty()
            && self.suggestion.is_some()
            && self.shadow_cursor == self.shadow_input.chars().count()
        {
            // Safe: guarded by suggestion.is_some() above.
            let rest = self.suggestion.take().expect("suggestion present");
            self.shadow_input.push_str(&rest);
            self.shadow_cursor = self.shadow_input.chars().count();
            self.last_action_was_suggestion = true;
            self.refresh_suggestion(history);
            return Action::Send(rest.into_bytes());
        }

        let bytes = keys::encode(&key, self.terminal.mode());
        if bytes.is_empty() {
            return Action::None;
        }

        self.apply_to_shadow(&key, history);
        Action::Send(bytes)
    }

    /// Update the shadow buffer in lockstep with what we just sent to the
    /// remote. This models a vanilla readline editor; non-readline shells or
    /// programs in alt-screen will quickly drift but the suggestion gate
    /// catches that case.
    fn apply_to_shadow(&mut self, key: &KeyEvent, history: &History) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Char(c) if ctrl => {
                match c.to_ascii_lowercase() {
                    'a' => self.shadow_cursor = 0,
                    'e' => self.shadow_cursor = self.shadow_input.chars().count(),
                    'u' => {
                        // Kill whole line.
                        self.shadow_input.clear();
                        self.shadow_cursor = 0;
                        self.last_action_was_suggestion = false;
                    }
                    'k' => {
                        let bi = self.char_to_byte(self.shadow_cursor);
                        self.shadow_input.truncate(bi);
                        self.last_action_was_suggestion = false;
                    }
                    'w' => self.kill_word_backward(),
                    'c' | 'd' => {
                        // Ctrl-C aborts, Ctrl-D on empty sends EOF; in both
                        // cases the prompt redraws blank.
                        self.shadow_input.clear();
                        self.shadow_cursor = 0;
                        self.suggestion = None;
                        self.last_action_was_suggestion = false;
                        self.last_input_was_enter = false;
                    }
                    'l' => {
                        // Most readlines redraw the prompt + current line on
                        // Ctrl-L; shadow stays valid.
                    }
                    _ => {}
                }
                self.refresh_suggestion(history);
            }
            KeyCode::Char(c) => {
                self.insert_char(c);
                self.refresh_suggestion(history);
            }
            KeyCode::Backspace => {
                if self.shadow_cursor > 0 {
                    self.shadow_cursor -= 1;
                    let bi = self.char_to_byte(self.shadow_cursor);
                    self.shadow_input.remove(bi);
                    self.last_action_was_suggestion = false;
                }
                self.refresh_suggestion(history);
            }
            KeyCode::Enter => {
                if !self.shadow_input.is_empty() {
                    let _ = history.record(HistoryEntry {
                        id: None,
                        host: self.host.clone(),
                        user: self.user.clone(),
                        cwd: self.cwd.as_ref().and_then(|p| p.to_str().map(String::from)),
                        command: self.shadow_input.clone(),
                        timestamp: 0,
                        exit_code: None,
                        duration_ms: None,
                        source: if self.last_action_was_suggestion {
                            HistorySource::SuggestHistory
                        } else {
                            HistorySource::User
                        },
                    });
                }
                self.shadow_input.clear();
                self.shadow_cursor = 0;
                self.suggestion = None;
                self.last_action_was_suggestion = false;
                self.last_input_was_enter = true;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                // Remote handles completion; we can't predict the resulting
                // line, so drop the suggestion. The shadow buffer stays as-is
                // and the prompt-row check in P4 will hide ghost-text if it
                // diverges.
                self.suggestion = None;
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                // Remote readline replaces the line on history nav; our model
                // is no longer trustworthy.
                self.shadow_input.clear();
                self.shadow_cursor = 0;
                self.suggestion = None;
                self.last_action_was_suggestion = false;
                self.last_input_was_enter = false;
            }
            KeyCode::Left if self.shadow_cursor > 0 => {
                self.shadow_cursor -= 1;
            }
            KeyCode::Right
                if self.shadow_cursor < self.shadow_input.chars().count() =>
            {
                self.shadow_cursor += 1;
            }
            KeyCode::Home => self.shadow_cursor = 0,
            KeyCode::End => self.shadow_cursor = self.shadow_input.chars().count(),
            _ => {}
        }
    }

    fn insert_char(&mut self, c: char) {
        let byte_idx = self.char_to_byte(self.shadow_cursor);
        self.shadow_input.insert(byte_idx, c);
        self.shadow_cursor += 1;
        self.last_action_was_suggestion = false;
    }

    /// Delete from the cursor back to the start of the previous word.
    fn kill_word_backward(&mut self) {
        if self.shadow_cursor == 0 {
            return;
        }
        let chars: Vec<char> = self.shadow_input.chars().collect();
        let mut i = self.shadow_cursor;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        if i == self.shadow_cursor {
            return;
        }
        let new_chars: String = chars[..i]
            .iter()
            .chain(chars[self.shadow_cursor..].iter())
            .collect();
        self.shadow_input = new_chars;
        self.shadow_cursor = i;
        self.last_action_was_suggestion = false;
    }

    fn char_to_byte(&self, char_pos: usize) -> usize {
        self.shadow_input
            .char_indices()
            .nth(char_pos)
            .map(|(b, _)| b)
            .unwrap_or(self.shadow_input.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;
    use tiny_ssh_core::HistoryEntry;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn mk() -> (App, History) {
        let h = History::open_in_memory().unwrap();
        (App::new("h1".into(), "u".into(), 80, 24), h)
    }

    /// Simulate typing each character: encode the key, update the shadow,
    /// and feed the emitted bytes back into the VT as if the remote shell
    /// echoed them. This keeps the VT cursor in sync with the shadow buffer
    /// so the five-condition gating works in tests.
    fn type_str(app: &mut App, h: &History, s: &str) {
        for c in s.chars() {
            let action = app.on_key(key(KeyCode::Char(c), KeyModifiers::NONE), h);
            if let Action::Send(bytes) = action {
                app.terminal.feed(&bytes);
            }
        }
    }

    fn record(h: &History, host: &str, cmd: &str, ts: i64) {
        h.record(HistoryEntry {
            id: None,
            host: host.into(),
            user: "u".into(),
            cwd: None,
            command: cmd.into(),
            timestamp: ts,
            exit_code: None,
            duration_ms: None,
            source: HistorySource::User,
        })
        .unwrap();
    }

    fn send_bytes(action: Action) -> Vec<u8> {
        match action {
            Action::Send(b) => b,
            other => panic!("expected Send, got {other:?}"),
        }
    }

    #[test]
    fn ctrl_q_returns_quit() {
        let (mut app, h) = mk();
        let action = app.on_key(key(KeyCode::Char('q'), KeyModifiers::CONTROL), &h);
        assert!(matches!(action, Action::Quit));
    }

    #[test]
    fn typing_a_char_sends_byte_and_updates_shadow() {
        let (mut app, h) = mk();
        let action = app.on_key(key(KeyCode::Char('x'), KeyModifiers::NONE), &h);
        assert_eq!(send_bytes(action), b"x");
        assert_eq!(app.shadow_input, "x");
        assert_eq!(app.shadow_cursor, 1);
    }

    #[test]
    fn enter_sends_cr_clears_shadow_and_records_history() {
        let (mut app, h) = mk();
        type_str(&mut app, &h, "ls -la");
        let action = app.on_key(key(KeyCode::Enter, KeyModifiers::NONE), &h);
        assert_eq!(send_bytes(action), b"\r");
        assert_eq!(app.shadow_input, "");
        assert_eq!(app.shadow_cursor, 0);
        let recent = h.recent("h1", 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].command, "ls -la");
    }

    #[test]
    fn ctrl_a_jumps_to_start_in_shadow() {
        let (mut app, h) = mk();
        type_str(&mut app, &h, "ls -la");
        assert_eq!(app.shadow_cursor, 6);
        let action = app.on_key(key(KeyCode::Char('a'), KeyModifiers::CONTROL), &h);
        assert_eq!(send_bytes(action), vec![0x01]);
        assert_eq!(app.shadow_cursor, 0);
        assert_eq!(app.shadow_input, "ls -la");
    }

    #[test]
    fn ctrl_e_jumps_to_end_in_shadow() {
        let (mut app, h) = mk();
        type_str(&mut app, &h, "echo hi");
        app.shadow_cursor = 0;
        app.on_key(key(KeyCode::Char('e'), KeyModifiers::CONTROL), &h);
        assert_eq!(app.shadow_cursor, 7);
    }

    #[test]
    fn ctrl_w_kills_word_in_shadow() {
        let (mut app, h) = mk();
        type_str(&mut app, &h, "git commit -m hello");
        app.on_key(key(KeyCode::Char('w'), KeyModifiers::CONTROL), &h);
        assert_eq!(app.shadow_input, "git commit -m ");
        app.on_key(key(KeyCode::Char('w'), KeyModifiers::CONTROL), &h);
        assert_eq!(app.shadow_input, "git commit ");
    }

    #[test]
    fn ctrl_w_at_start_of_shadow_is_noop() {
        let (mut app, h) = mk();
        type_str(&mut app, &h, "foo");
        app.shadow_cursor = 0;
        app.on_key(key(KeyCode::Char('w'), KeyModifiers::CONTROL), &h);
        assert_eq!(app.shadow_input, "foo");
        assert_eq!(app.shadow_cursor, 0);
    }

    #[test]
    fn ctrl_k_truncates_shadow() {
        let (mut app, h) = mk();
        type_str(&mut app, &h, "echo hello world");
        app.shadow_cursor = 4;
        app.on_key(key(KeyCode::Char('k'), KeyModifiers::CONTROL), &h);
        assert_eq!(app.shadow_input, "echo");
        assert_eq!(app.shadow_cursor, 4);
    }

    #[test]
    fn ctrl_u_kills_whole_shadow() {
        let (mut app, h) = mk();
        type_str(&mut app, &h, "abc def");
        app.on_key(key(KeyCode::Char('u'), KeyModifiers::CONTROL), &h);
        assert_eq!(app.shadow_input, "");
        assert_eq!(app.shadow_cursor, 0);
    }

    #[test]
    fn ctrl_c_clears_shadow() {
        let (mut app, h) = mk();
        type_str(&mut app, &h, "wip");
        let action = app.on_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL), &h);
        assert_eq!(send_bytes(action), vec![0x03]);
        assert_eq!(app.shadow_input, "");
    }

    #[test]
    fn left_inside_shadow_moves_cursor() {
        let (mut app, h) = mk();
        type_str(&mut app, &h, "abc");
        app.on_key(key(KeyCode::Left, KeyModifiers::NONE), &h);
        assert_eq!(app.shadow_cursor, 2);
        assert_eq!(app.shadow_input, "abc");
    }

    #[test]
    fn right_inside_shadow_moves_cursor() {
        let (mut app, h) = mk();
        type_str(&mut app, &h, "abc");
        app.shadow_cursor = 0;
        app.on_key(key(KeyCode::Right, KeyModifiers::NONE), &h);
        assert_eq!(app.shadow_cursor, 1);
        assert_eq!(app.shadow_input, "abc");
    }

    #[test]
    fn right_at_end_accepts_suggestion() {
        let (mut app, h) = mk();
        record(&h, "h1", "git status", 100);
        app.terminal.feed(b"\x1b]133;B\x07"); // establish prompt
        type_str(&mut app, &h, "gi");
        assert_eq!(app.suggestion.as_deref(), Some("t status"));
        let action = app.on_key(key(KeyCode::Right, KeyModifiers::NONE), &h);
        assert_eq!(send_bytes(action), b"t status");
        assert_eq!(app.shadow_input, "git status");
        assert_eq!(app.shadow_cursor, 10);
    }

    #[test]
    fn right_at_end_without_suggestion_sends_csi() {
        let (mut app, h) = mk();
        type_str(&mut app, &h, "ab");
        // No history — no suggestion. Right should pass through as CSI C.
        assert!(app.suggestion.is_none());
        let action = app.on_key(key(KeyCode::Right, KeyModifiers::NONE), &h);
        assert_eq!(send_bytes(action), b"\x1b[C");
    }

    #[test]
    fn alt_screen_hides_suggestion() {
        let (mut app, h) = mk();
        record(&h, "h1", "git status", 100);
        app.on_session_event(SessionEvent::Output(b"\x1b]133;B\x07".to_vec()));
        // Enter alt-screen via DEC private mode.
        app.terminal.feed(b"\x1b[?1049h");
        type_str(&mut app, &h, "gi");
        assert!(app.suggestion.is_some(), "suggestion computed even in alt-screen");
        assert!(
            !app.can_show_suggestion(),
            "ghost-text hidden by alt-screen gate"
        );
    }

    #[test]
    fn up_clears_shadow_and_passes_through() {
        let (mut app, h) = mk();
        type_str(&mut app, &h, "draft");
        let action = app.on_key(key(KeyCode::Up, KeyModifiers::NONE), &h);
        assert_eq!(send_bytes(action), b"\x1b[A");
        assert_eq!(app.shadow_input, "");
    }

    #[test]
    fn tab_passes_through_and_drops_suggestion() {
        let (mut app, h) = mk();
        record(&h, "h1", "git status", 100);
        app.terminal.feed(b"\x1b]133;B\x07"); // establish prompt
        type_str(&mut app, &h, "gi");
        assert!(app.suggestion.is_some());
        let action = app.on_key(key(KeyCode::Tab, KeyModifiers::NONE), &h);
        assert_eq!(send_bytes(action), b"\t");
        assert!(app.suggestion.is_none());
    }

    // ── P4: OSC 133/7 + five-condition gating ─────────────────────────

    #[test]
    fn osc_133_prompt_end_sets_prompt_pos_and_clears_shadow() {
        let (mut app, h) = mk();
        type_str(&mut app, &h, "ls");
        app.on_session_event(SessionEvent::Output(b"\x1b]133;B\x07".to_vec()));
        assert!(app.prompt_pos.is_some());
        assert_eq!(app.shadow_input, "");
        assert_eq!(app.shadow_cursor, 0);
    }

    #[test]
    fn osc_133_command_start_clears_prompt_pos() {
        let (mut app, _h) = mk();
        app.on_session_event(SessionEvent::Output(b"\x1b]133;B\x07".to_vec()));
        assert!(app.prompt_pos.is_some());
        app.on_session_event(SessionEvent::Output(b"\x1b]133;C\x07".to_vec()));
        assert!(app.prompt_pos.is_none());
    }

    #[test]
    fn osc_7_cwd_updates_app_cwd() {
        let (mut app, _h) = mk();
        app.on_session_event(SessionEvent::Output(
            b"\x1b]7;file:///home/user/work\x07".to_vec(),
        ));
        assert_eq!(app.cwd.as_deref(), Some(std::path::Path::new("/home/user/work")));
    }

    #[test]
    fn five_condition_gate_requires_prompt_pos() {
        let (mut app, h) = mk();
        record(&h, "h1", "git status", 100);
        // Without any OSC 133 or heuristic, prompt_pos stays None.
        type_str(&mut app, &h, "gi");
        assert!(app.prompt_pos.is_none());
        assert!(
            !app.can_show_suggestion(),
            "no ghost-text without prompt_pos (gating condition #1)"
        );
    }

    #[test]
    fn five_condition_gate_requires_same_row() {
        let (mut app, h) = mk();
        record(&h, "h1", "git status", 100);
        // Simulate a prompt at row 0 via OSC 133 (processed through on_session_event
        // so take_osc_events is drained and prompt_pos is actually set).
        app.on_session_event(SessionEvent::Output(b"\x1b]133;B\x07".to_vec()));
        // Type "gi" — cursor on row 0.
        type_str(&mut app, &h, "gi");
        assert!(app.can_show_suggestion());
        // Feed a newline, moving cursor to row 1. The prompt position
        // is still at row 0, so ghost-text should disappear.
        app.terminal.feed(b"\n");
        assert!(
            !app.can_show_suggestion(),
            "no ghost-text when cursor row != prompt row (gating condition #2)"
        );
    }

    #[test]
    fn five_condition_gate_requires_length_match() {
        let (mut app, h) = mk();
        record(&h, "h1", "git status", 100);
        app.on_session_event(SessionEvent::Output(b"\x1b]133;B\x07".to_vec()));
        // Type "gi" (2 chars). Cursor at col 2.
        type_str(&mut app, &h, "gi");
        assert_eq!(app.terminal.cursor(), (2, 0));
        assert!(app.can_show_suggestion());
        // Feed a space — cursor at col 3, but shadow still 2 chars.
        app.terminal.feed(b" ");
        assert!(
            !app.can_show_suggestion(),
            "no ghost-text when shadow length != grid distance (gating condition #5)"
        );
    }

    #[test]
    fn heuristic_prompt_detection_after_enter() {
        let (mut app, h) = mk();
        record(&h, "h1", "git status", 100);
        // Type a command and press Enter.
        type_str(&mut app, &h, "echo");
        app.on_key(key(KeyCode::Enter, KeyModifiers::NONE), &h);
        assert!(app.last_input_was_enter);
        assert!(app.prompt_pos.is_none());
        // Remote shell sends prompt on a new line.
        app.on_session_event(SessionEvent::Output(b"\r\n$ ".to_vec()));
        assert!(!app.last_input_was_enter);
        assert!(app.prompt_pos.is_some(),
            "heuristic detected prompt after Enter + newline");
        // Now ghost-text should appear on the new prompt.
        type_str(&mut app, &h, "gi");
        assert!(app.can_show_suggestion());
    }
}
