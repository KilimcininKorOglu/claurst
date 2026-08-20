//! External status line: a user-configured shell command whose stdout is
//! rendered in its own row above the footer.
//!
//! The command receives the session state as JSON on stdin, the same contract
//! ready-made status line scripts expect. It runs on state changes rather than
//! on a timer, so an idle session spawns nothing.

use std::time::Duration;

use mikmik_core::config::StatusLineConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Changes that arrive within this window collapse into one run.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// A script that neither finishes nor prints is abandoned after this long.
const RUN_TIMEOUT: Duration = Duration::from_secs(10);
/// Output beyond this is dropped; the reader closes the pipe at the limit.
const MAX_OUTPUT_BYTES: u64 = 4096;

/// One run request: the payload to feed the command and the terminal size to
/// expose through `COLUMNS` / `LINES`.
#[derive(Debug, Clone)]
pub struct Request {
    pub payload: String,
    pub columns: u16,
    pub rows: u16,
}

/// Handle held by the interactive loop.
pub struct StatusLine {
    tx: mpsc::Sender<Request>,
    cancel: CancellationToken,
    refresh_interval: Option<Duration>,
}

impl StatusLine {
    /// Start the runner. Returns `None` when the configuration does not name a
    /// command to run.
    pub fn spawn(config: &StatusLineConfig, out: mpsc::Sender<String>) -> Option<Self> {
        if !config.is_command() {
            return None;
        }
        let (tx, rx) = mpsc::channel::<Request>(4);
        let cancel = CancellationToken::new();
        let command = config.command.clone();
        let task_cancel = cancel.clone();
        tokio::spawn(async move { run(command, rx, out, task_cancel).await });
        Some(Self {
            tx,
            cancel,
            // The documented minimum is one second.
            refresh_interval: config
                .refresh_interval
                .map(|secs| Duration::from_secs(secs.max(1))),
        })
    }

    /// Queue a run. Drops the request when the runner is already backed up,
    /// because a newer state will follow on the next loop turn anyway.
    pub fn request(&self, payload: String, columns: u16, rows: u16) {
        let _ = self.tx.try_send(Request {
            payload,
            columns,
            rows,
        });
    }

    pub fn refresh_interval(&self) -> Option<Duration> {
        self.refresh_interval
    }

    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

enum Outcome {
    /// The command finished (or hit the output limit) and printed this.
    Output(String),
    /// A newer request arrived mid-run; the command was killed.
    Superseded(Request),
    /// The session is shutting down.
    Cancelled,
    /// The command could not be started, or produced nothing usable.
    Failed,
}

async fn run(
    command: String,
    mut rx: mpsc::Receiver<Request>,
    out: mpsc::Sender<String>,
    cancel: CancellationToken,
) {
    let mut pending: Option<Request> = None;
    loop {
        let mut request = match pending.take() {
            Some(request) => request,
            None => tokio::select! {
                _ = cancel.cancelled() => return,
                received = rx.recv() => match received {
                    Some(request) => request,
                    None => return,
                },
            },
        };

        // Collapse a burst of state changes into a single run.
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                received = rx.recv() => match received {
                    Some(newer) => request = newer,
                    None => return,
                },
                _ = tokio::time::sleep(DEBOUNCE) => break,
            }
        }

        match run_once(&command, &request, &mut rx, &cancel).await {
            Outcome::Output(text) => {
                if out.send(text).await.is_err() {
                    return;
                }
            }
            Outcome::Superseded(newer) => pending = Some(newer),
            Outcome::Cancelled => return,
            Outcome::Failed => {}
        }
    }
}

async fn run_once(
    command: &str,
    request: &Request,
    rx: &mut mpsc::Receiver<Request>,
    cancel: &CancellationToken,
) -> Outcome {
    let mut builder = if cfg!(target_os = "windows") {
        let mut builder = tokio::process::Command::new("cmd");
        builder.args(["/C", command]);
        builder
    } else {
        let mut builder = tokio::process::Command::new("sh");
        builder.args(["-c", command]);
        builder
    };
    // Scripts cannot measure the terminal, because their output is captured
    // rather than attached to it. These two variables are how they size it.
    builder
        .env("COLUMNS", request.columns.to_string())
        .env("LINES", request.rows.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());

    let Ok(mut child) = builder.spawn() else {
        return Outcome::Failed;
    };

    if let Some(mut stdin) = child.stdin.take() {
        // A command that ignores its stdin closes the pipe, which surfaces here
        // as a broken-pipe write. That is normal for something like `date`, so
        // the payload is best-effort and the command still gets to run.
        let _ = stdin.write_all(request.payload.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }

    let Some(stdout) = child.stdout.take() else {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Outcome::Failed;
    };

    // Reading to the cap (rather than waiting on the process) bounds memory and
    // frees a command that would otherwise fill the pipe and block forever.
    let mut buffer = Vec::new();
    let mut limited = stdout.take(MAX_OUTPUT_BYTES);
    let outcome = tokio::select! {
        result = limited.read_to_end(&mut buffer) => match result {
            Ok(_) => None,
            Err(_) => Some(Outcome::Failed),
        },
        _ = cancel.cancelled() => Some(Outcome::Cancelled),
        received = rx.recv() => match received {
            Some(newer) => Some(Outcome::Superseded(newer)),
            None => Some(Outcome::Cancelled),
        },
        _ = tokio::time::sleep(RUN_TIMEOUT) => Some(Outcome::Failed),
    };

    // Kill unconditionally: the command may still be running after the read cap
    // was reached, and leaving it behind would leak a process per update.
    let _ = child.start_kill();
    let _ = child.wait().await;

    match outcome {
        Some(other) => other,
        None => Outcome::Output(String::from_utf8_lossy(&buffer).trim_end().to_string()),
    }
}

/// The part of the session state that decides *whether* to re-run the command.
///
/// Wall-clock values are deliberately absent: including them would make every
/// loop turn look like a change and turn the event-driven contract back into a
/// busy poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerKey {
    model: String,
    cwd: String,
    permission_mode: String,
    vim_mode: Option<String>,
    output_style: String,
    effort: String,
    context_used_tokens: u64,
    cost_bits: u64,
    message_count: usize,
    streaming: bool,
}

/// Everything the payload needs, collected once per change.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub trigger: TriggerKey,
    pub session_id: String,
    pub transcript_path: Option<String>,
    pub project_dir: String,
    pub duration_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub context_window_size: u64,
}

impl Snapshot {
    /// The JSON handed to the command on stdin.
    pub fn payload(&self) -> String {
        let total_input = self.input_tokens + self.cache_creation_tokens + self.cache_read_tokens;
        let used_percentage = (self.context_window_size > 0).then(|| {
            (self.trigger.context_used_tokens as f64 / self.context_window_size as f64) * 100.0
        });
        let mut value = serde_json::json!({
            "session_id": self.session_id,
            "version": env!("CARGO_PKG_VERSION"),
            "cwd": self.trigger.cwd,
            "workspace": {
                "current_dir": self.trigger.cwd,
                "project_dir": self.project_dir,
            },
            "model": {
                "id": self.trigger.model,
                "display_name": self.trigger.model,
            },
            "permission_mode": self.trigger.permission_mode,
            "output_style": { "name": self.trigger.output_style },
            "effort": { "level": self.trigger.effort },
            "cost": {
                "total_cost_usd": f64::from_bits(self.trigger.cost_bits),
                "total_duration_ms": self.duration_ms,
            },
            "context_window": {
                "total_input_tokens": total_input,
                "total_output_tokens": self.output_tokens,
                "context_window_size": self.context_window_size,
                "used_percentage": used_percentage,
                "remaining_percentage": used_percentage.map(|used| 100.0 - used),
                "current_usage": {
                    "input_tokens": self.input_tokens,
                    "output_tokens": self.output_tokens,
                    "cache_creation_input_tokens": self.cache_creation_tokens,
                    "cache_read_input_tokens": self.cache_read_tokens,
                },
            },
            "exceeds_200k_tokens": total_input + self.output_tokens > 200_000,
        });
        if let Some(path) = &self.transcript_path {
            value["transcript_path"] = serde_json::Value::String(path.clone());
        }
        if let Some(mode) = &self.trigger.vim_mode {
            value["vim"] = serde_json::json!({ "mode": mode });
        }
        value.to_string()
    }
}

/// Collect the current state of an interactive session.
pub fn snapshot(
    app: &mikmik_tui::app::App,
    session_id: &str,
    transcript_path: Option<String>,
    project_dir: &str,
    message_count: usize,
) -> Snapshot {
    let cost_tracker = &app.cost_tracker;
    Snapshot {
        trigger: TriggerKey {
            model: app.model_name.clone(),
            cwd: app.current_dir.clone().unwrap_or_default(),
            permission_mode: format!("{:?}", app.config.permission_mode),
            vim_mode: vim_mode_name(app),
            output_style: app.output_style.clone(),
            effort: app.effort_level.as_str().to_string(),
            context_used_tokens: app.context_used_tokens,
            cost_bits: app.cost_usd.to_bits(),
            message_count,
            streaming: app.is_streaming,
        },
        session_id: session_id.to_string(),
        transcript_path,
        project_dir: project_dir.to_string(),
        duration_ms: app.session_start.elapsed().as_millis() as u64,
        input_tokens: cost_tracker.input_tokens(),
        output_tokens: cost_tracker.output_tokens(),
        cache_creation_tokens: cost_tracker.cache_creation_tokens(),
        cache_read_tokens: cost_tracker.cache_read_tokens(),
        context_window_size: app.context_window_size,
    }
}

fn vim_mode_name(app: &mikmik_tui::app::App) -> Option<String> {
    use mikmik_tui::prompt_input::VimMode;
    if !app.prompt_input.vim_enabled {
        return None;
    }
    Some(
        match app.prompt_input.vim_mode {
            VimMode::Insert => "INSERT",
            VimMode::Normal => "NORMAL",
            VimMode::Visual => "VISUAL",
            VimMode::VisualLine => "VISUAL LINE",
            VimMode::VisualBlock => "VISUAL BLOCK",
            VimMode::Command => "COMMAND",
            VimMode::Search => "SEARCH",
        }
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(command: &str) -> StatusLineConfig {
        StatusLineConfig {
            kind: "command".to_string(),
            command: command.to_string(),
            padding: None,
            refresh_interval: None,
            hide_vim_mode_indicator: false,
        }
    }

    fn snapshot_fixture() -> Snapshot {
        Snapshot {
            trigger: TriggerKey {
                model: "claude-opus-5".to_string(),
                cwd: "/work/project".to_string(),
                permission_mode: "Default".to_string(),
                vim_mode: None,
                output_style: "auto".to_string(),
                effort: "high".to_string(),
                context_used_tokens: 40_000,
                cost_bits: 1.25f64.to_bits(),
                message_count: 4,
                streaming: false,
            },
            session_id: "abc-123".to_string(),
            transcript_path: Some("/transcripts/abc-123.jsonl".to_string()),
            project_dir: "/work".to_string(),
            duration_ms: 61_000,
            input_tokens: 1_000,
            output_tokens: 500,
            cache_creation_tokens: 200,
            cache_read_tokens: 300,
            context_window_size: 200_000,
        }
    }

    #[test]
    fn the_payload_carries_the_documented_fields() {
        let value: serde_json::Value =
            serde_json::from_str(&snapshot_fixture().payload()).expect("valid json");

        assert_eq!(value["session_id"], "abc-123");
        assert_eq!(value["transcript_path"], "/transcripts/abc-123.jsonl");
        assert_eq!(value["cwd"], "/work/project");
        assert_eq!(value["workspace"]["current_dir"], "/work/project");
        assert_eq!(value["workspace"]["project_dir"], "/work");
        assert_eq!(value["model"]["display_name"], "claude-opus-5");
        assert_eq!(value["cost"]["total_cost_usd"], 1.25);
        assert_eq!(value["cost"]["total_duration_ms"], 61_000);
        assert_eq!(value["context_window"]["context_window_size"], 200_000);
        // Input counts cache reads and writes.
        assert_eq!(value["context_window"]["total_input_tokens"], 1_500);
        assert_eq!(value["context_window"]["used_percentage"], 20.0);
        assert_eq!(value["context_window"]["remaining_percentage"], 80.0);
        assert_eq!(value["exceeds_200k_tokens"], false);
        assert_eq!(value["output_style"]["name"], "auto");
        assert_eq!(value["effort"]["level"], "high");
        assert!(value.get("vim").is_none(), "vim is absent when off");
    }

    #[test]
    fn an_unknown_context_window_leaves_the_percentages_null() {
        let mut snapshot = snapshot_fixture();
        snapshot.context_window_size = 0;

        let value: serde_json::Value =
            serde_json::from_str(&snapshot.payload()).expect("valid json");

        assert!(value["context_window"]["used_percentage"].is_null());
        assert!(value["context_window"]["remaining_percentage"].is_null());
    }

    #[test]
    fn vim_mode_appears_only_when_vim_is_on() {
        let mut snapshot = snapshot_fixture();
        snapshot.trigger.vim_mode = Some("NORMAL".to_string());

        let value: serde_json::Value =
            serde_json::from_str(&snapshot.payload()).expect("valid json");

        assert_eq!(value["vim"]["mode"], "NORMAL");
    }

    #[tokio::test]
    async fn a_command_receives_the_payload_on_stdin() {
        let (tx, mut rx) = mpsc::channel::<String>(4);
        let status_line = StatusLine::spawn(&config("cat"), tx).expect("runner");

        status_line.request("{\"session_id\":\"xyz\"}".to_string(), 80, 24);
        let text = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("no timeout")
            .expect("output");

        assert_eq!(text, "{\"session_id\":\"xyz\"}");
        status_line.shutdown();
    }

    #[tokio::test]
    async fn the_terminal_size_reaches_the_command() {
        let (tx, mut rx) = mpsc::channel::<String>(4);
        let status_line = StatusLine::spawn(&config("echo $COLUMNS/$LINES"), tx).expect("runner");

        status_line.request("{}".to_string(), 120, 40);
        let text = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("no timeout")
            .expect("output");

        assert_eq!(text, "120/40");
        status_line.shutdown();
    }

    #[tokio::test]
    async fn output_stops_at_the_cap() {
        let (tx, mut rx) = mpsc::channel::<String>(4);
        let status_line =
            StatusLine::spawn(&config("yes abcdefgh | head -c 1000000"), tx).expect("runner");

        status_line.request("{}".to_string(), 80, 24);
        let text = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("no timeout")
            .expect("output");

        assert_eq!(text.len() as u64, MAX_OUTPUT_BYTES);
        status_line.shutdown();
    }

    #[tokio::test]
    async fn a_hanging_command_does_not_hold_the_runner() {
        let (tx, mut rx) = mpsc::channel::<String>(4);
        let status_line = StatusLine::spawn(&config("sleep 300"), tx).expect("runner");

        status_line.request("{}".to_string(), 80, 24);
        // The runner abandons it, so nothing is published within the window.
        let idle = tokio::time::timeout(Duration::from_millis(800), rx.recv()).await;
        assert!(idle.is_err(), "a hanging command must publish nothing");

        status_line.shutdown();
    }

    #[tokio::test]
    async fn a_burst_of_requests_runs_the_command_once() {
        let (tx, mut rx) = mpsc::channel::<String>(4);
        let status_line = StatusLine::spawn(&config("echo tick"), tx).expect("runner");

        for _ in 0..4 {
            status_line.request("{}".to_string(), 80, 24);
        }
        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("no timeout")
            .expect("output");
        assert_eq!(first, "tick");

        let second = tokio::time::timeout(Duration::from_millis(800), rx.recv()).await;
        assert!(second.is_err(), "the burst must collapse into one run");

        status_line.shutdown();
    }

    #[test]
    fn a_non_command_type_starts_nothing() {
        let (tx, _rx) = mpsc::channel::<String>(4);
        let mut cfg = config("date");
        cfg.kind = "webhook".to_string();

        assert!(StatusLine::spawn(&cfg, tx).is_none());
    }

    #[tokio::test]
    async fn the_refresh_interval_never_drops_below_a_second() {
        let (tx, _rx) = mpsc::channel::<String>(4);
        let mut cfg = config("date");
        cfg.refresh_interval = Some(0);

        let status_line = StatusLine::spawn(&cfg, tx).expect("runner");
        assert_eq!(status_line.refresh_interval(), Some(Duration::from_secs(1)));
        status_line.shutdown();
    }
}
