use crate::state::{App, ConfirmModal, Pane};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Wrap};

pub fn draw_ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)].as_ref())
        .split(f.area());

    draw_status_bar(f, app, chunks[0]);
    draw_main_content(f, app, chunks[1]);

    if app.show_help {
        draw_help(f);
    }

    if let Some(modal) = &app.confirm_modal {
        draw_confirm(f, modal);
    }
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let mode_str = if app.bus.is_some() { "LIVE" } else { "REPLAY" };
    let status_style = if app.bus.is_some() {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::Yellow)
    };

    let sys_metrics = app.state.metrics.get("system");
    let agents_running = sys_metrics
        .and_then(|g| g.values.get("agents_running"))
        .map(String::from)
        .unwrap_or_else(|| "N/A".into());
    let bus_queue = sys_metrics
        .and_then(|g| g.values.get("bus_queue_depth"))
        .map(String::from)
        .unwrap_or_else(|| "N/A".into());

    let confirm_status = if app.confirm_modal.is_some() {
        Span::styled(
            "CONFIRM REQUIRED (!)",
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK),
        )
    } else {
        Span::raw("")
    };

    let filter_status = if let Some(input) = &app.filter_input {
        format!("FILTER: {input}_")
    } else {
        String::new()
    };

    let info = vec![
        Line::from(vec![
            Span::styled("Mode: ", Style::default().fg(Color::Gray)),
            Span::styled(mode_str, status_style.add_modifier(Modifier::BOLD)),
            Span::raw(" | "),
            Span::styled(
                format!("Trace: {}", app.trace_id),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(" | "),
            Span::styled(
                format!("agents:{} queue:{}", agents_running, bus_queue),
                Style::default().fg(Color::Green),
            ),
            Span::raw(" | "),
            confirm_status,
        ]),
        Line::from(vec![
            Span::raw(format!("Active: {:?} | ", app.active_pane)),
            Span::raw(if app.paused { "PAUSED | " } else { "" }),
            Span::styled(filter_status, Style::default().fg(Color::Yellow)),
            Span::raw("Press '?' for help, 'q' to quit"),
        ]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Runloop Monitor");
    f.render_widget(Paragraph::new(info).block(block), area);
}

fn draw_main_content(f: &mut Frame, app: &App, area: Rect) {
    match app.active_pane {
        Pane::Log => draw_logs(f, app, area),
        Pane::Plan => draw_plan(f, app, area),
        Pane::Metrics => draw_metrics(f, app, area),
        Pane::Trace => draw_trace(f, app, area),
    }
}

fn draw_logs(f: &mut Frame, app: &App, area: Rect) {
    let filter = app.filter_input.as_deref().unwrap_or("");
    let items: Vec<ListItem> = app
        .state
        .logs
        .iter()
        .filter(|l| filter.is_empty() || l.msg.contains(filter) || l.level.contains(filter))
        .rev() // Show newest at bottom usually, but list renders top-down. Let's keep standard order but scroll?
        // Actually typical logs: new at bottom. List renders top-down.
        // We'll just reverse to show newest at top for simplicity, or implement auto-scroll.
        // For TUI, newest at top is often easier without stateful scroll.
        // Let's do newest at top.
        .map(|log| {
            let color = match log.level.as_str() {
                "ERROR" => Color::Red,
                "WARN" => Color::Yellow,
                "INFO" => Color::Green,
                _ => Color::White,
            };
            let content = format!(
                "[{}] {:<5} {} {}",
                log.ts,
                log.level,
                log.node.as_deref().unwrap_or("-"),
                log.msg
            );
            ListItem::new(Line::from(Span::styled(
                content,
                Style::default().fg(color),
            )))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Logs")
        .border_style(active_style(app.active_pane == Pane::Log));
    f.render_widget(List::new(items).block(block), area);
}

fn draw_plan(f: &mut Frame, app: &App, area: Rect) {
    let filter = app.filter_input.as_deref().unwrap_or("");
    let header_cells = [
        "Node", "Status", "Attempt", "Duration", "Errors", "In", "Out",
    ]
    .iter()
    .map(|h| Cell::from(*h).style(Style::default().fg(Color::Yellow)));
    let header = Row::new(header_cells).height(1).bottom_margin(1);

    let rows = app
        .state
        .plan
        .values()
        .filter(|n| filter.is_empty() || n.id.contains(filter))
        .map(|node| {
            let status_color = match node.status.as_str() {
                "ok" | "succeeded" => Color::Green,
                "error" | "failed" => Color::Red,
                "running" => Color::Blue,
                "pending" => Color::Gray,
                _ => Color::White,
            };
            let cells = vec![
                Cell::from(node.id.clone()),
                Cell::from(node.status.clone()).style(Style::default().fg(status_color)),
                Cell::from(node.attempt.to_string()),
                Cell::from(format!("{}ms", node.duration)),
                Cell::from(node.error_count.to_string()),
                Cell::from(node.inputs.len().to_string()),
                Cell::from(node.outputs.len().to_string()),
            ];
            Row::new(cells).height(1)
        });

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(18),
            Constraint::Percentage(16),
            Constraint::Percentage(8),
            Constraint::Percentage(16),
            Constraint::Percentage(8),
            Constraint::Percentage(17),
            Constraint::Percentage(17),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Plan (DAG)")
            .border_style(active_style(app.active_pane == Pane::Plan)),
    );
    f.render_widget(table, area);
}

fn draw_metrics(f: &mut Frame, app: &App, area: Rect) {
    let filter = app.filter_input.as_deref().unwrap_or("");
    let mut rows = Vec::new();

    for (scope, group) in &app.state.metrics {
        if filter.is_empty() || scope.contains(filter) {
            rows.push(Row::new(vec![
                Cell::from(Span::styled(
                    format!("-- {} --", scope.to_uppercase()),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                )),
                Cell::from(""),
            ]));
        }
        for (k, v) in &group.values {
            if filter.is_empty() || scope.contains(filter) || k.contains(filter) {
                rows.push(Row::new(vec![Cell::from(k.clone()), Cell::from(v.clone())]));
            }
        }
    }

    let table = Table::new(
        rows,
        [Constraint::Percentage(50), Constraint::Percentage(50)],
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Metrics")
            .border_style(active_style(app.active_pane == Pane::Metrics)),
    );
    f.render_widget(table, area);
}

fn draw_trace(f: &mut Frame, app: &App, area: Rect) {
    let filter = app.filter_input.as_deref().unwrap_or("");
    let items: Vec<ListItem> = app
        .state
        .trace
        .iter()
        .filter(|t| filter.is_empty() || t.msg.contains(filter) || t.kind.contains(filter))
        .rev() // Newest first
        .map(|t| ListItem::new(format!("[{}] {}: {}", t.ts, t.kind, t.msg)))
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Trace Ladder")
        .border_style(active_style(app.active_pane == Pane::Trace));
    f.render_widget(List::new(items).block(block), area);
}

fn draw_help(f: &mut Frame) {
    let area = centered_rect(60, 50, f.area());
    let text = vec![
        Line::from("Keybindings:"),
        Line::from("  q, Ctrl+c  : Quit"),
        Line::from("  ?          : Toggle Help"),
        Line::from("  Tab        : Next Pane"),
        Line::from("  Shift+Tab  : Prev Pane"),
        Line::from("  /          : Filter Pane"),
        Line::from("  .          : Pause/Resume"),
        Line::from("  !          : Clear Pane"),
        Line::from(""),
        Line::from("Confirmation:"),
        Line::from("  Enter      : Approve Action"),
        Line::from("  Esc        : Reject Action"),
    ];
    let block = Block::default()
        .title("Help")
        .borders(Borders::ALL)
        .bg(Color::Blue);
    let p = Paragraph::new(text).block(block).wrap(Wrap { trim: true });
    f.render_widget(Clear, area); // Clear background
    f.render_widget(p, area);
}

fn draw_confirm(f: &mut Frame, modal: &ConfirmModal) {
    let area = centered_rect(60, 40, f.area());
    let proposal_str = serde_json::to_string_pretty(&modal.proposal).unwrap_or_default();
    let text = vec![
        Line::from(Span::styled(
            "Action Confirmation Required",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("An agent is requesting permission to perform an action."),
        Line::from(""),
        Line::from(proposal_str),
        Line::from(""),
        Line::from(Span::styled(
            "Press ENTER to Approve, ESC to Reject",
            Style::default().fg(Color::Yellow),
        )),
    ];

    let block = Block::default()
        .title("Confirmation Request")
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::DarkGray));

    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn active_style(active: bool) -> Style {
    if active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::White)
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
