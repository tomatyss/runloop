use agtop::init_tracing;
use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

fn main() -> Result<()> {
    let _ = init_tracing();
    let cli = Cli::parse();
    run(cli)
}

#[derive(Parser, Debug)]
#[command(name = "agtop", about = "Runloop agent telemetry viewer")]
struct Cli {
    /// Optional NDJSON file to read; defaults to stdin.
    #[arg(short, long)]
    input: Option<PathBuf>,
}

fn run(cli: Cli) -> Result<()> {
    let label = cli
        .input
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "stdin".into());
    let (tx, rx) = mpsc::channel();
    let input_path = cli.input.clone();
    thread::spawn(move || {
        if let Err(err) = read_stream(input_path.as_deref(), tx) {
            eprintln!("agtop input error: {err}");
        }
    });
    let mut state = DashboardState::default();
    render_loop(&label, &mut state, rx)
}

fn read_stream(path: Option<&Path>, tx: mpsc::Sender<NdjsonRecord>) -> io::Result<()> {
    let reader: Box<dyn BufRead> = match path {
        Some(p) => Box::new(BufReader::new(File::open(p)?)),
        None => Box::new(BufReader::new(io::stdin().lock())),
    };
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<NdjsonRecord>(&line) {
            Ok(record) => {
                if tx.send(record).is_err() {
                    break;
                }
            }
            Err(err) => eprintln!("invalid NDJSON line: {err}"),
        }
    }
    Ok(())
}

fn render_loop(label: &str, state: &mut DashboardState, rx: Receiver<NdjsonRecord>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut stream_closed = false;
    loop {
        if !stream_closed {
            loop {
                match rx.try_recv() {
                    Ok(record) => state.apply(record),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        stream_closed = true;
                        break;
                    }
                }
            }
        }

        terminal.draw(|frame| draw(frame, label, state, stream_closed))?;

        if crossterm::event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    break;
                }
                _ => {}
            }
        }

        if stream_closed && state.nodes.is_empty() {
            break;
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

fn draw(frame: &mut ratatui::Frame, label: &str, state: &DashboardState, closed: bool) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(4)])
        .split(frame.area());

    let status = format!(
        "input: {label} | last run: {} | opening: {} | stream: {}",
        state.last_run_id.as_deref().unwrap_or("<none>"),
        state.last_opening.as_deref().unwrap_or("<unknown>"),
        if closed { "closed" } else { "live" }
    );
    let ts = state
        .last_event_ts
        .map(|ts| format!("{} ms", ts))
        .unwrap_or_else(|| "n/a".into());
    let summary = Paragraph::new(vec![
        Line::from(status),
        Line::from(format!(
            "last event @ {ts} | total events {}",
            state.total_events
        )),
        Line::from("press 'q' to exit"),
    ])
    .block(Block::default().borders(Borders::ALL).title("runloop"));
    frame.render_widget(summary, chunks[0]);

    let mut rows = Vec::new();
    for (name, stats) in state.sorted_nodes() {
        rows.push(Row::new(vec![
            Cell::from(name.clone()),
            Cell::from(stats.status.clone()),
            Cell::from(stats.attempt.to_string()),
            Cell::from(stats.duration_ms.to_string()),
            Cell::from(stats.last_level.clone()),
            Cell::from(truncate(&stats.last_message, 60)),
        ]));
    }

    if rows.is_empty() {
        rows.push(Row::new(vec![
            Cell::from("<waiting>"),
            Cell::from("-"),
            Cell::from("-"),
            Cell::from("-"),
            Cell::from("-"),
            Cell::from("-"),
        ]));
    }
    let widths = [
        Constraint::Length(18),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(12),
        Constraint::Length(8),
        Constraint::Min(10),
    ];
    let table = Table::new(rows, widths)
        .header(
            Row::new(vec![
                "node",
                "status",
                "attempt",
                "duration_ms",
                "level",
                "message",
            ])
            .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(Block::default().borders(Borders::ALL).title("agents"));
    frame.render_widget(table, chunks[1]);
}

#[derive(Debug, Deserialize)]
struct NdjsonRecord {
    ts_ms: u64,
    #[serde(rename = "trace_id")]
    _trace_id: String,
    run_id: String,
    opening_id: String,
    kind: String,
    level: String,
    message: String,
    #[serde(default)]
    meta: Value,
}

#[derive(Default)]
struct DashboardState {
    nodes: HashMap<String, NodeStats>,
    last_run_id: Option<String>,
    last_opening: Option<String>,
    total_events: usize,
    last_event_ts: Option<u64>,
}

impl DashboardState {
    fn apply(&mut self, record: NdjsonRecord) {
        self.total_events += 1;
        self.last_event_ts = Some(record.ts_ms);
        if record.kind == "run.started" {
            self.last_run_id = Some(record.run_id.clone());
            let opening = record
                .meta
                .get("opening_name")
                .and_then(Value::as_str)
                .map(|s| s.to_string())
                .unwrap_or_else(|| record.opening_id.clone());
            self.last_opening = Some(opening);
        }

        if let Some(node_name) = record.meta.get("node").and_then(Value::as_str) {
            let entry = self.nodes.entry(node_name.to_string()).or_default();
            entry.last_ts = record.ts_ms;
            entry.last_level = record.level.clone();
            let message = record
                .meta
                .get("chunk")
                .and_then(Value::as_str)
                .unwrap_or(&record.message);
            entry.last_message = message.to_string();

            if let Some(status) = record.meta.get("status").and_then(Value::as_str) {
                entry.status = status.to_string();
            }
            if let Some(attempt) = record.meta.get("attempt").and_then(Value::as_u64) {
                entry.attempt = attempt as u32;
            }
            if let Some(duration) = record.meta.get("duration_ms").and_then(Value::as_u64) {
                entry.duration_ms = duration;
            }
            if record.level.eq_ignore_ascii_case("error") {
                entry.errors = entry.errors.saturating_add(1);
            }
        }
    }

    fn sorted_nodes(&self) -> Vec<(String, &NodeStats)> {
        let mut pairs = self
            .nodes
            .iter()
            .map(|(k, v)| (k.clone(), v))
            .collect::<Vec<_>>();
        pairs.sort_by_key(|(_, stats)| std::cmp::Reverse(stats.last_ts));
        pairs
    }
}

#[derive(Clone, Default)]
struct NodeStats {
    status: String,
    attempt: u32,
    duration_ms: u64,
    last_level: String,
    last_message: String,
    last_ts: u64,
    errors: u32,
}

fn truncate(input: &str, max: usize) -> String {
    if input.len() <= max {
        input.to_string()
    } else {
        let mut slice = input
            .chars()
            .take(max.saturating_sub(3))
            .collect::<String>();
        slice.push_str("...");
        slice
    }
}
