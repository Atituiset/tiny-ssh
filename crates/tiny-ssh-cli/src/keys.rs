//! Crossterm key event → terminal byte sequence encoder.
//!
//! Translates `KeyEvent` into the bytes a real xterm-compatible terminal
//! emulator would send for that keystroke. This is what gets shipped to the
//! remote PTY so the shell sees a keypress, not a logical line.
//!
//! The single knob from the VT side is `TermMode`: when the remote enables
//! application-cursor mode (`DECCKM`), arrow keys switch from the CSI form
//! `ESC [ A` to the SS3 form `ESC O A`. That's the only mode-dependent
//! behavior we need for P3; mouse + bracketed paste come in P5.
//!
//! Keys we don't translate (e.g. media keys, untranslatable modifier-only
//! events) yield an empty `Vec` so the caller can no-op them.
//!
//! References:
//! - xterm Control Sequences (ctlseqs): function keys, cursor keys
//! - VT100/VT220 user guide for app-cursor / app-keypad behavior

use alacritty_terminal::term::TermMode;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// Encode a key press into the bytes a real xterm would emit.
///
/// Returns an empty vec when the key has no terminal-protocol mapping
/// (modifier-only key down events, unsupported special keys, etc.).
pub fn encode(key: &KeyEvent, mode: TermMode) -> Vec<u8> {
    let app_cursor = mode.contains(TermMode::APP_CURSOR);

    match key.code {
        KeyCode::Char(c) => encode_char(c, key.modifiers),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Left => arrow(b'D', app_cursor),
        KeyCode::Right => arrow(b'C', app_cursor),
        KeyCode::Up => arrow(b'A', app_cursor),
        KeyCode::Down => arrow(b'B', app_cursor),
        KeyCode::Home => arrow(b'H', app_cursor),
        KeyCode::End => arrow(b'F', app_cursor),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::F(n) => f_key(n),
        _ => Vec::new(),
    }
}

/// Encode a mouse event into an SGR-style sequence.
///
/// Returns an empty vec when the remote has not enabled SGR mouse mode
/// (`TermMode::SGR_MOUSE`), so the caller can silently drop the event.
pub fn encode_mouse(event: &MouseEvent, mode: TermMode) -> Vec<u8> {
    if !mode.contains(TermMode::SGR_MOUSE) {
        return Vec::new();
    }

    let base = match event.kind {
        MouseEventKind::Down(MouseButton::Left) => 0,
        MouseEventKind::Down(MouseButton::Middle) => 1,
        MouseEventKind::Down(MouseButton::Right) => 2,
        MouseEventKind::Up(_) => 3,
        MouseEventKind::Drag(MouseButton::Left) => 32,
        MouseEventKind::Drag(MouseButton::Middle) => 33,
        MouseEventKind::Drag(MouseButton::Right) => 34,
        MouseEventKind::ScrollUp => 64,
        MouseEventKind::ScrollDown => 65,
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => return Vec::new(),
        MouseEventKind::Moved => return Vec::new(),
    };

    let mut btn = base;
    if event.modifiers.contains(KeyModifiers::SHIFT) {
        btn |= 4;
    }
    if event.modifiers.contains(KeyModifiers::ALT) {
        btn |= 8;
    }
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        btn |= 16;
    }

    let x = event.column + 1;
    let y = event.row + 1;
    let suffix = match event.kind {
        MouseEventKind::Up(_) => 'm',
        _ => 'M',
    };

    format!("\x1b[<{};{};{}{}", btn, x, y, suffix).into_bytes()
}

fn arrow(letter: u8, app_cursor: bool) -> Vec<u8> {
    if app_cursor {
        vec![0x1b, b'O', letter]
    } else {
        vec![0x1b, b'[', letter]
    }
}

fn encode_char(c: char, mods: KeyModifiers) -> Vec<u8> {
    if mods.contains(KeyModifiers::CONTROL) {
        if let Some(b) = ctrl_byte(c) {
            return if mods.contains(KeyModifiers::ALT) {
                vec![0x1b, b]
            } else {
                vec![b]
            };
        }
    }

    let mut buf = [0u8; 4];
    let bytes = c.encode_utf8(&mut buf).as_bytes().to_vec();
    if mods.contains(KeyModifiers::ALT) {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        out.push(0x1b);
        out.extend_from_slice(&bytes);
        out
    } else {
        bytes
    }
}

fn ctrl_byte(c: char) -> Option<u8> {
    let lower = c.to_ascii_lowercase();
    match lower {
        'a'..='z' => Some(lower as u8 - b'a' + 1),
        '@' | '2' | ' ' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

fn f_key(n: u8) -> Vec<u8> {
    match n {
        1 => b"\x1bOP".to_vec(),
        2 => b"\x1bOQ".to_vec(),
        3 => b"\x1bOR".to_vec(),
        4 => b"\x1bOS".to_vec(),
        5 => b"\x1b[15~".to_vec(),
        6 => b"\x1b[17~".to_vec(),
        7 => b"\x1b[18~".to_vec(),
        8 => b"\x1b[19~".to_vec(),
        9 => b"\x1b[20~".to_vec(),
        10 => b"\x1b[21~".to_vec(),
        11 => b"\x1b[23~".to_vec(),
        12 => b"\x1b[24~".to_vec(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn k(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn enc(code: KeyCode, mods: KeyModifiers) -> Vec<u8> {
        encode(&k(code, mods), TermMode::empty())
    }

    fn enc_app(code: KeyCode, mods: KeyModifiers) -> Vec<u8> {
        encode(&k(code, mods), TermMode::APP_CURSOR)
    }

    #[test]
    fn plain_letter() {
        assert_eq!(enc(KeyCode::Char('a'), KeyModifiers::NONE), b"a");
    }

    #[test]
    fn shift_letter_passes_uppercase() {
        // crossterm reports Shift+a as Char('A') with SHIFT modifier — we
        // just emit the codepoint.
        assert_eq!(enc(KeyCode::Char('A'), KeyModifiers::SHIFT), b"A");
    }

    #[test]
    fn ctrl_letter_emits_control_byte() {
        assert_eq!(enc(KeyCode::Char('a'), KeyModifiers::CONTROL), vec![0x01]);
        assert_eq!(enc(KeyCode::Char('z'), KeyModifiers::CONTROL), vec![0x1a]);
    }

    #[test]
    fn ctrl_uppercase_letter_also_works() {
        // Shift+Ctrl+a still produces ^A; case is normalized.
        assert_eq!(
            enc(
                KeyCode::Char('A'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ),
            vec![0x01]
        );
    }

    #[test]
    fn ctrl_punctuation() {
        assert_eq!(enc(KeyCode::Char('@'), KeyModifiers::CONTROL), vec![0x00]);
        assert_eq!(enc(KeyCode::Char('['), KeyModifiers::CONTROL), vec![0x1b]);
        assert_eq!(enc(KeyCode::Char('?'), KeyModifiers::CONTROL), vec![0x7f]);
    }

    #[test]
    fn alt_letter_prepends_esc() {
        assert_eq!(enc(KeyCode::Char('a'), KeyModifiers::ALT), b"\x1ba");
    }

    #[test]
    fn alt_ctrl_letter_is_esc_then_control_byte() {
        assert_eq!(
            enc(
                KeyCode::Char('a'),
                KeyModifiers::ALT | KeyModifiers::CONTROL
            ),
            vec![0x1b, 0x01]
        );
    }

    #[test]
    fn enter_is_cr() {
        assert_eq!(enc(KeyCode::Enter, KeyModifiers::NONE), b"\r");
    }

    #[test]
    fn tab_is_ht() {
        assert_eq!(enc(KeyCode::Tab, KeyModifiers::NONE), b"\t");
    }

    #[test]
    fn shift_tab_is_csi_z() {
        assert_eq!(enc(KeyCode::BackTab, KeyModifiers::SHIFT), b"\x1b[Z");
    }

    #[test]
    fn backspace_is_del() {
        assert_eq!(enc(KeyCode::Backspace, KeyModifiers::NONE), vec![0x7f]);
    }

    #[test]
    fn esc_is_esc() {
        assert_eq!(enc(KeyCode::Esc, KeyModifiers::NONE), vec![0x1b]);
    }

    #[test]
    fn arrows_use_csi_in_normal_mode() {
        assert_eq!(enc(KeyCode::Up, KeyModifiers::NONE), b"\x1b[A");
        assert_eq!(enc(KeyCode::Down, KeyModifiers::NONE), b"\x1b[B");
        assert_eq!(enc(KeyCode::Right, KeyModifiers::NONE), b"\x1b[C");
        assert_eq!(enc(KeyCode::Left, KeyModifiers::NONE), b"\x1b[D");
    }

    #[test]
    fn arrows_use_ss3_in_app_cursor_mode() {
        assert_eq!(enc_app(KeyCode::Up, KeyModifiers::NONE), b"\x1bOA");
        assert_eq!(enc_app(KeyCode::Down, KeyModifiers::NONE), b"\x1bOB");
        assert_eq!(enc_app(KeyCode::Right, KeyModifiers::NONE), b"\x1bOC");
        assert_eq!(enc_app(KeyCode::Left, KeyModifiers::NONE), b"\x1bOD");
    }

    #[test]
    fn home_end_track_app_cursor() {
        assert_eq!(enc(KeyCode::Home, KeyModifiers::NONE), b"\x1b[H");
        assert_eq!(enc(KeyCode::End, KeyModifiers::NONE), b"\x1b[F");
        assert_eq!(enc_app(KeyCode::Home, KeyModifiers::NONE), b"\x1bOH");
        assert_eq!(enc_app(KeyCode::End, KeyModifiers::NONE), b"\x1bOF");
    }

    #[test]
    fn page_keys_and_insert_delete() {
        assert_eq!(enc(KeyCode::Insert, KeyModifiers::NONE), b"\x1b[2~");
        assert_eq!(enc(KeyCode::Delete, KeyModifiers::NONE), b"\x1b[3~");
        assert_eq!(enc(KeyCode::PageUp, KeyModifiers::NONE), b"\x1b[5~");
        assert_eq!(enc(KeyCode::PageDown, KeyModifiers::NONE), b"\x1b[6~");
    }

    #[test]
    fn function_keys_f1_to_f4_use_ss3() {
        assert_eq!(enc(KeyCode::F(1), KeyModifiers::NONE), b"\x1bOP");
        assert_eq!(enc(KeyCode::F(2), KeyModifiers::NONE), b"\x1bOQ");
        assert_eq!(enc(KeyCode::F(3), KeyModifiers::NONE), b"\x1bOR");
        assert_eq!(enc(KeyCode::F(4), KeyModifiers::NONE), b"\x1bOS");
    }

    #[test]
    fn function_keys_f5_to_f12_use_csi_tilde() {
        assert_eq!(enc(KeyCode::F(5), KeyModifiers::NONE), b"\x1b[15~");
        assert_eq!(enc(KeyCode::F(6), KeyModifiers::NONE), b"\x1b[17~");
        assert_eq!(enc(KeyCode::F(11), KeyModifiers::NONE), b"\x1b[23~");
        assert_eq!(enc(KeyCode::F(12), KeyModifiers::NONE), b"\x1b[24~");
    }

    #[test]
    fn unicode_chars_pass_utf8() {
        assert_eq!(
            enc(KeyCode::Char('你'), KeyModifiers::NONE),
            "你".as_bytes()
        );
    }

    // ── mouse encoding tests ──────────────────────────────────────────

    fn m(kind: MouseEventKind, col: u16, row: u16, mods: KeyModifiers) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: mods,
        }
    }

    #[test]
    fn mouse_left_press_sgr() {
        let ev = m(MouseEventKind::Down(MouseButton::Left), 0, 0, KeyModifiers::NONE);
        assert_eq!(encode_mouse(&ev, TermMode::SGR_MOUSE), b"\x1b[<0;1;1M");
    }

    #[test]
    fn mouse_right_press_with_shift_sgr() {
        let ev = m(
            MouseEventKind::Down(MouseButton::Right),
            10,
            5,
            KeyModifiers::SHIFT,
        );
        // 2 + 4 = 6
        assert_eq!(encode_mouse(&ev, TermMode::SGR_MOUSE), b"\x1b[<6;11;6M");
    }

    #[test]
    fn mouse_release_uses_button_3() {
        let ev = m(MouseEventKind::Up(MouseButton::Left), 0, 0, KeyModifiers::NONE);
        assert_eq!(encode_mouse(&ev, TermMode::SGR_MOUSE), b"\x1b[<3;1;1m");
    }

    #[test]
    fn mouse_scroll_up() {
        let ev = m(MouseEventKind::ScrollUp, 2, 3, KeyModifiers::NONE);
        assert_eq!(encode_mouse(&ev, TermMode::SGR_MOUSE), b"\x1b[<64;3;4M");
    }

    #[test]
    fn mouse_drag_left() {
        let ev = m(MouseEventKind::Drag(MouseButton::Left), 0, 0, KeyModifiers::NONE);
        assert_eq!(encode_mouse(&ev, TermMode::SGR_MOUSE), b"\x1b[<32;1;1M");
    }

    #[test]
    fn mouse_without_sgr_mode_is_ignored() {
        let ev = m(MouseEventKind::Down(MouseButton::Left), 0, 0, KeyModifiers::NONE);
        assert_eq!(encode_mouse(&ev, TermMode::empty()), Vec::<u8>::new());
    }

    #[test]
    fn mouse_moved_is_ignored() {
        let ev = m(MouseEventKind::Moved, 0, 0, KeyModifiers::NONE);
        assert_eq!(encode_mouse(&ev, TermMode::SGR_MOUSE), Vec::<u8>::new());
    }

    #[test]
    fn unsupported_keys_yield_empty() {
        assert_eq!(enc(KeyCode::Null, KeyModifiers::NONE), Vec::<u8>::new());
        assert_eq!(enc(KeyCode::F(99), KeyModifiers::NONE), Vec::<u8>::new());
    }
}
