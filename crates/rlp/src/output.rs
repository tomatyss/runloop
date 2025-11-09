use clap::Args;
use is_terminal::IsTerminal;
use serde_json::Value;
use std::io::{self, Write};
use terminal_size::{Height, Width, terminal_size};
use textwrap::Options as WrapOptions;
use unicode_width::UnicodeWidthStr;

const MIN_COL_WIDTH: usize = 8;
const DEFAULT_WIDTH: usize = 80;

#[derive(Args, Debug, Clone, Default)]
pub struct OutputArgs {
    /// Emit structured JSON output.
    #[arg(long, conflicts_with = "table")]
    pub json: bool,

    /// Force table output even when stdout is not a TTY.
    #[arg(long, conflicts_with = "json")]
    pub table: bool,

    /// Limit the number of rendered columns (default shows all).
    #[arg(long, value_name = "N")]
    pub max_cols: Option<usize>,

    /// Limit the number of rendered rows.
    #[arg(long, value_name = "N")]
    pub max_rows: Option<usize>,

    /// Disable soft-wrapping for cells; truncate instead.
    #[arg(long)]
    pub no_wrap: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputMode {
    Table,
    Json,
}

pub struct OutputSettings {
    pub mode: OutputMode,
    pub max_cols: Option<usize>,
    pub max_rows: Option<usize>,
    pub wrap: bool,
    stdout_is_tty: bool,
    stdin_is_tty: bool,
    term_width: usize,
    term_height: Option<usize>,
}

impl OutputArgs {
    pub fn resolve(&self) -> OutputSettings {
        let stdout_is_tty = std::io::stdout().is_terminal();
        let stdin_is_tty = std::io::stdin().is_terminal();
        let mode = if self.json {
            OutputMode::Json
        } else if self.table || stdout_is_tty {
            OutputMode::Table
        } else {
            OutputMode::Json
        };
        let (term_width, term_height) = detect_terminal_size();
        OutputSettings {
            mode,
            max_cols: self.max_cols,
            max_rows: self.max_rows,
            wrap: !self.no_wrap,
            stdout_is_tty,
            stdin_is_tty,
            term_width,
            term_height,
        }
    }
}

fn detect_terminal_size() -> (usize, Option<usize>) {
    if let Some((Width(w), Height(h))) = terminal_size() {
        (w.max(MIN_COL_WIDTH as u16) as usize, Some(h as usize))
    } else if let Some((Width(w), _)) = terminal_size() {
        (w as usize, None)
    } else {
        (DEFAULT_WIDTH, None)
    }
}

#[derive(Clone, Debug)]
pub struct Cell {
    pub value: String,
    pub numeric: bool,
}

impl Cell {
    pub fn text<T: Into<String>>(value: T) -> Self {
        Self {
            value: value.into(),
            numeric: false,
        }
    }

    pub fn number<T: Into<String>>(value: T) -> Self {
        Self {
            value: value.into(),
            numeric: true,
        }
    }
}

pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    pub footer_notes: Vec<String>,
}

impl Table {
    pub fn new(headers: Vec<String>) -> Self {
        Self {
            headers,
            rows: Vec::new(),
            footer_notes: Vec::new(),
        }
    }

    pub fn add_row(&mut self, cells: Vec<Cell>) {
        self.rows.push(cells);
    }

    pub fn add_note<T: Into<String>>(&mut self, note: T) {
        self.footer_notes.push(note.into());
    }
}

pub fn print_table(table: &Table, settings: &OutputSettings) -> io::Result<()> {
    let mut headers = table.headers.clone();
    if headers.is_empty() {
        return Ok(());
    }

    let mut rows = table.rows.clone();
    let mut truncated_cols_note = None;
    if let Some(max_cols) = settings
        .max_cols
        .filter(|&max_cols| max_cols < headers.len())
    {
        truncated_cols_note = Some(format!(
            "columns truncated to first {max_cols}; use --max-cols to adjust"
        ));
        headers.truncate(max_cols);
        for row in &mut rows {
            row.truncate(max_cols);
        }
    }

    let mut truncated_rows = false;
    if let Some(max_rows) = settings.max_rows.filter(|&max_rows| max_rows < rows.len()) {
        rows.truncate(max_rows);
        truncated_rows = true;
    }

    let column_count = headers.len();
    let header_cells: Vec<Cell> = headers.iter().map(|h| Cell::text(h.clone())).collect();
    let content_widths = measure_column_widths(&header_cells, &rows);
    let widths = fit_widths(content_widths, column_count, settings.term_width);

    let mut lines = Vec::new();
    lines.extend(format_row(&header_cells, &widths, settings.wrap));
    lines.push(render_separator(&widths));
    for row in &rows {
        let formatted = format_row(row, &widths, settings.wrap);
        for line in formatted {
            lines.push(line);
        }
    }

    if truncated_rows {
        lines.push(format!(
            "… {} more row(s) (use --max-rows to adjust)",
            table.rows.len() - rows.len()
        ));
    }

    if let Some(note) = truncated_cols_note {
        lines.push(note);
    }
    for note in &table.footer_notes {
        lines.push(note.clone());
    }

    paginate_and_print(
        lines,
        settings.stdout_is_tty && settings.stdin_is_tty,
        settings.term_height,
    )
}

fn measure_column_widths(headers: &[Cell], rows: &[Vec<Cell>]) -> Vec<usize> {
    let mut widths = headers
        .iter()
        .map(|h| UnicodeWidthStr::width(h.value.as_str()))
        .collect::<Vec<_>>();
    for row in rows {
        for (idx, cell) in row.iter().enumerate().take(widths.len()) {
            let width = UnicodeWidthStr::width(cell.value.as_str());
            widths[idx] = widths[idx].max(width);
        }
    }
    for width in widths.iter_mut() {
        *width = (*width).max(MIN_COL_WIDTH);
    }
    widths
}

fn fit_widths(mut widths: Vec<usize>, columns: usize, total_width: usize) -> Vec<usize> {
    if columns == 0 {
        return widths;
    }
    let separators = columns.saturating_sub(1) * 3;
    let available = total_width
        .saturating_sub(separators)
        .max(columns * MIN_COL_WIDTH);
    let mut current: usize = widths.iter().sum();
    if current <= available {
        return widths;
    }
    while current > available {
        if let Some((idx, _)) = widths.iter().enumerate().max_by(|a, b| a.1.cmp(b.1)) {
            if widths[idx] <= MIN_COL_WIDTH {
                break;
            }
            widths[idx] -= 1;
            current -= 1;
        } else {
            break;
        }
    }
    widths
}

fn format_row(row: &[Cell], widths: &[usize], wrap: bool) -> Vec<String> {
    let mut per_cell = Vec::new();
    let mut max_lines = 1;
    for (idx, cell) in row.iter().enumerate() {
        let width = widths.get(idx).cloned().unwrap_or(MIN_COL_WIDTH);
        let lines = format_cell(&cell.value, width, wrap);
        max_lines = max_lines.max(lines.len());
        per_cell.push((lines, cell.numeric));
    }
    let mut rows = Vec::new();
    for line_idx in 0..max_lines {
        let mut line = String::new();
        for (col_idx, (lines, numeric)) in per_cell.iter().enumerate() {
            if col_idx > 0 {
                line.push_str(" | ");
            }
            let width = widths[col_idx];
            let content = lines.get(line_idx).map(|s| s.as_str()).unwrap_or("");
            let display_width = UnicodeWidthStr::width(content);
            let padding = width.saturating_sub(display_width);
            if *numeric {
                line.push_str(&" ".repeat(padding));
                line.push_str(content);
            } else {
                line.push_str(content);
                line.push_str(&" ".repeat(padding));
            }
        }
        rows.push(line);
    }
    rows
}

fn format_cell(value: &str, width: usize, wrap: bool) -> Vec<String> {
    if value.is_empty() {
        return vec![String::new()];
    }
    if wrap {
        let options = WrapOptions::new(width).break_words(false);
        textwrap::wrap(value, &options)
            .into_iter()
            .map(|cow| cow.into_owned())
            .collect()
    } else if UnicodeWidthStr::width(value) <= width {
        vec![value.to_string()]
    } else {
        let mut truncated = value.chars().collect::<Vec<_>>();
        truncated.truncate(width.saturating_sub(1));
        let mut rendered = truncated.into_iter().collect::<String>();
        rendered.push('…');
        vec![rendered]
    }
}

fn render_separator(widths: &[usize]) -> String {
    let mut parts = Vec::new();
    for width in widths {
        parts.push("-".repeat(*width));
    }
    parts.join("-+-")
}

fn paginate_and_print(
    lines: Vec<String>,
    paginate: bool,
    term_height: Option<usize>,
) -> io::Result<()> {
    if !paginate {
        let mut stdout = io::stdout();
        for line in lines {
            writeln!(stdout, "{line}")?;
        }
        stdout.flush()
    } else {
        let page_len = term_height.unwrap_or(24).saturating_sub(2).max(1);
        let mut stdout = io::stdout();
        let mut idx = 0;
        while idx < lines.len() {
            let end = (idx + page_len).min(lines.len());
            for line in &lines[idx..end] {
                writeln!(stdout, "{line}")?;
            }
            stdout.flush()?;
            idx = end;
            if idx < lines.len() {
                eprint!("-- more -- (press q to quit, any key to continue) ");
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                let trimmed = input.trim().to_ascii_lowercase();
                if trimmed == "q" || trimmed == "quit" {
                    break;
                }
            }
        }
        Ok(())
    }
}

pub fn print_json(value: &Value) -> serde_json::Result<()> {
    let rendered = serde_json::to_string_pretty(value)?;
    println!("{rendered}");
    Ok(())
}

pub fn display_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(num) => num.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".into(),
        _ => value.to_string(),
    }
}

pub fn summarize_json(value: &Value, max_len: usize) -> String {
    let raw = display_value(value);
    if raw.len() <= max_len {
        raw
    } else {
        let mut truncated = raw
            .chars()
            .take(max_len.saturating_sub(1))
            .collect::<String>();
        truncated.push('…');
        truncated
    }
}
