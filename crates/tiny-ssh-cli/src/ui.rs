//! ratatui rendering for the App.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use tiny_ssh_core::SessionState;
use unicode_width_compat::UnicodeWidthCompat;

use crate::app::App;

pub fn render(f: &mut Frame<'_>, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // output
            Constraint::Length(3), // input box
            Constraint::Length(1), // status bar
        ])
        .split(area);

    render_output(f, chunks[0], app);
    render_input(f, chunks[1], app);
    render_status(f, chunks[2], app);
}

fn render_output(f: &mut Frame<'_>, area: Rect, app: &App) {
    let title = format!(" {}@{} — output ", app.user, app.host);
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);

    let visible_rows = inner.height as usize;
    let start = app.output.len().saturating_sub(visible_rows);
    let lines: Vec<Line<'_>> = app
        .output
        .iter()
        .skip(start)
        .map(|s| Line::from(s.clone()))
        .collect();

    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(block),
        area,
    );
}

fn render_input(f: &mut Frame<'_>, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title(" input ");
    let inner = block.inner(area);

    let mut spans: Vec<Span<'_>> = Vec::new();
    spans.push(Span::raw(app.input.clone()));
    if let Some(suggestion) = &app.suggestion {
        spans.push(Span::styled(
            suggestion.clone(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ));
    }

    f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);

    // Place the cursor at the user's logical position.
    let cursor_col = inner.x + display_width(&app.input, app.cursor);
    f.set_cursor_position((cursor_col.min(area.right().saturating_sub(1)), inner.y));
}

fn render_status(f: &mut Frame<'_>, area: Rect, app: &App) {
    let state_label = match &app.state {
        SessionState::Connecting => "connecting".to_string(),
        SessionState::Authenticated => "authenticated".to_string(),
        SessionState::ShellOpen => "shell open".to_string(),
        SessionState::Closed => "closed".to_string(),
        SessionState::Failed(msg) => format!("failed: {msg}"),
    };
    let mut spans = vec![
        Span::styled(
            format!("[{state_label}]"),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(" "),
        Span::raw("Tab: accept · Ctrl-D: EOF · Ctrl-Q: quit · Ctrl-L: clear"),
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

/// Width of the first `chars` characters of `s` in terminal cells.
fn display_width(s: &str, chars: usize) -> u16 {
    let prefix: String = s.chars().take(chars).collect();
    prefix.terminal_width() as u16
}

// Tiny shim to avoid pulling unicode-width into the workspace deps for one
// helper. ratatui already brings unicode-width transitively, but the public
// re-export name varies between versions; bind it here.
mod unicode_width_compat {
    pub trait UnicodeWidthCompat {
        fn terminal_width(&self) -> usize;
    }
    impl UnicodeWidthCompat for str {
        fn terminal_width(&self) -> usize {
            // Fallback: count chars (works for ASCII; CJK will under-count).
            // Acceptable for v0.1 — cursor placement is best-effort.
            self.chars().count()
        }
    }
}
