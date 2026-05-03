//! Tiny ANSI escape stripper.
//!
//! This is intentionally a stop-gap: line-mode rendering for v0.1.
//! v0.2 will swap this for `alacritty_terminal` for real VT emulation.

/// Remove the most common ANSI control sequences from `input`.
///
/// Handles:
/// - CSI sequences: `ESC [ ... <final>`
/// - OSC sequences: `ESC ] ... BEL` or `ESC ] ... ESC \`
/// - Two-byte ESC sequences (charset switches, etc.)
/// - Lone BEL (`0x07`)
/// - Backspace (`0x08`) by popping from the output
/// - Carriage return (`\r`) is dropped — line-mode rendering doesn't model the cursor
pub fn strip(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => skip_csi(&mut chars),
                Some(']') => skip_osc(&mut chars),
                Some(_) | None => {} // ESC + single byte (or trailing ESC) — drop
            },
            '\x07' => {} // BEL
            '\x08' => {
                out.pop();
            }
            '\r' => {} // ignore CR for line-mode
            other => out.push(other),
        }
    }
    out
}

fn skip_csi<I: Iterator<Item = char>>(chars: &mut std::iter::Peekable<I>) {
    // CSI: any number of params + intermediates, then a final byte 0x40..=0x7E
    for c in chars.by_ref() {
        if matches!(c, '@'..='~') {
            break;
        }
    }
}

fn skip_osc<I: Iterator<Item = char>>(chars: &mut std::iter::Peekable<I>) {
    // OSC: terminated by BEL or ST (ESC \)
    while let Some(c) = chars.next() {
        match c {
            '\x07' => break,
            '\x1b' => {
                if matches!(chars.peek(), Some('\\')) {
                    chars.next();
                    break;
                }
            }
            _ => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::strip;

    #[test]
    fn strips_color() {
        assert_eq!(strip("\x1b[31mhello\x1b[0m"), "hello");
    }

    #[test]
    fn strips_cursor_moves() {
        assert_eq!(strip("a\x1b[2Jb\x1b[Hc"), "abc");
    }

    #[test]
    fn handles_osc_title() {
        assert_eq!(strip("\x1b]0;title\x07after"), "after");
    }

    #[test]
    fn applies_backspace() {
        assert_eq!(strip("ab\x08c"), "ac");
    }

    #[test]
    fn drops_cr() {
        assert_eq!(strip("hi\r\nthere"), "hi\nthere");
    }

    #[test]
    fn passes_plain_utf8() {
        assert_eq!(strip("héllo 你好"), "héllo 你好");
    }
}
