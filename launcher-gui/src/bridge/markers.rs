//! Parses the markers the model embeds in its prose and strips them from what
//! the player sees.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub enum Marker {
    Ping { x: f64, y: f64, label: String },
    CloneLike { x: f64, y: f64, dx: f64, dy: f64, label: String },
    Clone { x1: f64, y1: f64, x2: f64, y2: f64, dx: f64, dy: f64, label: String },
    Place { label: String, entities: Vec<Entity> },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Entity {
    pub name: String,
    pub x: f64,
    pub y: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<Request>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Request {
    pub name: String,
    pub count: i64,
}

const DIRECTIONS: [&str; 8] = [
    "north", "northeast", "east", "southeast", "south", "southwest", "west", "northwest",
];
pub const MAX_PLACE_ENTITIES: usize = 300;

fn ping_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\[\[ping:\s*(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)\s*(?:\|([^\]]*))?\]\]")
            .expect("ping regex")
    })
}

fn clone_like_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\[\[clone_like:\s*(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)\s*(?:->|to)\s*(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)\s*(?:\|([^\]]*))?\]\]",
        )
        .expect("clone_like regex")
    })
}

fn clone_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\[\[clone:([^\n\]]*)\n\s*from:\s*(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)\s+(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)\s*\n\s*to:\s*(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)\s*\n?\s*\]\]",
        )
        .expect("clone regex")
    })
}

fn place_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?s)\[\[build:([^\n\]]*)\n(.*?)\]\]").expect("place regex")
    })
}

fn num(s: &str) -> f64 {
    s.parse().unwrap_or(0.0)
}

/// An explicitly empty label is as unhelpful as a missing one; both get the fallback.
fn label_or(raw: Option<&str>, fallback: &str) -> String {
    let trimmed = raw.unwrap_or("").trim();
    if trimmed.is_empty() { fallback.to_string() } else { trimmed.to_string() }
}

/// One entity per line: name, x, y, then optional direction / recipe / item:count.
pub fn parse_entities(body: &str) -> (Vec<Entity>, Vec<String>) {
    let mut entities = Vec::new();
    let mut errors = Vec::new();
    for (lineno, raw) in body.trim().lines().enumerate() {
        let line = raw.trim().trim_start_matches('-').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split(',').map(str::trim).collect();
        if parts.len() < 3 {
            errors.push(format!("line {}: need at least name, x, y", lineno + 1));
            continue;
        }
        let (x, y) = match (parts[1].parse::<f64>(), parts[2].parse::<f64>()) {
            (Ok(x), Ok(y)) => (x, y),
            _ => {
                errors.push(format!("line {}: x and y must be numbers", lineno + 1));
                continue;
            }
        };
        let mut entity = Entity {
            name: parts[0].to_string(),
            x,
            y,
            direction: None,
            recipe: None,
            requests: Vec::new(),
        };
        for extra in &parts[3..] {
            if extra.is_empty() {
                continue;
            }
            let lower = extra.to_lowercase();
            if DIRECTIONS.contains(&lower.as_str()) {
                entity.direction = Some(lower);
            } else if let Some((item, count)) = extra.split_once(':') {
                match count.trim().parse::<i64>() {
                    Ok(n) => entity.requests.push(Request {
                        name: item.trim().to_string(),
                        count: n,
                    }),
                    Err(_) => errors.push(format!("line {}: bad request {extra:?}", lineno + 1)),
                }
            } else {
                entity.recipe = Some(extra.to_string());
            }
        }
        entities.push(entity);
        if entities.len() >= MAX_PLACE_ENTITIES {
            errors.push(format!("truncated at {MAX_PLACE_ENTITIES} entities"));
            break;
        }
    }
    (entities, errors)
}

/// Strips every complete marker, returning what the player should see, the markers
/// found, and any text held back because it may be a marker still arriving.
pub fn drain(buffer: &str, final_pass: bool) -> (String, Vec<Marker>, String, Vec<String>) {
    let mut text = buffer.to_string();
    let mut markers = Vec::new();
    let mut errors = Vec::new();

    for caps in clone_like_re().captures_iter(&text.clone()) {
        markers.push(Marker::CloneLike {
            x: num(&caps[1]),
            y: num(&caps[2]),
            dx: num(&caps[3]),
            dy: num(&caps[4]),
            label: label_or(caps.get(5).map(|m| m.as_str()), "copy of this block"),
        });
    }
    text = clone_like_re().replace_all(&text, "").into_owned();

    for caps in clone_re().captures_iter(&text.clone()) {
        markers.push(Marker::Clone {
            x1: num(&caps[2]),
            y1: num(&caps[3]),
            x2: num(&caps[4]),
            y2: num(&caps[5]),
            dx: num(&caps[6]),
            dy: num(&caps[7]),
            label: label_or(caps.get(1).map(|m| m.as_str()), "copy"),
        });
    }
    text = clone_re().replace_all(&text, "").into_owned();

    for caps in place_re().captures_iter(&text.clone()) {
        let (entities, mut errs) = parse_entities(&caps[2]);
        errors.append(&mut errs);
        if !entities.is_empty() {
            markers.push(Marker::Place {
                label: label_or(caps.get(1).map(|m| m.as_str()), "place"),
                entities,
            });
        }
    }
    text = place_re().replace_all(&text, "").into_owned();

    for caps in ping_re().captures_iter(&text.clone()) {
        markers.push(Marker::Ping {
            x: num(&caps[1]),
            y: num(&caps[2]),
            label: label_or(caps.get(3).map(|m| m.as_str()), "LLM Scout"),
        });
    }
    text = ping_re().replace_all(&text, "").into_owned();

    if final_pass {
        return (text, markers, String::new(), errors);
    }

    // Hold back anything that might be the opening of a marker still streaming.
    if let Some(cut) = text.rfind("[[") {
        if !text[cut..].contains("]]") {
            let held = text[cut..].to_string();
            text.truncate(cut);
            return (text, markers, held, errors);
        }
    }
    // A chunk can end on a lone "[" that is really the first half of "[[". Emitting it
    // strips the marker's opening, so the rest arrives piecemeal and never matches.
    if text.ends_with('[') {
        text.pop();
        return (text, markers, "[".to_string(), errors);
    }
    (text, markers, String::new(), errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = "Nearest block is at (-257, -594).

[[clone_like: -257,-594 -> -264,-560 | green circuit block]]

[[ping:-257,-594|source: existing block]]

Icons like [item=electronic-circuit] must survive.

[[build:two more
assembling-machine-3, -182, -441, low-density-structure
requester-chest, -182, -444, copper-plate:200
]]

[[ping:-264,-560|destination]]";

    /// The bug that leaked markers into the chat: a chunk ending on a lone '['.
    #[test]
    fn markers_survive_every_chunk_boundary() {
        for size in 1..60 {
            let mut buffer = String::new();
            let mut shown = String::new();
            let mut all = Vec::new();
            let chars: Vec<char> = ANSWER.chars().collect();
            for piece in chars.chunks(size) {
                buffer.push_str(&piece.iter().collect::<String>());
                let (text, markers, held, _) = drain(&buffer, false);
                shown.push_str(&text);
                all.extend(markers);
                buffer = held;
            }
            let (text, markers, _, _) = drain(&buffer, true);
            shown.push_str(&text);
            all.extend(markers);

            assert!(!shown.contains("[["), "size {size}: marker leaked: {shown:?}");
            assert!(
                shown.contains("[item=electronic-circuit]"),
                "size {size}: rich text was eaten"
            );
            assert_eq!(all.iter().filter(|m| matches!(m, Marker::Ping { .. })).count(), 2,
                       "size {size}: wrong ping count");
            assert_eq!(all.iter().filter(|m| matches!(m, Marker::CloneLike { .. })).count(), 1,
                       "size {size}: wrong clone_like count");
            assert_eq!(all.iter().filter(|m| matches!(m, Marker::Place { .. })).count(), 1,
                       "size {size}: wrong place count");
        }
    }

    #[test]
    fn blank_labels_fall_back_rather_than_reaching_the_player_empty() {
        let cases = [
            ("[[ping:1,2|   ]]", "LLM Scout"),
            ("[[ping:1,2]]", "LLM Scout"),
            ("[[clone_like: 1,2 -> 3,4 |  ]]", "copy of this block"),
            ("[[clone_like: 1,2 -> 3,4]]", "copy of this block"),
            ("[[clone:   \nfrom: 1,2 3,4\nto: 5,6\n]]", "copy"),
            ("[[build:  \nwooden-chest, 1, 2\n]]", "place"),
        ];
        for (input, expected) in cases {
            let (_, markers, _, _) = drain(input, true);
            let label = match markers.first().expect(input) {
                Marker::Ping { label, .. }
                | Marker::CloneLike { label, .. }
                | Marker::Clone { label, .. }
                | Marker::Place { label, .. } => label,
            };
            assert_eq!(label, expected, "for {input:?}");
        }
    }

    #[test]
    fn entity_fields_are_recognised_by_shape() {
        let (entities, errors) = parse_entities(
            "assembling-machine-3, -182, -441, north, low-density-structure\n\
             requester-chest, -182, -444, copper-plate:200, steel-plate:100\n\
             broken line",
        );
        assert!(errors.iter().any(|e| e.contains("line 3")));
        assert_eq!(entities[0].direction.as_deref(), Some("north"));
        assert_eq!(entities[0].recipe.as_deref(), Some("low-density-structure"));
        assert_eq!(entities[1].requests.len(), 2);
        assert_eq!(entities[1].requests[0].count, 200);
    }
}
