//! VT terminal emulator wrapping `alacritty_terminal::Term`.
//!
//! Bytes from the SSH session are pushed through `feed`; the underlying `Term`
//! maintains the grid, cursor, scroll region, and alt-screen state. A side
//! scanner peeks at the byte stream for `OSC 7` (cwd) and `OSC 133` (prompt
//! markers) and reports them through `take_osc_events`.

use std::path::PathBuf;

use alacritty_terminal::Term;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::{Config, RenderableContent, TermMode};
use alacritty_terminal::vte::ansi::Processor;

/// Side-channel events sniffed from the byte stream while feeding the VT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OscEvent {
    /// `OSC 133;A` — about to print prompt.
    PromptStart,
    /// `OSC 133;B` — prompt finished, cursor at user-input column.
    PromptEnd,
    /// `OSC 133;C` — user pressed Enter, command output is starting.
    CommandStart,
    /// `OSC 133;D` — command finished. Optional exit code is ignored for now.
    CommandEnd,
    /// `OSC 7;<file-url>` — server reports a new working directory.
    Cwd(PathBuf),
}

/// Wraps a `Term` driven by a `Processor` so callers can push raw bytes in
/// and pull rendering state out.
pub struct Terminal {
    inner: Term<VoidListener>,
    parser: Processor,
    osc_scanner: OscScanner,
    pending_osc: Vec<OscEvent>,
}

impl Terminal {
    pub fn new(cols: u16, rows: u16) -> Self {
        let size = TermSize::new(cols.max(1) as usize, rows.max(1) as usize);
        let inner = Term::new(Config::default(), &size, VoidListener);
        Self {
            inner,
            parser: Processor::new(),
            osc_scanner: OscScanner::default(),
            pending_osc: Vec::new(),
        }
    }

    /// Feed bytes received from the remote PTY.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.osc_scanner.scan(bytes, &mut self.pending_osc);
        self.parser.advance(&mut self.inner, bytes);
    }

    /// Resize the grid. Idempotent; cheap when dimensions are unchanged.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let size = TermSize::new(cols.max(1) as usize, rows.max(1) as usize);
        self.inner.resize(size);
    }

    /// Current cursor position as `(col, line)`, both zero-indexed.
    pub fn cursor(&self) -> (u16, u16) {
        let p = self.inner.renderable_content().cursor.point;
        (p.column.0 as u16, p.line.0.max(0) as u16)
    }

    pub fn mode(&self) -> TermMode {
        *self.inner.mode()
    }

    pub fn is_alt_screen(&self) -> bool {
        self.inner.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Drain queued OSC events (caller takes ownership).
    pub fn take_osc_events(&mut self) -> Vec<OscEvent> {
        std::mem::take(&mut self.pending_osc)
    }

    /// Borrow renderable content for the current frame.
    pub fn renderable_content(&self) -> RenderableContent<'_> {
        self.inner.renderable_content()
    }

    /// Scroll the display viewport by `lines` (positive = down, negative = up).
    pub fn scroll_display(&mut self, lines: i32) {
        self.inner.scroll_display(Scroll::Delta(lines));
    }

    #[cfg(test)]
    fn cell_char(&self, col: u16, line: u16) -> char {
        use alacritty_terminal::index::{Column, Line, Point};
        let p = Point::new(Line(line as i32), Column(col as usize));
        self.inner.grid()[p].c
    }
}

/// Streaming scanner that recognizes `OSC 7;...` and `OSC 133;...` payloads
/// across arbitrary `feed` boundaries.
#[derive(Default)]
struct OscScanner {
    state: ScanState,
    payload: Vec<u8>,
}

#[derive(Default, Copy, Clone, PartialEq, Eq)]
enum ScanState {
    #[default]
    Plain,
    Esc,
    InOsc,
    OscMaybeSt,
}

const OSC_PAYLOAD_CAP: usize = 4096;

impl OscScanner {
    fn scan(&mut self, bytes: &[u8], out: &mut Vec<OscEvent>) {
        for &b in bytes {
            match self.state {
                ScanState::Plain => {
                    if b == 0x1b {
                        self.state = ScanState::Esc;
                    }
                }
                ScanState::Esc => {
                    if b == b']' {
                        self.payload.clear();
                        self.state = ScanState::InOsc;
                    } else {
                        self.state = ScanState::Plain;
                    }
                }
                ScanState::InOsc => match b {
                    0x07 => {
                        self.dispatch(out);
                        self.payload.clear();
                        self.state = ScanState::Plain;
                    }
                    0x1b => {
                        self.state = ScanState::OscMaybeSt;
                    }
                    _ => {
                        if self.payload.len() < OSC_PAYLOAD_CAP {
                            self.payload.push(b);
                        }
                    }
                },
                ScanState::OscMaybeSt => {
                    if b == b'\\' {
                        self.dispatch(out);
                    }
                    self.payload.clear();
                    self.state = ScanState::Plain;
                }
            }
        }
    }

    fn dispatch(&self, out: &mut Vec<OscEvent>) {
        let p = &self.payload;
        if let Some(rest) = p.strip_prefix(b"7;") {
            if let Some(path) = parse_file_url(rest) {
                out.push(OscEvent::Cwd(path));
            }
        } else if let Some(rest) = p.strip_prefix(b"133;") {
            match rest.first() {
                Some(b'A') => out.push(OscEvent::PromptStart),
                Some(b'B') => out.push(OscEvent::PromptEnd),
                Some(b'C') => out.push(OscEvent::CommandStart),
                Some(b'D') => out.push(OscEvent::CommandEnd),
                _ => {}
            }
        }
    }
}

/// Parse `file://host/path/with%20encoding` (or just `/raw/path`) into a
/// `PathBuf`. Returns `None` for empty or undecodable input.
fn parse_file_url(bytes: &[u8]) -> Option<PathBuf> {
    let s = std::str::from_utf8(bytes).ok()?;
    let path_part = if let Some(rest) = s.strip_prefix("file://") {
        let slash = rest.find('/')?;
        &rest[slash..]
    } else {
        s
    };
    let mut decoded = String::with_capacity(path_part.len());
    let mut iter = path_part.bytes();
    while let Some(b) = iter.next() {
        if b == b'%' {
            let h = iter.next()?;
            let l = iter.next()?;
            let high = (h as char).to_digit(16)?;
            let low = (l as char).to_digit(16)?;
            decoded.push(char::from((high * 16 + low) as u8));
        } else {
            decoded.push(char::from(b));
        }
    }
    if decoded.is_empty() {
        None
    } else {
        Some(PathBuf::from(decoded))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(cols: u16, rows: u16) -> Terminal {
        Terminal::new(cols, rows)
    }

    #[test]
    fn plain_ascii_lands_in_grid() {
        let mut t = term(20, 5);
        t.feed(b"hi");
        assert_eq!(t.cell_char(0, 0), 'h');
        assert_eq!(t.cell_char(1, 0), 'i');
        assert_eq!(t.cursor(), (2, 0));
    }

    #[test]
    fn cr_lf_advances_to_next_row() {
        let mut t = term(20, 5);
        t.feed(b"a\r\nb");
        assert_eq!(t.cell_char(0, 0), 'a');
        assert_eq!(t.cell_char(0, 1), 'b');
        assert_eq!(t.cursor(), (1, 1));
    }

    #[test]
    fn cursor_move_csi() {
        let mut t = term(20, 5);
        // CSI 3;5 H — move to row 3 col 5 (1-based)
        t.feed(b"\x1b[3;5H");
        // Cursor at row 2 col 4 (zero-based)
        assert_eq!(t.cursor(), (4, 2));
    }

    #[test]
    fn alt_screen_toggle() {
        let mut t = term(20, 5);
        assert!(!t.is_alt_screen());
        t.feed(b"\x1b[?1049h");
        assert!(t.is_alt_screen());
        t.feed(b"\x1b[?1049l");
        assert!(!t.is_alt_screen());
    }

    #[test]
    fn osc_7_cwd_decoded() {
        let mut t = term(20, 5);
        t.feed(b"\x1b]7;file:///home/user/work\x07");
        let events = t.take_osc_events();
        assert_eq!(events, vec![OscEvent::Cwd(PathBuf::from("/home/user/work"))]);
    }

    #[test]
    fn osc_7_with_percent_encoding() {
        let mut t = term(20, 5);
        t.feed(b"\x1b]7;file:///home/space%20here\x07");
        let events = t.take_osc_events();
        assert_eq!(events, vec![OscEvent::Cwd(PathBuf::from("/home/space here"))]);
    }

    #[test]
    fn osc_7_terminated_by_st_escape() {
        let mut t = term(20, 5);
        t.feed(b"\x1b]7;file:///x\x1b\\");
        assert_eq!(t.take_osc_events(), vec![OscEvent::Cwd(PathBuf::from("/x"))]);
    }

    #[test]
    fn osc_133_prompt_markers() {
        let mut t = term(20, 5);
        t.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07hi\r\n\x1b]133;D\x07");
        assert_eq!(
            t.take_osc_events(),
            vec![
                OscEvent::PromptStart,
                OscEvent::PromptEnd,
                OscEvent::CommandStart,
                OscEvent::CommandEnd,
            ]
        );
    }

    #[test]
    fn osc_payload_split_across_feeds() {
        let mut t = term(20, 5);
        t.feed(b"\x1b]7;file:///pa");
        t.feed(b"rt\x07");
        assert_eq!(t.take_osc_events(), vec![OscEvent::Cwd(PathBuf::from("/part"))]);
    }

    #[test]
    fn unknown_osc_is_ignored() {
        let mut t = term(20, 5);
        // OSC 0;title (window title) — we don't surface it as Cwd or 133
        t.feed(b"\x1b]0;mywindow\x07");
        assert!(t.take_osc_events().is_empty());
    }

    #[test]
    fn osc_scanner_recovers_after_lone_esc() {
        // Direct OscScanner test: an ESC not followed by `]` should not
        // poison the scanner — a real OSC after it must still be recognized.
        let mut s = OscScanner::default();
        let mut out = Vec::new();
        s.scan(b"\x1bA\x1b]133;A\x07", &mut out);
        assert_eq!(out, vec![OscEvent::PromptStart]);
    }

    #[test]
    fn resize_changes_dimensions() {
        let mut t = term(20, 5);
        t.feed(b"hello");
        t.resize(40, 10);
        // Content within bounds is preserved; cursor stays where it was.
        assert_eq!(t.cell_char(0, 0), 'h');
        assert_eq!(t.cursor(), (5, 0));
    }
}
