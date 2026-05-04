# Bugfix Notes: Clipboard, Mouse Scroll, and Wide-Char Rendering

## Overview

Three UX issues were reported when running tssh on a phone and connecting to a remote host to run Claude Code:

1. **Wide-character rendering gaps** — CJK and other wide glyphs showed extra spacing.
2. **No copy-paste** — Mouse text selection did not work inside the tssh alternate screen.
3. **No mouse wheel scrolling** — The scroll wheel sent events to the remote instead of scrolling local history.

This document explains the root cause of each issue and the fix strategy.

---

## 1. Wide-Character Rendering Gaps

### Symptom
Chinese characters and other wide glyphs appeared with an extra space between them, as if every other character was missing or shifted.

### Root Cause
`alacritty_terminal` stores each grid cell as one display column. A wide character (e.g., a CJK character that occupies two terminal columns) is stored in the primary cell; the **next** cell is marked as a `WIDE_CHAR_SPACER` (or `LEADING_WIDE_CHAR_SPACER`) and contains a placeholder.

The original `render_grid` function in `ui.rs` iterated over `display_iter` and pushed **every** cell's character into the output string, including these spacer cells. The spacer cells typically contain a space or empty character, causing the visual gap.

### Fix
Filter out cells with the `WIDE_CHAR_SPACER` or `LEADING_WIDE_CHAR_SPACER` flag before rendering:

```rust
use alacritty_terminal::term::cell::Flags;

for indexed in content.display_iter {
    // ... line change logic ...
    if !indexed.cell.flags.intersects(
        Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER
    ) {
        current_text.push(indexed.cell.c);
    }
}
```

### Files Changed
- `crates/tiny-ssh-cli/src/ui.rs`

---

## 2. No Copy-Paste (Mouse Text Selection)

### Symptom
Users could not select text with the mouse to copy it to the system clipboard.

### Root Cause (Two Layers)

#### Layer 1: Mouse Capture
Tssh enables crossterm `EnableMouseCapture` on startup. This causes the terminal emulator to send all mouse events (clicks, drags, scrolls) to the application as byte sequences instead of handling them natively. When mouse capture is active, the terminal emulator typically cannot initiate text selection because it never sees the mouse events.

#### Layer 2: Alternate Screen
Tssh runs inside the terminal's **alternate screen** (`EnterAlternateScreen`). Most terminal emulators do not support mouse-based text selection inside the alternate screen at all, regardless of mouse capture state. The alternate screen is a separate buffer designed for full-screen TUI applications.

### Fix Strategy

#### 2a. Toggle Mouse Capture (`Ctrl+Shift+M`)
Added a shortcut to toggle crossterm mouse capture on/off dynamically:

- **On** (default): Mouse events are forwarded to the remote PTY. Required for remote apps like `vim` or `claude` that use mouse input.
- **Off**: The terminal emulator handles mouse events natively. Some terminals allow `Shift+drag` to select text even in alternate screen when mouse capture is disabled.

Implementation:
- Added `mouse_capture: bool` and `mouse_capture_changed: bool` to `App` state.
- `on_key` detects `Ctrl+Shift+M` and flips the flag.
- The `drive` loop syncs the crossterm state via `EnableMouseCapture` / `DisableMouseCapture`.
- Status bar shows `mouse` or `MOUSE-OFF`.

#### 2b. OSC 52 Clipboard Copy (`Ctrl+Shift+C`)
Since alternate-screen text selection is fundamentally limited by terminal emulators, the robust solution is **OSC 52** — a VT escape sequence that writes directly to the system clipboard without requiring mouse selection.

OSC 52 sequence format:
```
ESC ] 52 ; c ; <base64-encoded-text> BEL
```

Implementation:
- `Ctrl+Shift+C` triggers `Action::CopyToClipboard`.
- `App::on_key` extracts the full visible screen text via `Terminal::screen_text()`.
- The `drive` loop base64-encodes the text and writes the OSC 52 sequence to stdout.
- Supported by iTerm2, Windows Terminal, alacritty, GNOME Terminal, and most modern terminals.

### Files Changed
- `crates/tiny-ssh-cli/src/app.rs`
- `crates/tiny-ssh-cli/src/main.rs`
- `crates/tiny-ssh-cli/src/term.rs`
- `crates/tiny-ssh-cli/Cargo.toml` (added `base64` dependency)

---

## 3. No Mouse Wheel Scrolling

### Symptom
Scrolling the mouse wheel sent scroll events to the remote application instead of scrolling the local VT history.

### Root Cause
All mouse events (including `ScrollUp` / `ScrollDown`) were unconditionally encoded as SGR mouse sequences and forwarded to the remote PTY. There was no distinction between:
- **Normal screen** (shell prompt): Wheel should scroll local history.
- **Alt screen** (vim, claude, etc.): Wheel should be forwarded to the remote app.

### Fix
In `handle_terminal_event`, check whether the remote is in alt-screen mode before deciding what to do with wheel events:

```rust
use crossterm::event::MouseEventKind;

match mouse.kind {
    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        if !app.terminal.is_alt_screen() =>
    {
        // Local scroll: scroll the VT grid history
        let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) { -3 } else { 3 };
        app.terminal.scroll_display(delta);
    }
    _ => {
        // Forward to remote PTY (clicks, drags, and alt-screen scrolls)
        let bytes = keys::encode_mouse(&mouse, app.terminal.mode());
        // ... send to remote ...
    }
}
```

`alacritty_terminal::Term::scroll_display` adjusts the viewport offset into the scrollback history. A delta of `-3` scrolls up (showing older lines), `+3` scrolls down.

### Files Changed
- `crates/tiny-ssh-cli/src/main.rs`
- `crates/tiny-ssh-cli/src/term.rs` (added `scroll_display` wrapper)

---

## Future Improvements

### Copy-Mode (Not Implemented)
For terminals that do not support OSC 52, a **tmux-style copy-mode** could be added:

1. `Ctrl+Q [` enters selection mode.
2. Arrow keys / `hjkl` move a selection cursor.
3. `Space` marks the start of selection.
4. `Enter` or `y` copies the highlighted region via OSC 52.

This would allow precise text selection without relying on the terminal emulator's mouse support.

### No-Alternate-Screen Mode (Not Implemented)
An option to run tssh without `EnterAlternateScreen` would let the terminal emulator handle all mouse and selection natively. The trade-off is screen residue after exit.

---

## Shortcut Reference

| Shortcut | Action |
|----------|--------|
| `Ctrl+Q` | Quit tssh |
| `Ctrl+Shift+M` | Toggle mouse capture on/off |
| `Ctrl+Shift+C` | Copy visible screen to clipboard (OSC 52) |
| Mouse wheel (normal screen) | Scroll local VT history |
| Mouse wheel (alt screen) | Forward to remote application |
