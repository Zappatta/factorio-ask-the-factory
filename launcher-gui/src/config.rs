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
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_bridge")]
    pub bridge: BridgeCfg,
    #[serde(default = "default_server")]
    pub server: ServerCfg,
    pub rcon: RconCfg,
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
fn default_bridge() -> BridgeCfg {
    BridgeCfg {
        backend: default_backend(),
    }
}
fn default_server() -> ServerCfg {
    ServerCfg {
        port: default_game_port(),
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
