//! `--mcp`: a stdio MCP server, so an MCP client such as Claude Code can read and change
//! the running game directly instead of going through the in-game chat window. It only
//! talks RCON; the server itself is still started from the launcher.

use crate::bridge::{self, compact_json, lua_quote, split_for_rcon, MAX_RCON_BODY, OFFER_PART_BUDGET};
use crate::config::{Config, RconCfg};
use crate::rcon::Rcon;
use serde_json::{json, Map, Value};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::time::Duration;

const MCP_GUIDE: &str = include_str!("../assets/mcp_guide.txt");
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const RCON_TIMEOUT: Duration = Duration::from_secs(30);

const INSTRUCTIONS: &str = "Reads and changes a live Factorio 2.0 game. Call factory_guide once \
before your first analysis or change in a conversation - it explains the snapshot's fields, \
the verified ratios, and the inserter-direction and entity-centre rules that placing gets wrong \
without it.";

struct Link {
    cfg: RconCfg,
    rcon: Option<Rcon>,
    staged: u64,
}

impl Link {
    fn command(&mut self, body: &str) -> Result<String, String> {
        // One retry, because a server restarted from the launcher leaves a dead socket.
        for attempt in 0..2 {
            if self.rcon.is_none() {
                let rc = &self.cfg;
                let rcon = Rcon::connect(&rc.host, rc.port, &rc.password, RCON_TIMEOUT).map_err(|e| {
                    format!(
                        "cannot reach the Factorio server over RCON at {}:{} ({e}). \
                         Start a session from the Ask the Factory launcher first.",
                        rc.host, rc.port
                    )
                })?;
                self.rcon = Some(rcon);
            }
            match self.rcon.as_mut().expect("connected above").command(body) {
                Ok(out) => return Ok(out),
                Err(e) => {
                    self.rcon = None;
                    if attempt == 1 {
                        return Err(format!("RCON transport failure: {e}"));
                    }
                }
            }
        }
        unreachable!("the loop returns on its second attempt")
    }

    fn remote(&mut self, func: &str, payload: &Value) -> Result<String, String> {
        let cmd = format!(
            "/silent-command remote.call('llm_scout','{func}','{}')",
            lua_quote(&compact_json(payload))
        );
        self.command(&cmd)
    }

    /// Runs one op in the mod and returns its result as compact JSON.
    fn call(&mut self, op: &str, args: Value) -> Result<String, String> {
        let request = json!({"op": op, "args": args});
        let body = compact_json(&request);
        let direct = format!(
            "/silent-command remote.call('llm_scout','mcp','{}')",
            lua_quote(&body)
        );
        let raw = if direct.len() <= MAX_RCON_BODY {
            self.command(&direct)?
        } else {
            self.staged += 1;
            let key = format!("{}-{}", std::process::id(), self.staged);
            let parts = split_for_rcon(&body, OFFER_PART_BUDGET);
            let total = parts.len();
            for (index, part) in parts.iter().enumerate() {
                self.remote("mcp_part", &json!({"key": key, "seq": index + 1, "part": part}))?;
            }
            self.remote("mcp", &json!({"staged": key, "total": total}))?
        };
        parse_reply(&raw)
    }
}

fn parse_reply(raw: &str) -> Result<String, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("the mod gave no answer. The running server probably has the mod from before \
                    MCP support - Stop and Resume it from the launcher."
            .into());
    }
    let reply: Value = serde_json::from_str(text).map_err(|_| {
        let hint = if text.contains("mcp") {
            " The running server probably has the mod from before MCP support - Stop and Resume it from the launcher."
        } else {
            ""
        };
        format!("unexpected reply from the game: {}{hint}", clip(text, 600))
    })?;
    if reply["ok"].as_bool() == Some(true) {
        Ok(reply["result"].to_string())
    } else {
        Err(reply["error"].as_str().unwrap_or("the mod reported an error with no message").to_string())
    }
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The MCP guide, then the system prompt's field reference and ratios. The prompt's
/// answer-formatting and marker sections are for the in-game window and are left out.
fn guide(root: &Path) -> String {
    let prompt = bridge::system_prompt(root);
    let start = prompt.find("SNAPSHOT FIELDS");
    let end = prompt.find("HOW TO ANSWER");
    match (start, end) {
        (Some(s), Some(e)) if s < e => format!("{MCP_GUIDE}\n{}", prompt[s..e].trim_end()),
        _ => MCP_GUIDE.to_string(),
    }
}

fn point(description: &str) -> Value {
    json!({
        "x": {"type": "number", "description": format!("map x of {description}")},
        "y": {"type": "number", "description": format!("map y of {description}")},
    })
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": properties, "required": required})
}

fn merge(mut a: Value, b: Value) -> Value {
    if let (Some(a), Value::Object(b)) = (a.as_object_mut(), b) {
        a.extend(b);
    }
    a
}

fn copy_options() -> Value {
    json!({
        "dest_x": {"type": "number", "description": "map x of the CENTRE of where the copy lands"},
        "dest_y": {"type": "number", "description": "map y of the CENTRE of where the copy lands"},
        "label": {"type": "string", "description": "a few words naming what is copied"},
        "ghosts": {"type": "boolean", "description": "place blueprint ghosts for robots instead of real entities"},
        "preview": {"type": "boolean", "description": "report source, destination and what is in the way, without placing"},
    })
}

fn item_args() -> Value {
    json!({
        "name": {"type": "string", "description": "internal item name, e.g. iron-plate"},
        "count": {"type": "integer", "minimum": 1},
        "quality": {"type": "string", "description": "normal, uncommon, rare, epic or legendary; default normal"},
    })
}

fn tools() -> Value {
    let read_only = json!({"readOnlyHint": true, "openWorldHint": false});
    let changes = json!({"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false});
    json!([
        {
            "name": "factory_guide",
            "description": "How to use these tools, what every snapshot field means, verified \
                            production ratios, and the placing rules. Read once per conversation.",
            "inputSchema": object(json!({}), &[]),
            "annotations": read_only,
        },
        {
            "name": "get_snapshot",
            "description": "The whole surface in summary: production rates, power, research, alerts, \
                            every machine group with status and what it is short on, trains, plus a \
                            belt-and-inserter local_view around one point. 30-45 KB.",
            "inputSchema": object(merge(point("the local_view centre; defaults to the player"), json!({
                "tier": {"type": "string", "enum": ["full", "compact"], "description": "compact is ~19 KB with no local_view or locations"},
            })), &[]),
            "annotations": read_only,
        },
        {
            "name": "item_report",
            "description": "One item or fluid in depth: made and used per minute and hour, every \
                            machine group that makes or uses it with status, short_on, feeds and \
                            locations, and the stock in logistic networks.",
            "inputSchema": object(json!({
                "name": {"type": "string", "description": "internal name, e.g. electronic-circuit, petroleum-gas"},
            }), &["name"]),
            "annotations": read_only,
        },
        {
            "name": "inspect_area",
            "description": "Every machine, inserter, belt run and chest in a square around a point, \
                            with status, contents and what each inserter takes from and drops into.",
            "inputSchema": object(merge(point("the centre"), json!({
                "radius": {"type": "number", "description": "tiles either side of the centre, 4-64, default 28"},
            })), &["x", "y"]),
            "annotations": read_only,
        },
        {
            "name": "inspect_entity",
            "description": "Everything at one position: inventories, fluids, recipe and progress, \
                            status and what it is short on, an inserter's targets and held item, \
                            each belt lane's contents.",
            "inputSchema": object(point("the entity"), &["x", "y"]),
            "annotations": read_only,
        },
        {
            "name": "list_builds",
            "description": "Everything placed or copied that can still be undone, with build ids.",
            "inputSchema": object(json!({}), &[]),
            "annotations": read_only,
        },
        {
            "name": "ping",
            "description": "Drop a permanent pin on the player's map. Label it with the place, not its state.",
            "inputSchema": object(merge(point("the pin"), json!({
                "label": {"type": "string", "description": "a few words naming the place"},
            })), &["x", "y", "label"]),
            "annotations": changes,
        },
        {
            "name": "copy_block",
            "description": "Copy the block around one existing machine to a new spot. The mod works \
                            out the block by following its inserters. Run with preview=true first.",
            "inputSchema": object(merge(point("any machine in the block to copy"), copy_options()),
                                  &["x", "y", "dest_x", "dest_y"]),
            "annotations": changes,
        },
        {
            "name": "copy_area",
            "description": "Copy an explicit rectangle to a new spot. Run with preview=true first.",
            "inputSchema": object(merge(json!({
                "x1": {"type": "number"}, "y1": {"type": "number"},
                "x2": {"type": "number"}, "y2": {"type": "number"},
            }), copy_options()), &["x1", "y1", "x2", "y2", "dest_x", "dest_y"]),
            "annotations": changes,
        },
        {
            "name": "place_entities",
            "description": "Place individual entities. Only when there is nothing to copy. An \
                            inserter's direction is the side it TAKES FROM. Positions are entity centres.",
            "inputSchema": object(json!({
                "label": {"type": "string"},
                "ghosts": {"type": "boolean", "description": "place ghosts for robots instead of real entities"},
                "entities": {
                    "type": "array",
                    "items": object(json!({
                        "name": {"type": "string", "description": "internal prototype name"},
                        "x": {"type": "number"},
                        "y": {"type": "number"},
                        "direction": {"type": "string", "enum": ["north", "east", "south", "west"]},
                        "recipe": {"type": "string"},
                        "requests": {
                            "type": "array",
                            "description": "logistic requests for a requester or buffer chest",
                            "items": object(json!({
                                "name": {"type": "string"}, "count": {"type": "integer"},
                            }), &["name", "count"]),
                        },
                    }), &["name", "x", "y"]),
                },
            }), &["entities"]),
            "annotations": changes,
        },
        {
            "name": "give_items",
            "description": "Cheat items into the player's inventory. Reports how many fit and how many they now have.",
            "inputSchema": object(item_args(), &["name", "count"]),
            "annotations": changes,
        },
        {
            "name": "remove_items",
            "description": "Remove items from the player's inventory. Reports how many were removed and how many are left.",
            "inputSchema": object(item_args(), &["name", "count"]),
            "annotations": json!({"readOnlyHint": false, "destructiveHint": true, "openWorldHint": false}),
        },
        {
            "name": "undo",
            "description": "Remove exactly what one place or copy created, including ghosts robots have since built.",
            "inputSchema": object(json!({
                "build_id": {"type": "string"},
            }), &["build_id"]),
            "annotations": json!({"readOnlyHint": false, "destructiveHint": true, "openWorldHint": false}),
        },
    ])
}

fn mod_op(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "get_snapshot" => "snapshot",
        "item_report" => "item",
        "inspect_area" => "area",
        "inspect_entity" => "entity",
        "list_builds" => "builds",
        "ping" => "ping",
        "copy_block" => "copy_block",
        "copy_area" => "copy_area",
        "place_entities" => "place",
        "undo" => "undo",
        "give_items" => "give",
        "remove_items" => "take",
        _ => return None,
    })
}

fn tool_result(outcome: Result<String, String>) -> Value {
    let (text, is_error) = match outcome {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

struct Server {
    guide: String,
    link: Link,
}

impl Server {
    fn handle(&mut self, method: &str, params: &Value) -> Result<Value, (i64, String)> {
        match method {
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or_default();
                let version = PROTOCOL_VERSIONS
                    .iter()
                    .find(|v| **v == asked)
                    .unwrap_or(&PROTOCOL_VERSIONS[0]);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "ask-the-factory", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or_default();
                let args = match &params["arguments"] {
                    Value::Object(map) => Value::Object(map.clone()),
                    _ => Value::Object(Map::new()),
                };
                if name == "factory_guide" {
                    return Ok(tool_result(Ok(self.guide.clone())));
                }
                let op = mod_op(name).ok_or((-32602, format!("unknown tool {name}")))?;
                Ok(tool_result(self.link.call(op, args)))
            }
            _ => Err((-32601, format!("method not found: {method}"))),
        }
    }
}

fn respond(out: &mut impl Write, message: &Value) -> io::Result<()> {
    writeln!(out, "{message}")?;
    out.flush()
}

pub fn run(root: &Path) -> Result<(), String> {
    let cfg = Config::load(&root.join("config.toml"))?;
    let mut server = Server {
        guide: guide(root),
        link: Link { cfg: cfg.rcon, rcon: None, staged: 0 },
    };
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| format!("stdin: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let reply = json!({"jsonrpc": "2.0", "id": null,
                                   "error": {"code": -32700, "message": format!("parse error: {e}")}});
                respond(&mut stdout, &reply).map_err(|e| format!("stdout: {e}"))?;
                continue;
            }
        };
        // No id means a notification, which never gets a reply.
        let Some(id) = request.get("id").cloned() else { continue };
        let method = request["method"].as_str().unwrap_or_default();
        let reply = match server.handle(method, &request["params"]) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        };
        respond(&mut stdout, &reply).map_err(|e| format!("stdout: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guide_carries_the_field_reference_but_not_the_chat_formatting() {
        let text = guide(Path::new("/nonexistent"));
        assert!(text.starts_with("ASK THE FACTORY - TOOL GUIDE"));
        assert!(text.contains("SNAPSHOT FIELDS"));
        assert!(text.contains("RATIOS AND RATES"));
        assert!(!text.contains("HOW TO ANSWER"));
        assert!(!text.contains("[[build:"));
    }

    #[test]
    fn every_listed_tool_is_routed() {
        for tool in tools().as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            assert!(name == "factory_guide" || mod_op(name).is_some(), "{name} has no mod op");
        }
    }

    #[test]
    fn replies_unwrap_results_and_surface_errors() {
        assert_eq!(parse_reply(r#"{"ok":true,"result":{"a":1}}"#).unwrap(), r#"{"a":1}"#);
        assert_eq!(parse_reply(r#"{"ok":false,"error":"no such offer"}"#).unwrap_err(), "no such offer");
        assert!(parse_reply("").unwrap_err().contains("Stop and Resume"));
        assert!(parse_reply("Unknown function: mcp").unwrap_err().contains("Stop and Resume"));
    }

    #[test]
    fn notifications_are_not_answered_and_unknown_methods_are() {
        let mut server = Server {
            guide: String::new(),
            link: Link {
                cfg: RconCfg { host: "127.0.0.1".into(), port: 1, password: String::new() },
                rcon: None,
                staged: 0,
            },
        };
        let init = server.handle("initialize", &json!({"protocolVersion": "2025-03-26"})).unwrap();
        assert_eq!(init["protocolVersion"], "2025-03-26");
        assert_eq!(server.handle("bogus", &json!({})).unwrap_err().0, -32601);
        let listed = server.handle("tools/list", &json!({})).unwrap();
        assert!(listed["tools"].as_array().unwrap().len() >= 10);
    }

    #[test]
    fn large_requests_are_staged_in_parts_that_fit() {
        let entities: Vec<Value> = (0..200)
            .map(|i| json!({"name": "assembling-machine-3", "x": i * 3, "y": 0, "recipe": "electronic-circuit"}))
            .collect();
        let body = compact_json(&json!({"op": "place", "args": {"entities": entities}}));
        for part in split_for_rcon(&body, OFFER_PART_BUDGET) {
            let cmd = format!(
                "/silent-command remote.call('llm_scout','mcp_part','{}')",
                lua_quote(&compact_json(&json!({"key": "1-1", "seq": 99, "part": part})))
            );
            assert!(cmd.len() <= MAX_RCON_BODY, "{} bytes", cmd.len());
        }
    }
}
