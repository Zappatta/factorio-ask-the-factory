use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct RconCfg {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_rcon_port")]
    pub port: u16,
    #[serde(default)]
    pub password: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerCfg {
    #[serde(default = "default_game_port")]
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BridgeCfg {
    #[serde(default = "default_backend")]
    pub backend: String,
    #[serde(default = "default_script_output_dir")]
    pub script_output_dir: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval: f64,
    #[serde(default = "default_history_turns")]
    pub history_turns: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SnapshotCfg {
    #[serde(default = "default_tier")]
    pub tier: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeCliCfg {
    #[serde(default = "default_claude_model")]
    pub model: String,
    #[serde(default = "default_claude_timeout")]
    pub timeout: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicCfg {
    #[serde(default = "default_claude_model")]
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
    #[serde(default)]
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OllamaCfg {
    #[serde(default = "default_ollama_host")]
    pub host: String,
    #[serde(default = "default_ollama_model")]
    pub model: String,
    #[serde(default = "default_num_ctx")]
    pub num_ctx: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiCfg {
    #[serde(default = "default_openai_base")]
    pub base_url: String,
    #[serde(default = "default_openai_model")]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub bridge: BridgeCfg,
    #[serde(default)]
    pub snapshot: SnapshotCfg,
    #[serde(default)]
    pub server: ServerCfg,
    pub rcon: RconCfg,
    #[serde(rename = "claude-cli", default)]
    pub claude_cli: ClaudeCliCfg,
    #[serde(rename = "anthropic-api", default)]
    pub anthropic_api: AnthropicCfg,
    #[serde(default)]
    pub ollama: OllamaCfg,
    #[serde(rename = "openai-compatible", default)]
    pub openai_compatible: OpenAiCfg,
}

fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_rcon_port() -> u16 {
    27015
}
fn default_game_port() -> u16 {
    34197
}
fn default_backend() -> String {
    "claude-cli".into()
}
fn default_script_output_dir() -> String {
    "./serverdata/script-output".into()
}
fn default_poll_interval() -> f64 {
    0.2
}
fn default_history_turns() -> usize {
    6
}
fn default_tier() -> String {
    "auto".into()
}
fn default_claude_model() -> String {
    "claude-sonnet-5".into()
}
fn default_claude_timeout() -> u64 {
    180
}
fn default_max_tokens() -> u64 {
    2000
}
fn default_ollama_host() -> String {
    "http://localhost:11434".into()
}
fn default_ollama_model() -> String {
    "llama3.1:8b".into()
}
fn default_num_ctx() -> u64 {
    16384
}
fn default_openai_base() -> String {
    "http://localhost:1234/v1".into()
}
fn default_openai_model() -> String {
    "local-model".into()
}

impl Default for BridgeCfg {
    fn default() -> Self {
        BridgeCfg {
            backend: default_backend(),
            script_output_dir: default_script_output_dir(),
            poll_interval: default_poll_interval(),
            history_turns: default_history_turns(),
        }
    }
}
impl Default for SnapshotCfg {
    fn default() -> Self {
        SnapshotCfg {
            tier: default_tier(),
        }
    }
}
impl Default for ServerCfg {
    fn default() -> Self {
        ServerCfg {
            port: default_game_port(),
        }
    }
}
impl Default for ClaudeCliCfg {
    fn default() -> Self {
        ClaudeCliCfg {
            model: default_claude_model(),
            timeout: default_claude_timeout(),
        }
    }
}
impl Default for AnthropicCfg {
    fn default() -> Self {
        AnthropicCfg {
            model: default_claude_model(),
            max_tokens: default_max_tokens(),
            api_key: String::new(),
        }
    }
}
impl Default for OllamaCfg {
    fn default() -> Self {
        OllamaCfg {
            host: default_ollama_host(),
            model: default_ollama_model(),
            num_ctx: default_num_ctx(),
        }
    }
}
impl Default for OpenAiCfg {
    fn default() -> Self {
        OpenAiCfg {
            base_url: default_openai_base(),
            model: default_openai_model(),
            api_key: String::new(),
        }
    }
}

pub const BACKENDS: [&str; 4] = [
    "claude-cli",
    "anthropic-api",
    "ollama",
    "openai-compatible",
];

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        toml::from_str(&raw).map_err(|e| format!("cannot parse {}: {e}", path.display()))
    }

    /// Rewrites just the backend line, preserving comments and everything else.
    pub fn save_backend(path: &Path, backend: &str) -> Result<(), String> {
        let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let mut in_bridge = false;
        let mut done = false;
        for line in raw.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                in_bridge = t == "[bridge]";
            }
            if in_bridge && !done && t.starts_with("backend") && t.contains('=') {
                out.push(format!("backend = \"{backend}\""));
                done = true;
                continue;
            }
            out.push(line.to_string());
        }
        std::fs::write(path, out.join("\n") + "\n").map_err(|e| e.to_string())
    }
}
