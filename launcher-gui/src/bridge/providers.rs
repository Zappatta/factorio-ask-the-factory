//! Streaming LLM backends. Each one feeds text to a sink as it arrives.

use crate::config::{AnthropicCfg, ClaudeCliCfg, Config, OllamaCfg, OpenAiCfg};
use serde::Serialize;
use serde_json::{json, Value};
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const HEADERS_TIMEOUT: Duration = Duration::from_secs(120);
const BODY_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderError(pub String);

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProviderError {}

fn err(msg: impl Into<String>) -> ProviderError {
    ProviderError(msg.into())
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Message {
            role: role.to_string(),
            content: content.into(),
        }
    }
}

/// Returning false from the sink aborts the stream, which is how shutdown gets out
/// of a half-finished answer.
pub type ChunkSink<'a> = &'a mut dyn FnMut(&str) -> bool;

/// What one line of a provider's stream means.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamStep {
    Text(String),
    Done,
    Skip,
}

pub fn stream(
    backend: &str,
    cfg: &Config,
    system: &str,
    messages: &[Message],
    sink: ChunkSink,
) -> Result<(), ProviderError> {
    match backend {
        "claude-cli" => claude_cli(&cfg.claude_cli, system, messages, sink),
        "anthropic-api" => anthropic_api(&cfg.anthropic_api, system, messages, sink),
        "ollama" => ollama(&cfg.ollama, system, messages, sink),
        "openai-compatible" => openai_compatible(&cfg.openai_compatible, system, messages, sink),
        other => Err(err(format!("unknown backend '{other}'"))),
    }
}

/// Collapses a message list into one prompt, for backends that take a single string.
fn flatten(messages: &[Message]) -> String {
    let Some((last, prior)) = messages.split_last() else {
        return String::new();
    };
    let mut parts: Vec<String> = prior
        .iter()
        .map(|m| format!("[{}]\n{}", m.role, m.content))
        .collect();
    parts.push(last.content.clone());
    parts.join("\n\n")
}

fn with_system(system: &str, messages: &[Message]) -> Vec<Message> {
    let mut all = vec![Message::new("system", system)];
    all.extend_from_slice(messages);
    all
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

// ---- claude CLI ---------------------------------------------------------

fn claude_cli(
    cfg: &ClaudeCliCfg,
    system: &str,
    messages: &[Message],
    sink: ChunkSink,
) -> Result<(), ProviderError> {
    let mut child = Command::new("claude")
        .args(["-p", "--output-format", "stream-json", "--include-partial-messages", "--verbose"])
        .arg("--model")
        .arg(&cfg.model)
        .arg("--system-prompt")
        .arg(system)
        .args([
            "--exclude-dynamic-system-prompt-sections",
            "--setting-sources",
            "",
            "--strict-mcp-config",
            "--mcp-config",
            r#"{"mcpServers":{}}"#,
            "--allowed-tools",
            "",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| err(format!("cannot run the claude CLI: {e}")))?;

    // The prompt carries a whole snapshot, which overflows the pipe buffer long before
    // the CLI starts draining it, so it has to go out on its own thread.
    let prompt = flatten(messages);
    let mut stdin = child.stdin.take().expect("piped");
    let feeder = thread::spawn(move || {
        let _ = stdin.write_all(prompt.as_bytes());
    });

    let stderr = child.stderr.take().expect("piped");
    let diagnostics = Arc::new(Mutex::new(String::new()));
    let collector = {
        let diagnostics = diagnostics.clone();
        thread::spawn(move || {
            let mut buf = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut buf);
            if let Ok(mut slot) = diagnostics.lock() {
                *slot = buf;
            }
        })
    };

    let stdout = child.stdout.take().expect("piped");
    let mut saw_text = false;
    let mut aborted = false;
    let mut failure = None;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        match parse_claude_line(line.trim()) {
            Ok(StreamStep::Text(text)) => {
                saw_text = true;
                if !sink(&text) {
                    aborted = true;
                    break;
                }
            }
            Ok(StreamStep::Done) => break,
            Ok(StreamStep::Skip) => {}
            Err(e) => {
                failure = Some(e);
                break;
            }
        }
    }

    let _ = feeder.join();
    if aborted || failure.is_some() {
        let _ = child.kill();
    }
    let status = wait_for(&mut child, Duration::from_secs(cfg.timeout));
    let _ = collector.join();

    if let Some(e) = failure {
        return Err(e);
    }
    let status = status?;
    if aborted {
        return Ok(());
    }
    if !status.success() && !saw_text {
        let detail = diagnostics
            .lock()
            .map(|d| clip(d.trim(), 400))
            .unwrap_or_default();
        return Err(err(format!(
            "claude CLI exited {}: {detail}",
            status.code().unwrap_or(-1)
        )));
    }
    Ok(())
}

fn wait_for(child: &mut Child, timeout: Duration) -> Result<std::process::ExitStatus, ProviderError> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(e) => return Err(err(format!("cannot wait on the claude CLI: {e}"))),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(err(format!(
                "claude CLI did not exit within {}s",
                timeout.as_secs()
            )));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

pub fn parse_claude_line(line: &str) -> Result<StreamStep, ProviderError> {
    if line.is_empty() {
        return Ok(StreamStep::Skip);
    }
    let Ok(evt) = serde_json::from_str::<Value>(line) else {
        return Ok(StreamStep::Skip);
    };
    match evt["type"].as_str() {
        Some("stream_event") => {
            let inner = &evt["event"];
            if inner["type"] == "content_block_delta" && inner["delta"]["type"] == "text_delta" {
                let text = inner["delta"]["text"].as_str().unwrap_or("");
                return Ok(StreamStep::Text(text.to_string()));
            }
            Ok(StreamStep::Skip)
        }
        Some("result") if evt["is_error"].as_bool().unwrap_or(false) => Err(err(evt["result"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("claude CLI reported an error")
            .to_string())),
        _ => Ok(StreamStep::Skip),
    }
}

// ---- HTTP backends ------------------------------------------------------

/// Posts JSON and hands back the still-open response body.
fn post_stream(
    url: &str,
    headers: &[(&str, &str)],
    body: &Value,
    what: &str,
) -> Result<Box<dyn Read>, ProviderError> {
    let payload = body.to_string();
    let mut request = ureq::post(url)
        .config()
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(HEADERS_TIMEOUT))
        .timeout_recv_body(Some(BODY_TIMEOUT))
        .build()
        .header("Content-Type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request
        .send(payload.as_str())
        .map_err(|e| err(format!("{what} unreachable: {e}")))?;
    let status = response.status();
    let mut received = response.into_body();
    if !status.is_success() {
        let detail = received.read_to_string().unwrap_or_default();
        return Err(err(format!(
            "{what} returned HTTP {}: {}",
            status.as_u16(),
            clip(detail.trim(), 400)
        )));
    }
    Ok(Box::new(received.into_reader()))
}

fn pump_lines(
    reader: Box<dyn Read>,
    what: &str,
    sink: ChunkSink,
    parse: fn(&str) -> Result<StreamStep, ProviderError>,
) -> Result<(), ProviderError> {
    for line in BufReader::new(reader).lines() {
        let line = line.map_err(|e| err(format!("{what} stream broke: {e}")))?;
        match parse(line.trim())? {
            StreamStep::Text(text) => {
                if !sink(&text) {
                    return Ok(());
                }
            }
            StreamStep::Done => return Ok(()),
            StreamStep::Skip => {}
        }
    }
    Ok(())
}

fn ollama(
    cfg: &OllamaCfg,
    system: &str,
    messages: &[Message],
    sink: ChunkSink,
) -> Result<(), ProviderError> {
    let host = cfg.host.trim_end_matches('/');
    let body = json!({
        "model": cfg.model,
        "messages": with_system(system, messages),
        "stream": true,
        "options": {"num_ctx": cfg.num_ctx},
    });
    let what = format!("ollama at {host}");
    let reader = post_stream(&format!("{host}/api/chat"), &[], &body, &what)?;
    pump_lines(reader, &what, sink, parse_ollama_line)
}

pub fn parse_ollama_line(line: &str) -> Result<StreamStep, ProviderError> {
    if line.is_empty() {
        return Ok(StreamStep::Skip);
    }
    let evt: Value = serde_json::from_str(line)
        .map_err(|e| err(format!("ollama sent a line that is not JSON: {e}")))?;
    if let Some(message) = evt["error"].as_str() {
        return Err(err(message.to_string()));
    }
    let chunk = evt["message"]["content"].as_str().unwrap_or("");
    if !chunk.is_empty() {
        return Ok(StreamStep::Text(chunk.to_string()));
    }
    if evt["done"].as_bool().unwrap_or(false) {
        return Ok(StreamStep::Done);
    }
    Ok(StreamStep::Skip)
}

fn openai_compatible(
    cfg: &OpenAiCfg,
    system: &str,
    messages: &[Message],
    sink: ChunkSink,
) -> Result<(), ProviderError> {
    let base = cfg.base_url.trim_end_matches('/');
    let body = json!({
        "model": cfg.model,
        "messages": with_system(system, messages),
        "stream": true,
    });
    let bearer = format!("Bearer {}", cfg.api_key);
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if !cfg.api_key.is_empty() {
        headers.push(("Authorization", &bearer));
    }
    let what = format!("endpoint at {base}");
    let reader = post_stream(
        &format!("{base}/chat/completions"),
        &headers,
        &body,
        &what,
    )?;
    pump_lines(reader, &what, sink, parse_openai_line)
}

pub fn parse_openai_line(line: &str) -> Result<StreamStep, ProviderError> {
    let Some(payload) = line.strip_prefix("data:") else {
        return Ok(StreamStep::Skip);
    };
    let payload = payload.trim();
    if payload == "[DONE]" {
        return Ok(StreamStep::Done);
    }
    if payload.is_empty() {
        return Ok(StreamStep::Skip);
    }
    let evt: Value = serde_json::from_str(payload)
        .map_err(|e| err(format!("endpoint sent a data line that is not JSON: {e}")))?;
    if let Some(message) = evt["error"]["message"].as_str() {
        return Err(err(message.to_string()));
    }
    let chunk = evt["choices"][0]["delta"]["content"].as_str().unwrap_or("");
    if chunk.is_empty() {
        return Ok(StreamStep::Skip);
    }
    Ok(StreamStep::Text(chunk.to_string()))
}

fn anthropic_api(
    cfg: &AnthropicCfg,
    system: &str,
    messages: &[Message],
    sink: ChunkSink,
) -> Result<(), ProviderError> {
    let key = if cfg.api_key.is_empty() {
        std::env::var("ANTHROPIC_API_KEY").unwrap_or_default()
    } else {
        cfg.api_key.clone()
    };
    if key.is_empty() {
        return Err(err(
            "no API key: set ANTHROPIC_API_KEY or config [anthropic-api].api_key",
        ));
    }
    let body = json!({
        "model": cfg.model,
        "max_tokens": cfg.max_tokens,
        "system": system,
        "messages": messages,
        "stream": true,
    });
    let headers = [
        ("x-api-key", key.as_str()),
        ("anthropic-version", ANTHROPIC_VERSION),
    ];
    let reader = post_stream(ANTHROPIC_URL, &headers, &body, "anthropic API")?;
    pump_lines(reader, "anthropic API", sink, parse_anthropic_line)
}

pub fn parse_anthropic_line(line: &str) -> Result<StreamStep, ProviderError> {
    let Some(payload) = line.strip_prefix("data:") else {
        return Ok(StreamStep::Skip);
    };
    let payload = payload.trim();
    if payload.is_empty() {
        return Ok(StreamStep::Skip);
    }
    let evt: Value = serde_json::from_str(payload)
        .map_err(|e| err(format!("anthropic API sent a data line that is not JSON: {e}")))?;
    match evt["type"].as_str() {
        Some("content_block_delta") if evt["delta"]["type"] == "text_delta" => Ok(
            StreamStep::Text(evt["delta"]["text"].as_str().unwrap_or("").to_string()),
        ),
        Some("error") => Err(err(evt["error"]["message"]
            .as_str()
            .unwrap_or("anthropic API reported an error")
            .to_string())),
        Some("message_stop") => Ok(StreamStep::Done),
        _ => Ok(StreamStep::Skip),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_labels_all_but_the_last_turn() {
        let messages = [
            Message::new("user", "first"),
            Message::new("assistant", "reply"),
            Message::new("user", "current"),
        ];
        assert_eq!(
            flatten(&messages),
            "[user]\nfirst\n\n[assistant]\nreply\n\ncurrent"
        );
        assert_eq!(flatten(&[]), "");
    }

    #[test]
    fn claude_cli_ndjson_yields_only_text_deltas() {
        let text = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Green "}}}"#;
        assert_eq!(
            parse_claude_line(text).unwrap(),
            StreamStep::Text("Green ".into())
        );

        let thinking = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hmm"}}}"#;
        assert_eq!(parse_claude_line(thinking).unwrap(), StreamStep::Skip);

        assert_eq!(parse_claude_line("not json at all").unwrap(), StreamStep::Skip);
        assert_eq!(parse_claude_line("").unwrap(), StreamStep::Skip);
        assert_eq!(
            parse_claude_line(r#"{"type":"result","is_error":false,"result":"done"}"#).unwrap(),
            StreamStep::Skip
        );
    }

    #[test]
    fn claude_cli_result_errors_are_surfaced() {
        let line = r#"{"type":"result","is_error":true,"result":"credit balance too low"}"#;
        assert_eq!(
            parse_claude_line(line).unwrap_err(),
            ProviderError("credit balance too low".into())
        );
        let bare = r#"{"type":"result","is_error":true}"#;
        assert!(parse_claude_line(bare).unwrap_err().0.contains("reported an error"));
    }

    #[test]
    fn ollama_ndjson_streams_then_finishes() {
        assert_eq!(
            parse_ollama_line(r#"{"message":{"role":"assistant","content":"412"},"done":false}"#)
                .unwrap(),
            StreamStep::Text("412".into())
        );
        assert_eq!(
            parse_ollama_line(r#"{"message":{"content":""},"done":true}"#).unwrap(),
            StreamStep::Done
        );
        assert_eq!(parse_ollama_line("").unwrap(), StreamStep::Skip);
        assert_eq!(
            parse_ollama_line(r#"{"error":"model 'nope' not found"}"#).unwrap_err(),
            ProviderError("model 'nope' not found".into())
        );
        assert!(parse_ollama_line("<html>502</html>").is_err());
    }

    #[test]
    fn openai_sse_reads_delta_content() {
        assert_eq!(
            parse_openai_line(
                r#"data: {"choices":[{"delta":{"content":"hello"},"index":0}]}"#
            )
            .unwrap(),
            StreamStep::Text("hello".into())
        );
        assert_eq!(parse_openai_line("data: [DONE]").unwrap(), StreamStep::Done);
        assert_eq!(parse_openai_line(": keep-alive").unwrap(), StreamStep::Skip);
        assert_eq!(parse_openai_line("").unwrap(), StreamStep::Skip);
        // The role-only opening frame carries no content.
        assert_eq!(
            parse_openai_line(r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#).unwrap(),
            StreamStep::Skip
        );
        assert_eq!(
            parse_openai_line(r#"data: {"error":{"message":"context length exceeded"}}"#)
                .unwrap_err(),
            ProviderError("context length exceeded".into())
        );
    }

    #[test]
    fn anthropic_sse_reads_text_deltas() {
        assert_eq!(
            parse_anthropic_line(
                r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Coal"}}"#
            )
            .unwrap(),
            StreamStep::Text("Coal".into())
        );
        assert_eq!(
            parse_anthropic_line(r#"data: {"type":"message_stop"}"#).unwrap(),
            StreamStep::Done
        );
        assert_eq!(
            parse_anthropic_line("event: content_block_delta").unwrap(),
            StreamStep::Skip
        );
        assert_eq!(
            parse_anthropic_line(r#"data: {"type":"ping"}"#).unwrap(),
            StreamStep::Skip
        );
        assert_eq!(
            parse_anthropic_line(
                r#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
            )
            .unwrap_err(),
            ProviderError("Overloaded".into())
        );
    }
}
