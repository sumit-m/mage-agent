//! mage-agent — a Buzz ACP harness with no LLM behind it.
//!
//! buzz-acp delivers each `@mage` mention as an ACP `session/prompt`. Every new
//! event runs on its own thread; mage queues overlapping requests itself. A
//! prompt is answered once its jobs finish; a `session/cancel` (buzz-acp's steer
//! path) answers it at once and leaves the jobs running so their results still
//! get posted. While a prompt is open it sends a keepalive every few minutes,
//! since buzz-acp times out a silent turn and a job can wait a while in mage's
//! queue.

mod command;
mod job;
mod proc;
mod prompt;
mod repo;
mod strip;
mod terminal;

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

const SEEN_CAPACITY: usize = 5000;
/// Well inside buzz-acp's idle timeout (25 minutes by default).
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5 * 60);

struct Agent {
    config: Arc<job::Config>,
    stdout: Mutex<std::io::Stdout>,
    pending: Mutex<HashMap<String, Pending>>,
    seen: Mutex<Seen>,
}

struct Pending {
    id: Value,
    answered: Arc<AtomicBool>,
}

#[derive(Default)]
struct Seen {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl Seen {
    fn insert(&mut self, id: &str) -> bool {
        if !self.ids.insert(id.to_string()) {
            return false;
        }
        self.order.push_back(id.to_string());
        if self.order.len() > SEEN_CAPACITY {
            if let Some(old) = self.order.pop_front() {
                self.ids.remove(&old);
            }
        }
        true
    }
}

impl Agent {
    fn send(&self, message: &Value) {
        let mut out = self.stdout.lock().unwrap_or_else(|p| p.into_inner());
        let _ = writeln!(out, "{message}");
        let _ = out.flush();
    }

    fn respond(&self, id: &Value, result: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn respond_error(&self, id: &Value, code: i64, message: &str) {
        self.send(
            &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
        );
    }

    fn handle(self: &Arc<Self>, message: Value) {
        let method = message["method"].as_str().unwrap_or_default();
        let params = &message["params"];
        let Some(id) = message.get("id").cloned() else {
            if method == "session/cancel" {
                self.cancel(params["sessionId"].as_str().unwrap_or_default());
            }
            return;
        };
        if message.get("method").is_none() {
            return;
        }
        match method {
            "initialize" => self.respond(
                &id,
                json!({
                    "protocolVersion": 2,
                    "agentCapabilities": { "loadSession": false },
                    "agentInfo": { "name": "mage-agent", "version": env!("CARGO_PKG_VERSION") },
                    "authMethods": []
                }),
            ),
            "session/new" => self.respond(&id, session_new_result()),
            "authenticate"
            | "session/set_model"
            | "session/set_config_option"
            | "session/set_mode" => self.respond(&id, json!({})),
            "session/prompt" => self.prompt(id, params),
            _ => self.respond_error(&id, -32601, &format!("method not found: {method}")),
        }
    }

    fn prompt(self: &Arc<Self>, id: Value, params: &Value) {
        let session = params["sessionId"].as_str().unwrap_or_default().to_string();
        let text = params["prompt"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
            .unwrap_or_default();

        let turn = prompt::parse_turn(&text);
        let fresh: Vec<prompt::Event> = {
            let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
            turn.events
                .into_iter()
                .filter(|e| seen.insert(&e.id))
                .collect()
        };
        if fresh.is_empty() {
            self.respond(&id, json!({ "stopReason": "end_turn" }));
            return;
        }

        let answered = Arc::new(AtomicBool::new(false));
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                session.clone(),
                Pending {
                    id: id.clone(),
                    answered: answered.clone(),
                },
            );

        let jobs: Vec<_> = fresh
            .into_iter()
            .map(|event| {
                let config = self.config.clone();
                let reply_to = prompt::reply_anchor_for(&event, turn.reply_to.as_deref());
                std::thread::spawn(move || job::run(&config, &event, reply_to.as_deref()))
            })
            .collect();

        let (jobs_done, wait) = mpsc::channel::<()>();
        let agent = self.clone();
        let (keepalive_session, keepalive_answered) = (session.clone(), answered.clone());
        std::thread::spawn(move || {
            while let Err(RecvTimeoutError::Timeout) = wait.recv_timeout(KEEPALIVE_INTERVAL) {
                if keepalive_answered.load(Ordering::SeqCst) {
                    break;
                }
                agent.keepalive(&keepalive_session);
            }
        });

        let agent = self.clone();
        std::thread::spawn(move || {
            for job in jobs {
                let _ = job.join();
            }
            drop(jobs_done);
            agent.finish(&session, &answered, &id, "end_turn");
        });
    }

    fn keepalive(&self, session: &str) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": { "sessionId": session, "update": { "sessionUpdate": "keepalive" } }
        }));
    }

    fn cancel(&self, session: &str) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session)
            .map(|p| (p.id.clone(), p.answered.clone()));
        if let Some((id, answered)) = pending {
            self.finish(session, &answered, &id, "cancelled");
        }
    }

    fn finish(&self, session: &str, answered: &AtomicBool, id: &Value, reason: &str) {
        if answered.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        if pending.get(session).is_some_and(|p| &p.id == id) {
            pending.remove(session);
        }
        drop(pending);
        self.respond(id, json!({ "stopReason": reason }));
    }
}

/// Buzz Desktop's model picker warns when a harness lists no models. mage picks
/// its model per request (`--model`), so one placeholder entry stands in.
fn session_new_result() -> Value {
    json!({
        "sessionId": new_session_id(),
        "models": {
            "currentModelId": "mage-cli",
            "availableModels": [{
                "modelId": "mage-cli",
                "name": "Default",
                "description": "Choose the image model with --model"
            }]
        }
    })
}

fn new_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("mage-{nanos:x}-{}", std::process::id())
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn config() -> job::Config {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from));
    let mage = env_path("MAGE_CLI")
        .or_else(|| {
            exe_dir
                .as_ref()
                .map(|d| d.join("mage.exe"))
                .filter(|p| p.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("mage"));
    let data_root = env_path("MAGE_AGENT_HOME")
        .or_else(|| env_path("LOCALAPPDATA").map(|d| d.join("mage-agent")))
        .unwrap_or_else(|| std::env::temp_dir().join("mage-agent"));
    job::Config {
        mage,
        buzz: env_path("BUZZ_CLI").unwrap_or_else(|| PathBuf::from("buzz")),
        display_name: std::env::var("BUZZ_ACP_DISPLAY_NAME").ok(),
        work_root: std::env::temp_dir().join("mage-agent"),
        cache_root: data_root.join("repos"),
    }
}

fn main() {
    let config = config();
    eprintln!(
        "mage-agent {}: mage={} buzz={}",
        env!("CARGO_PKG_VERSION"),
        config.mage.display(),
        config.buzz.display()
    );
    let agent = Arc::new(Agent {
        config: Arc::new(config),
        stdout: Mutex::new(std::io::stdout()),
        pending: Mutex::new(HashMap::new()),
        seen: Mutex::new(Seen::default()),
    });

    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim_start_matches('\u{feff}');
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(message) => agent.handle(message),
            Err(e) => eprintln!("mage-agent: bad frame: {e}"),
        }
    }
}
