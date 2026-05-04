//! ratatui rendering for the App.
//!
//! Layout: a full-screen VT grid on top, a single status line on the bottom.
//! The visible cursor is placed at the VT cursor position; the autosuggest
//! ghost-text is overlaid as dim text starting at that cursor.

use alacritty_terminal::term::cell::Flags;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use tiny_ssh_core::SessionState;

use crate::app::App;

pub fn render(f: &mut Frame<'_>, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // VT grid
            Constraint::Length(1), // status bar
        ])
        .split(area);

    let grid_area = chunks[0];
    render_grid(f, grid_area, app);
    render_status(f, chunks[1], app);

    let (cur_col, cur_row) = app.terminal.cursor();
    let cx = grid_area.x.saturating_add(cur_col);
    let cy = grid_area.y.saturating_add(cur_row);
    if cx < grid_area.right() && cy < grid_area.bottom() {
        f.set_cursor_position((cx, cy));
    }
}

/// Render the VT grid plus optional ghost-text overlay.
///
/// P3 keeps styling minimal — just glyphs. Colour and attributes wait until
/// later phases.
fn render_grid(f: &mut Frame<'_>, area: Rect, app: &App) {
    let content = app.terminal.renderable_content();
    let mut lines: Vec<Line<'_>> = Vec::new();
    let mut current_line: i32 = i32::MIN;
    let mut current_text: String = String::new();
    for indexed in content.display_iter {
        let line_idx = indexed.point.line.0;
        if line_idx != current_line {
            if current_line != i32::MIN {
                lines.push(Line::from(std::mem::take(&mut current_text)));
            }
            current_line = line_idx;
        }
        if !indexed
            .cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            current_text.push(indexed.cell.c);
        }
    }
    if current_line != i32::MIN {
        lines.push(Line::from(current_text));
    }

    f.render_widget(Paragraph::new(lines), area);

    if app.can_show_suggestion() {
        if let Some(suggestion) = &app.suggestion {
            let (cur_col, cur_row) = app.terminal.cursor();
            let x = area.x.saturating_add(cur_col);
            let y = area.y.saturating_add(cur_row);
            if x < area.right() && y < area.bottom() {
                let max_w = area.right().saturating_sub(x) as usize;
                let truncated: String = suggestion.chars().take(max_w).collect();
                let width = truncated.chars().count().min(max_w) as u16;
                if width > 0 {
                    let ghost_area = Rect {
                        x,
                        y,
                        width,
                        height: 1,
                    };
                    let style = Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM);
                    f.render_widget(
                        Paragraph::new(Line::from(Span::styled(truncated, style))),
                        ghost_area,
                    );
                }
            }
        }
    }
}

fn render_status(f: &mut Frame<'_>, area: Rect, app: &App) {
    let state_label = match &app.state {
        SessionState::Connecting => "connecting".to_string(),
        SessionState::Authenticated => "authenticated".to_string(),
        SessionState::ShellOpen => "shell open".to_string(),
        SessionState::Closed => "closed".to_string(),
        SessionState::Failed(msg) => format!("failed: {msg}"),
    };
    let cwd_label = app
        .cwd
        .as_ref()
        .map(|p| format!(" cwd:{}", p.display()))
        .unwrap_or_default();
    let mouse_label = if app.mouse_capture { "mouse" } else { "MOUSE-OFF" };
    let mut spans = vec![
        Span::styled(
            format!("[{state_label}]"),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(cwd_label),
        Span::raw(format!(" · {mouse_label} · → accept · Tab/Ctrl-* remote · Ctrl-Q quit")),
    ];
    if let Some(err) = &app.last_error {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            format!("err: {err}"),
            Style::default().fg(Color::Red),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
