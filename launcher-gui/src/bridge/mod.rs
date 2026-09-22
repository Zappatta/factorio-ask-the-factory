//! The bridge daemon: tails the mod's bus file, streams an answer out of the
//! configured provider and pushes it back into the game over RCON.

pub mod markers;
pub mod providers;

use crate::config::{Config, BACKENDS};
use crate::rcon::Rcon;
use markers::Marker;
use providers::{Message, ProviderError};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const BUS_FILE: &str = "llm_scout_bus.jsonl";
const HEARTBEAT: Duration = Duration::from_secs(10);
const FLUSH_INTERVAL: Duration = Duration::from_millis(200);
const FLUSH_CHARS: usize = 60;
const MAX_RCON_BODY: usize = 3500;
const RCON_TEXT_BUDGET: usize = 1200;
const OFFER_PART_BUDGET: usize = 900;
const COMPACT_BACKENDS: [&str; 2] = ["ollama", "openai-compatible"];
const EMBEDDED_PROMPT: &str = include_str!("../../assets/prompt.txt");

/// Where log lines go. The GUI feeds these straight into its log pane.
pub type LogSink = Box<dyn Fn(&str) + Send>;

pub struct BridgeOpts {
    pub root: PathBuf,
    pub cfg: Config,
    pub backend: String,
}

/// Escapes a string for a Lua single-quoted literal. Backslash first, or the
/// escapes we add below get escaped in turn.
fn lua_quote(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
        .replace('\r', "")
}

/// The mod's JSON decoder only sees the bytes that survive the RCON hop, so every
/// non-ASCII character travels as a \u escape (Python's ensure_ascii).
fn escape_non_ascii(text: &str) -> String {
    if text.is_ascii() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            let mut units = [0u16; 2];
            for unit in ch.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

fn compact_json(value: &Value) -> String {
    escape_non_ascii(&value.to_string())
}

/// Chunks by characters so a piece never splits a multi-byte character.
fn split_for_rcon(text: &str, budget: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut piece = String::new();
    let mut count = 0;
    for ch in text.chars() {
        piece.push(ch);
        count += 1;
        if count == budget {
            parts.push(std::mem::take(&mut piece));
            count = 0;
        }
    }
    if !piece.is_empty() {
        parts.push(piece);
    }
    parts
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The system prompt ships inside the binary, but a prompt.txt beside the executable
/// or at the project root wins so it can be tuned without a rebuild.
pub fn system_prompt(root: &Path) -> String {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("prompt.txt"));
        }
    }
    candidates.push(root.join("prompt.txt"));
    for path in candidates {
        if let Ok(text) = fs::read_to_string(&path) {
            if !text.trim().is_empty() {
                return text;
            }
        }
    }
    EMBEDDED_PROMPT.to_string()
}

#[derive(Debug, Deserialize)]
struct Ask {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    player_index: Option<i64>,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default = "empty_object")]
    snapshot: Value,
}

fn empty_object() -> Value {
    json!({})
}

impl Ask {
    fn question(&self) -> &str {
        self.question.as_deref().unwrap_or("")
    }
    fn id_label(&self) -> String {
        self.id.map_or_else(|| "?".to_string(), |v| v.to_string())
    }
}

/// Returns the ask on that line, or None for blanks and other bus record types.
fn parse_bus_line(line: &str) -> Result<Option<Ask>, String> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let ask: Ask = serde_json::from_str(line).map_err(|e| e.to_string())?;
    if ask.kind != "ask" {
        return Ok(None);
    }
    Ok(Some(ask))
}

/// An RCON connection that reconnects on demand, the way the game comes and goes.
struct RconLink {
    host: String,
    port: u16,
    password: String,
    link: Option<Rcon>,
}

impl RconLink {
    fn new(host: &str, port: u16, password: &str) -> Self {
        RconLink {
            host: host.to_string(),
            port,
            password: password.to_string(),
            link: None,
        }
    }

    fn connect(&mut self) -> Result<(), String> {
        self.link = None;
        let link = Rcon::connect(&self.host, self.port, &self.password, Duration::from_secs(10))
            .map_err(|e| e.to_string())?;
        self.link = Some(link);
        Ok(())
    }

    fn command(&mut self, body: &str) -> Result<String, String> {
        if self.link.is_none() {
            self.connect()?;
        }
        let link = self.link.as_mut().expect("connected above");
        match link.command(body) {
            Ok(out) => Ok(out),
            Err(e) => {
                self.link = None;
                Err(format!("RCON transport failure: {e}"))
            }
        }
    }
}

pub struct Bridge {
    root: PathBuf,
    cfg: Config,
    backend: String,
    system: String,
    bus_path: PathBuf,
    offset: u64,
    rcon: RconLink,
    history: HashMap<i64, Vec<Message>>,
    last_heartbeat: Option<Instant>,
    log: LogSink,
    stop: Arc<AtomicBool>,
}

impl Bridge {
    pub fn new(opts: BridgeOpts, log: LogSink, stop: Arc<AtomicBool>) -> Result<Self, String> {
        let configured = Path::new(&opts.cfg.bridge.script_output_dir).to_path_buf();
        let out_dir = if configured.is_absolute() {
            configured
        } else {
            opts.root.join(configured)
        };
        let bus_path = out_dir.join(BUS_FILE);
        if let Some(parent) = bus_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        if !bus_path.exists() {
            File::create(&bus_path).map_err(|e| format!("cannot create {}: {e}", bus_path.display()))?;
        }
        let offset = fs::metadata(&bus_path).map(|m| m.len()).unwrap_or(0);

        let rc = &opts.cfg.rcon;
        let rcon = RconLink::new(&rc.host, rc.port, &rc.password);
        let system = system_prompt(&opts.root);
        Ok(Bridge {
            system,
            backend: opts.backend,
            cfg: opts.cfg,
            root: opts.root,
            bus_path,
            offset,
            rcon,
            history: HashMap::new(),
            last_heartbeat: None,
            log,
            stop,
        })
    }

    fn info(&self, msg: &str) {
        (self.log)(msg);
    }
    fn warn(&self, msg: &str) {
        (self.log)(&format!("WARNING {msg}"));
    }
    fn error(&self, msg: &str) {
        (self.log)(&format!("ERROR {msg}"));
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    // ---- game I/O -------------------------------------------------------

    fn call_mod(&mut self, func: &str, payload: &Value) {
        let body = compact_json(payload);
        let cmd = format!(
            "/silent-command remote.call('llm_scout','{func}','{}')",
            lua_quote(&body)
        );
        if cmd.len() > MAX_RCON_BODY {
            self.error(&format!(
                "RCON command too long ({} bytes) for {func}, dropped",
                cmd.len()
            ));
            return;
        }
        match self.rcon.command(&cmd) {
            Ok(out) => {
                let text = out.trim();
                if text.is_empty() {
                    return;
                }
                // A Lua error inside the mod comes back here; do not bury it.
                let head: String = text.chars().take(40).collect();
                if text.contains("Error") || head.contains("error") {
                    self.error(&format!("mod error from {func}: {}", clip(text, 400)));
                }
            }
            Err(e) => self.warn(&format!("rcon {func} failed: {e}")),
        }
    }

    fn heartbeat(&mut self) {
        if let Some(last) = self.last_heartbeat {
            if last.elapsed() < HEARTBEAT {
                return;
            }
        }
        self.last_heartbeat = Some(Instant::now());
        let payload = json!({
            "backends": BACKENDS,
            "selected": self.backend,
            "tier": self.cfg.snapshot.tier,
        });
        self.call_mod("hello", &payload);
    }

    fn send_text(&mut self, id: Option<i64>, text: &str) {
        for piece in split_for_rcon(text, RCON_TEXT_BUDGET) {
            self.call_mod("deliver", &json!({"id": id, "text": piece, "final": false}));
        }
    }

    fn send_error(&mut self, id: Option<i64>, message: &str) {
        self.call_mod(
            "deliver",
            &json!({"id": id, "error": clip(message, 500), "final": true}),
        );
    }

    /// A build spec is far bigger than one RCON command, so it goes over in
    /// numbered parts the mod reassembles.
    fn send_place(&mut self, id: Option<i64>, player_index: i64, label: &str, entities: &[markers::Entity]) {
        let body = compact_json(&json!({"label": label, "entities": entities}));
        let parts = split_for_rcon(&body, OFFER_PART_BUDGET);
        let total = parts.len();
        for (index, part) in parts.iter().enumerate() {
            self.call_mod(
                "build_offer",
                &json!({
                    "id": id,
                    "player_index": player_index,
                    "seq": index + 1,
                    "total": total,
                    "part": part,
                }),
            );
        }
    }

    // ---- request handling -----------------------------------------------

    fn effective_tier(&self) -> String {
        let tier = &self.cfg.snapshot.tier;
        if tier != "auto" {
            return tier.clone();
        }
        if COMPACT_BACKENDS.contains(&self.backend.as_str()) {
            "compact".into()
        } else {
            "full".into()
        }
    }

    fn build_messages(&self, ask: &Ask) -> Vec<Message> {
        let snapshot = compact_json(&ask.snapshot);
        let user = format!(
            "Current factory snapshot:\n{snapshot}\n\nPlayer question: {}",
            ask.question()
        );
        let key = ask.player_index.unwrap_or(0);
        let window = self.cfg.bridge.history_turns * 2;
        let mut messages: Vec<Message> = self
            .history
            .get(&key)
            .map(|turns| turns[turns.len().saturating_sub(window)..].to_vec())
            .unwrap_or_default();
        messages.push(Message::new("user", user));
        messages
    }

    fn remember(&mut self, key: i64, question: &str, answer: &str) {
        let window = self.cfg.bridge.history_turns * 2;
        let turns = self.history.entry(key).or_default();
        turns.push(Message::new("user", question));
        turns.push(Message::new("assistant", answer));
        if turns.len() > window {
            turns.drain(..turns.len() - window);
        }
    }

    /// Sends the markers that act on the world straight away; pings are collected
    /// so they land after the text, which is what the mod's chat pane expects.
    fn dispatch(
        &mut self,
        id: Option<i64>,
        player_index: i64,
        found: Vec<Marker>,
        errors: Vec<String>,
        pings: &mut Vec<Value>,
    ) {
        if !errors.is_empty() {
            self.warn(&format!("build spec issues: {}", errors.join("; ")));
        }
        for marker in found {
            match marker {
                Marker::CloneLike { x, y, dx, dy, label } => {
                    self.info(&format!("clone_like: ({x},{y}) -> ({dx},{dy})  {label}"));
                    self.call_mod(
                        "clone_like",
                        &json!({"x": x, "y": y, "dx": dx, "dy": dy, "label": label,
                                "id": id, "player_index": player_index}),
                    );
                }
                Marker::Clone { x1, y1, x2, y2, dx, dy, label } => {
                    self.info(&format!(
                        "clone offer: {label}  ({x1},{y1})-({x2},{y2}) -> ({dx},{dy})"
                    ));
                    self.call_mod(
                        "clone_offer",
                        &json!({"label": label, "x1": x1, "y1": y1, "x2": x2, "y2": y2,
                                "dx": dx, "dy": dy, "id": id, "player_index": player_index}),
                    );
                }
                Marker::Place { label, entities } => {
                    self.info(&format!(
                        "build offer: {label} ({} entities)",
                        entities.len()
                    ));
                    self.send_place(id, player_index, &label, &entities);
                }
                Marker::Ping { x, y, label } => {
                    self.info(&format!("ping {x},{y} {label}"));
                    pings.push(json!({"x": x, "y": y, "text": label,
                                      "player_index": player_index, "id": id}));
                }
            }
        }
    }

    fn handle(&mut self, ask: Ask) {
        let id = ask.id;
        let player_index = ask.player_index.unwrap_or(1);
        let question = ask.question().trim().to_string();
        let backend = match ask.backend.as_deref() {
            Some(name) if BACKENDS.contains(&name) => name.to_string(),
            _ => self.backend.clone(),
        };
        self.backend = backend.clone();

        let snapshot_kb = ask.snapshot.to_string().len() as f64 / 1024.0;
        self.info(&format!(
            "q#{} [{backend}] {:?} (snapshot {snapshot_kb:.1} KB)",
            ask.id_label(),
            clip(&question, 80)
        ));

        let messages = self.build_messages(&ask);
        let cfg = self.cfg.clone();
        let system = self.system.clone();
        let started = Instant::now();

        let mut buffer = String::new();
        let mut emitted = String::new();
        let mut raw = String::new();
        let mut pings: Vec<Value> = Vec::new();
        let mut pending = String::new();
        let mut last_flush = Instant::now();

        let outcome = {
            let mut sink = |chunk: &str| -> bool {
                if self.stopping() {
                    return false;
                }
                raw.push_str(chunk);
                buffer.push_str(chunk);
                let (ready, found, held, errors) = markers::drain(&buffer, false);
                buffer = held;
                self.dispatch(id, player_index, found, errors, &mut pings);
                if !ready.is_empty() {
                    pending.push_str(&ready);
                    emitted.push_str(&ready);
                }
                if !pending.is_empty()
                    && (pending.chars().count() >= FLUSH_CHARS
                        || last_flush.elapsed() >= FLUSH_INTERVAL)
                {
                    self.send_text(id, &pending);
                    pending.clear();
                    last_flush = Instant::now();
                }
                true
            };
            providers::stream(&backend, &cfg, &system, &messages, &mut sink)
        };

        if let Err(ProviderError(reason)) = outcome {
            self.error(&format!("provider failed: {reason}"));
            self.send_error(id, &format!("{backend}: {reason}"));
            return;
        }
        if self.stopping() {
            self.warn(&format!("q#{} abandoned, bridge is stopping", ask.id_label()));
            return;
        }

        let (ready, found, _, errors) = markers::drain(&buffer, true);
        self.dispatch(id, player_index, found, errors, &mut pings);
        pending.push_str(&ready);
        emitted.push_str(&ready);
        if !pending.is_empty() {
            self.send_text(id, &pending);
        }
        self.call_mod("deliver", &json!({"id": id, "text": "", "final": true}));
        for ping in &pings {
            self.call_mod("ping", ping);
        }

        self.write_answer_log(&ask.id_label(), &backend, &question, &raw);
        if raw.contains("[[") && emitted.contains("[[") {
            self.warn(
                "a [[...]] marker survived into the displayed text - see bridge/logs/last_answer.txt",
            );
        }

        let answer = emitted.trim().to_string();
        self.remember(ask.player_index.unwrap_or(0), &question, &answer);
        self.info(&format!(
            "q#{} answered in {:.1}s ({} chars)",
            ask.id_label(),
            started.elapsed().as_secs_f64(),
            answer.chars().count()
        ));
    }

    fn write_answer_log(&self, id: &str, backend: &str, question: &str, raw: &str) {
        let dir = self.root.join("bridge").join("logs");
        let written = fs::create_dir_all(&dir)
            .and_then(|_| fs::write(dir.join("last_answer.txt"), raw))
            .and_then(|_| {
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("answers.log"))?;
                let rule = "=".repeat(70);
                let dashes = "-".repeat(70);
                write!(file, "\n{rule}\nq#{id} [{backend}] {question}\n{dashes}\n{raw}\n")
            });
        if let Err(e) = written {
            self.warn(&format!("could not write answer log: {e}"));
        }
    }

    // ---- main loop ------------------------------------------------------

    /// Reads whatever the mod has appended since last time and returns the
    /// complete lines that are asks.
    fn pump_bus(&mut self, carry: &mut String) -> Vec<Ask> {
        let size = match fs::metadata(&self.bus_path) {
            Ok(meta) => meta.len(),
            Err(_) => return Vec::new(),
        };
        if size < self.offset {
            self.info("bus file truncated, rewinding");
            self.offset = 0;
            carry.clear();
        }
        if size <= self.offset {
            return Vec::new();
        }
        let mut file = match File::open(&self.bus_path) {
            Ok(f) => f,
            Err(e) => {
                self.warn(&format!("cannot open the bus file: {e}"));
                return Vec::new();
            }
        };
        let mut fresh = Vec::new();
        let read = file
            .seek(SeekFrom::Start(self.offset))
            .and_then(|_| file.read_to_end(&mut fresh));
        match read {
            Ok(count) => self.offset += count as u64,
            Err(e) => {
                self.warn(&format!("cannot read the bus file: {e}"));
                return Vec::new();
            }
        }
        carry.push_str(&String::from_utf8_lossy(&fresh));

        let mut asks = Vec::new();
        while let Some(end) = carry.find('\n') {
            let line: String = carry.drain(..=end).collect();
            match parse_bus_line(&line) {
                Ok(Some(ask)) => asks.push(ask),
                Ok(None) => {}
                Err(e) => self.warn(&format!("skipping malformed bus line: {e}")),
            }
        }
        asks
    }

    pub fn run(&mut self) {
        self.info(&format!(
            "watching {} (from byte {})",
            self.bus_path.display(),
            self.offset
        ));
        self.info(&format!(
            "backend={} tier={}",
            self.backend,
            self.effective_tier()
        ));
        match self.rcon.connect() {
            Ok(()) => self.info("rcon connected"),
            Err(e) => self.warn(&format!("rcon not reachable yet ({e}) - will retry")),
        }

        let poll = Duration::from_secs_f64(self.cfg.bridge.poll_interval.clamp(0.01, 5.0));
        let mut carry = String::new();
        while !self.stopping() {
            self.heartbeat();
            for ask in self.pump_bus(&mut carry) {
                if self.stopping() {
                    break;
                }
                self.handle(ask);
            }
            thread::sleep(poll);
        }
        self.info("bridge stopped");
    }
}

/// One question from the terminal against the live game. The dev tool that was ask.py.
pub fn ask_once(
    root: &Path,
    cfg: &Config,
    question: &str,
    backend: Option<&str>,
    tier: &str,
    show_snapshot: bool,
) -> Result<(), String> {
    let backend = backend.unwrap_or(&cfg.bridge.backend).to_string();
    let rc = &cfg.rcon;
    let mut client = Rcon::connect(&rc.host, rc.port, &rc.password, Duration::from_secs(30))
        .map_err(|e| format!("cannot reach RCON at {}:{}: {e}", rc.host, rc.port))?;

    let request = compact_json(&json!({"player_index": 1, "tier": tier, "dump": true}));
    let raw = client
        .command(&format!(
            "/silent-command remote.call('llm_scout','probe','{}')",
            lua_quote(&request)
        ))
        .map_err(|e| format!("probe failed: {e}"))?;
    let snapshot = raw
        .lines()
        .find(|line| line.starts_with('{'))
        .ok_or_else(|| format!("could not get a snapshot. probe said:\n{raw}"))?;

    eprintln!(
        "[snapshot {:.1} KB, backend {backend}]\n",
        snapshot.len() as f64 / 1024.0
    );
    if show_snapshot {
        match serde_json::from_str::<Value>(snapshot) {
            Ok(value) => eprintln!(
                "{}",
                clip(&serde_json::to_string_pretty(&value).unwrap_or_default(), 4000)
            ),
            Err(e) => eprintln!("snapshot is not valid JSON: {e}"),
        }
    }

    let messages = [Message::new(
        "user",
        format!("Current factory snapshot:\n{snapshot}\n\nPlayer question: {question}"),
    )];
    let system = system_prompt(root);
    let mut sink = |chunk: &str| -> bool {
        print!("{chunk}");
        let _ = std::io::stdout().flush();
        true
    };
    providers::stream(&backend, cfg, &system, &messages, &mut sink).map_err(|e| e.0)?;
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_quote_escapes_backslashes_before_anything_else() {
        assert_eq!(lua_quote(r"C:\path"), r"C:\\path");
        assert_eq!(lua_quote("it's"), r"it\'s");
        assert_eq!(lua_quote("a\nb"), r"a\nb");
        assert_eq!(lua_quote("a\r\nb"), r"a\nb");
        // A literal backslash-n in the payload must not become a newline escape.
        assert_eq!(lua_quote(r"a\nb"), r"a\\nb");
        assert_eq!(lua_quote(r#"{"t":"o'clock\n"}"#), r#"{"t":"o\'clock\\n"}"#);
    }

    #[test]
    fn non_ascii_travels_as_escapes() {
        assert_eq!(escape_non_ascii("plain"), "plain");
        assert_eq!(escape_non_ascii("caf\u{e9}"), r"caf\u00e9");
        // Astral characters need a surrogate pair, as the Lua side expects.
        assert_eq!(escape_non_ascii("\u{1f600}"), r"\ud83d\ude00");
        assert_eq!(
            compact_json(&json!({"text": "\u{e9}"})),
            r#"{"text":"\u00e9"}"#
        );
    }

    #[test]
    fn chunking_respects_the_budget_at_every_boundary() {
        assert_eq!(split_for_rcon("", 4), Vec::<String>::new());
        assert_eq!(split_for_rcon("abc", 4), vec!["abc"]);
        // Exactly one budget: one piece, no trailing empty.
        assert_eq!(split_for_rcon("abcd", 4), vec!["abcd"]);
        assert_eq!(split_for_rcon("abcde", 4), vec!["abcd", "e"]);
        assert_eq!(split_for_rcon("abcdefgh", 4), vec!["abcd", "efgh"]);
        // Budget counts characters, so multi-byte text never splits mid-character.
        let wide = "\u{e9}".repeat(5);
        let parts = split_for_rcon(&wide, 2);
        assert_eq!(parts, vec!["\u{e9}\u{e9}", "\u{e9}\u{e9}", "\u{e9}"]);
    }

    #[test]
    fn place_parts_are_numbered_and_reassemble() {
        let body = "x".repeat(OFFER_PART_BUDGET * 2 + 7);
        let parts = split_for_rcon(&body, OFFER_PART_BUDGET);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), OFFER_PART_BUDGET);
        assert_eq!(parts[2].len(), 7);
        assert_eq!(parts.concat(), body);
    }

    #[test]
    fn bus_lines_are_filtered_to_asks() {
        let line = r#"{"type":"ask","id":4,"player_index":1,"question":"why?","backend":"ollama","snapshot":{"meta":{"tick":10}}}"#;
        let ask = parse_bus_line(line).unwrap().expect("an ask");
        assert_eq!(ask.id, Some(4));
        assert_eq!(ask.player_index, Some(1));
        assert_eq!(ask.question(), "why?");
        assert_eq!(ask.backend.as_deref(), Some("ollama"));
        assert_eq!(ask.snapshot["meta"]["tick"], 10);

        assert!(parse_bus_line("").unwrap().is_none());
        assert!(parse_bus_line("   \n").unwrap().is_none());
        assert!(parse_bus_line(r#"{"type":"hello"}"#).unwrap().is_none());
        assert!(parse_bus_line("{not json").is_err());

        // A bare ask must still produce a usable snapshot object, not null.
        let bare = parse_bus_line(r#"{"type":"ask"}"#).unwrap().expect("an ask");
        assert_eq!(bare.snapshot, json!({}));
        assert_eq!(bare.id_label(), "?");
    }

    #[test]
    fn the_embedded_prompt_is_present() {
        assert!(EMBEDDED_PROMPT.contains("LLM Scout"));
        assert!(EMBEDDED_PROMPT.len() > 1000);
    }
}
