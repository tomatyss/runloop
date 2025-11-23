mod state;
mod ui;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event as CEvent};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use runloop_bus::{Bus, Message, PublisherKind};
use runloop_core::Config;
use runloop_core::content::{
    CT_ACTION_DECISION, CT_ACTION_REQUEST, CT_METRICS_SNAPSHOT, CT_RUN_EVENT,
};
use runloop_rmp::decode_payload;
use serde_json::Value;
use state::{ActionCommand, App, Event as AppEvent};
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use ui::draw_ui;

#[derive(Parser, Debug)]
#[command(name = "agtop", about = "Runloop agent telemetry viewer")]
struct Cli {
    /// Optional NDJSON file to read (replay mode).
    #[arg(short, long)]
    input: Option<PathBuf>,

    /// Connect to the daemon bus (live mode).
    #[arg(long)]
    connect: bool,

    /// Trace ID to monitor (required for live mode run events).
    #[arg(long)]
    trace_id: Option<String>,

    /// Socket path override
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Agents to monitor (comma-separated IDs)
    #[arg(long, value_delimiter = ',')]
    monitor_agents: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let mode = if cli.connect {
        AppMode::Live {
            trace_id: cli.trace_id.clone(),
            socket: cli.socket.clone(),
            monitor_agents: cli.monitor_agents.clone(),
        }
    } else {
        AppMode::Replay {
            path: cli.input.clone(),
        }
    };

    let (tx, rx) = mpsc::channel();
    let (action_tx, action_rx) = tokio::sync::mpsc::unbounded_channel();
    let tick_rate = Duration::from_millis(250);

    let mut app = App::new(cli.trace_id.clone().unwrap_or_default(), Some(action_tx));

    match mode {
        AppMode::Live {
            trace_id,
            socket,
            monitor_agents,
        } => {
            tokio::spawn(run_live_collector(
                tx.clone(),
                trace_id,
                socket,
                monitor_agents,
                action_rx,
            ));
        }
        AppMode::Replay { path } => {
            std::thread::spawn(move || {
                run_replay_collector(tx, path);
            });
        }
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_app(&mut terminal, &mut app, rx, tick_rate);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        eprintln!("Error: {err:?}");
    }

    Ok(())
}

enum AppMode {
    Live {
        trace_id: Option<String>,
        socket: Option<PathBuf>,
        monitor_agents: Vec<String>,
    },
    Replay {
        path: Option<PathBuf>,
    },
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    rx: mpsc::Receiver<AppEvent>,
    tick_rate: Duration,
) -> Result<()> {
    let mut last_tick = Instant::now();
    loop {
        terminal.draw(|f| draw_ui(f, app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if crossterm::event::poll(timeout)?
            && let CEvent::Key(key) = event::read()?
        {
            app.on_key(key);
        }

        if last_tick.elapsed() >= tick_rate {
            app.on_tick();
            last_tick = Instant::now();
        }

        while let Ok(event) = rx.try_recv() {
            app.handle_event(event);
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

async fn run_live_collector(
    tx: mpsc::Sender<AppEvent>,
    trace_id: Option<String>,
    socket_path: Option<PathBuf>,
    monitor_agents: Vec<String>,
    mut action_rx: tokio::sync::mpsc::UnboundedReceiver<ActionCommand>,
) {
    let path = if let Some(p) = socket_path {
        p
    } else {
        match Config::load() {
            Ok(cfg) => PathBuf::from(
                cfg.runtime
                    .socket_path
                    .unwrap_or_else(|| cfg.runtime.sockets_dir + "/rmp.sock"),
            ),
            Err(_) => PathBuf::from("/tmp/runloop/rmp.sock"),
        }
    };

    let bus = match Bus::connect_as(&path, PublisherKind::Tui).await {
        Ok(b) => b,
        Err(e) => {
            let _ = tx.send(AppEvent::Error(format!("Bus connect failed: {e}")));
            return;
        }
    };

    let _ = tx.send(AppEvent::BusConnected(bus.clone()));

    let mut metrics_sub = bus.subscribe("rlp/sys/metrics").await.expect("sub metrics");

    // Subscribe to agent metrics if provided
    // We need to hold the subscriptions
    let mut agent_subs = Vec::new();
    for agent_id in monitor_agents {
        let topic = format!("rlp/agents/{}/metrics", agent_id);
        if let Ok(sub) = bus.subscribe(&topic).await {
            agent_subs.push((agent_id, sub));
        }
    }

    let mut action_sub = bus
        .subscribe("action.request")
        .await
        .expect("sub action.request");

    let mut run_stream: std::pin::Pin<
        Box<dyn futures_util::Stream<Item = runloop_bus::Message> + Send>,
    > = if let Some(tid) = trace_id {
        let topic = format!("rlp/runs/{}/events", tid);
        let sub = bus.subscribe(&topic).await.expect("sub run events");
        Box::pin(sub)
    } else {
        Box::pin(futures_util::stream::pending())
    };

    let mut agent_stream = futures_util::stream::select_all(
        agent_subs
            .into_iter()
            .map(|(id, sub)| sub.map(move |msg| (id.clone(), msg))),
    );

    loop {
        tokio::select! {
            Some(msg) = metrics_sub.next() => {
                if msg.header.schema_id == CT_METRICS_SNAPSHOT
                    && let Ok(decoded) = decode_payload::<Value>(CT_METRICS_SNAPSHOT, &msg.body)
                {
                    let _ = tx.send(AppEvent::Metrics(decoded.payload, None));
                }
            }
            Some((agent_id, msg)) = agent_stream.next() => {
                if msg.header.schema_id == CT_METRICS_SNAPSHOT
                    && let Ok(decoded) = decode_payload::<Value>(CT_METRICS_SNAPSHOT, &msg.body)
                {
                    let _ = tx.send(AppEvent::Metrics(decoded.payload, Some(agent_id)));
                }
            }
            Some(msg) = action_sub.next() => {
                if msg.header.schema_id == CT_ACTION_REQUEST
                    && let Ok(decoded) = decode_payload::<Value>(CT_ACTION_REQUEST, &msg.body)
                {
                    let _ = tx.send(AppEvent::ActionRequest(msg.header, decoded.payload));
                }
            }
            Some(msg) = run_stream.next() => {
                if msg.header.schema_id == CT_RUN_EVENT
                    && let Ok(decoded) = decode_payload::<Value>(CT_RUN_EVENT, &msg.body)
                {
                    let _ = tx.send(AppEvent::Run(decoded.payload));
                }
            }
            Some(cmd) = action_rx.recv() => {
                let ActionCommand::PublishDecision(header, payload, topic) = cmd;
                if let Ok(body) = runloop_rmp::encode_payload(CT_ACTION_DECISION, &payload, None)
                    && let Ok(msg) = Message::new(header, body.into())
                {
                    let _ = bus.publish(&topic, msg).await;
                }
            }
        }
    }
}

fn run_replay_collector(tx: mpsc::Sender<AppEvent>, path: Option<PathBuf>) {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let reader: Box<dyn BufRead> = match path {
        Some(p) => {
            if let Ok(f) = File::open(p) {
                Box::new(BufReader::new(f))
            } else {
                let _ = tx.send(AppEvent::Error("Failed to open input file".into()));
                return;
            }
        }
        None => Box::new(BufReader::new(io::stdin().lock())),
    };

    for l in reader.lines().map_while(Result::ok) {
        if l.trim().is_empty() {
            continue;
        }
        if let Ok(json) = serde_json::from_str::<Value>(&l) {
            let _ = tx.send(AppEvent::Run(json));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
