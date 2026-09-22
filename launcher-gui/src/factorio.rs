use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModEntry {
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModList {
    pub mods: Vec<ModEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct ModInfo {
    version: String,
}

#[derive(Debug, Clone)]
pub struct SaveEntry {
    pub path: PathBuf,
    pub name: String,
    pub modified: SystemTime,
    pub size: u64,
}

impl SaveEntry {
    pub fn is_autosave(&self) -> bool {
        self.name.starts_with("_autosave")
    }

    pub fn size_label(&self) -> String {
        format!("{:.1} MB", self.size as f64 / 1024.0 / 1024.0)
    }

    pub fn age_label(&self) -> String {
        let secs = self
            .modified
            .elapsed()
            .map(|d| d.as_secs())
            .unwrap_or(0);
        const MIN: u64 = 60;
        const HOUR: u64 = 60 * MIN;
        const DAY: u64 = 24 * HOUR;
        const MONTH: u64 = 30 * DAY;
        const YEAR: u64 = 365 * DAY;
        let (n, unit) = match secs {
            s if s < 90 => return "just now".into(),
            s if s < HOUR => (s / MIN, "min"),
            s if s < DAY => (s / HOUR, "hour"),
            s if s < MONTH => (s / DAY, "day"),
            s if s < YEAR => (s / MONTH, "month"),
            s => (s / YEAR, "year"),
        };
        format!("{} {}{} ago", n, unit, if n == 1 { "" } else { "s" })
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn steam_root() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/Steam")
    } else if cfg!(target_os = "windows") {
        PathBuf::from(
            std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| "C:\\Program Files (x86)".into()),
        )
        .join("Steam")
    } else {
        home().join(".steam/steam")
    }
}

pub fn candidate_data_dirs() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![home().join("Library/Application Support/factorio")]
    } else if cfg!(target_os = "windows") {
        let appdata = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(home);
        vec![appdata.join("Factorio"), home().join("Factorio")]
    } else {
        vec![home().join(".factorio")]
    }
}

pub fn candidate_binaries() -> Vec<PathBuf> {
    let steam = steam_root();
    let mut out = if cfg!(target_os = "macos") {
        vec![
            steam.join("steamapps/common/Factorio/factorio.app/Contents/MacOS/factorio"),
            PathBuf::from("/Applications/factorio.app/Contents/MacOS/factorio"),
            home().join("Applications/factorio.app/Contents/MacOS/factorio"),
        ]
    } else if cfg!(target_os = "windows") {
        vec![
            steam.join("steamapps/common/Factorio/bin/x64/factorio.exe"),
            PathBuf::from("C:\\Program Files\\Factorio\\bin\\x64\\factorio.exe"),
            home().join("Factorio/bin/x64/factorio.exe"),
        ]
    } else {
        vec![
            steam.join("steamapps/common/Factorio/bin/x64/factorio"),
            home().join(".local/share/Steam/steamapps/common/Factorio/bin/x64/factorio"),
            PathBuf::from("/usr/share/factorio/bin/x64/factorio"),
            home().join("factorio/bin/x64/factorio"),
        ]
    };
    out.extend(steam_library_binaries());
    out
}

/// Factorio may live in a secondary Steam library; libraryfolders.vdf lists them.
fn steam_library_binaries() -> Vec<PathBuf> {
    let vdf = steam_root().join("steamapps/libraryfolders.vdf");
    let Ok(text) = fs::read_to_string(&vdf) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        if !line.contains("\"path\"") {
            continue;
        }
        let parts: Vec<&str> = line.split('"').collect();
        if parts.len() < 4 {
            continue;
        }
        let base = PathBuf::from(parts[3].replace("\\\\", "\\")).join("steamapps/common/Factorio");
        for rel in [
            "bin/x64/factorio.exe",
            "bin/x64/factorio",
            "factorio.app/Contents/MacOS/factorio",
        ] {
            out.push(base.join(rel));
        }
    }
    out
}

pub fn detect_data_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FACTORIO_DIR").map(PathBuf::from) {
        if p.is_dir() {
            return Some(p);
        }
    }
    candidate_data_dirs()
        .into_iter()
        .find(|p| p.join("saves").is_dir() || p.join("mods").is_dir())
}

pub fn detect_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FACTORIO_BIN").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    candidate_binaries().into_iter().find(|p| p.is_file())
}

pub fn list_saves(data_dir: &Path) -> Vec<SaveEntry> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(data_dir.join("saves")) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("zip") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        out.push(SaveEntry {
            path,
            name,
            modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            size: meta.len(),
        });
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

pub fn write_server_config(server_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(server_dir.join("saves"))?;
    fs::create_dir_all(server_dir.join("script-output"))?;
    fs::create_dir_all(server_dir.join("mods"))?;
    fs::write(
        server_dir.join("config.ini"),
        format!(
            "[path]\nread-data=__PATH__system-read-data__\nwrite-data={}\n\n[general]\nlocale=auto\n",
            server_dir.display()
        ),
    )
}

fn mod_version(mod_src: &Path) -> io::Result<String> {
    let raw = fs::read_to_string(mod_src.join("info.json"))?;
    let info: ModInfo = serde_json::from_str(&raw)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(info.version)
}

fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

/// The connecting client needs the mod too, or multiplayer refuses to join.
pub fn install_mod_into_client(mod_src: &Path, client_mods: &Path) -> io::Result<String> {
    let version = mod_version(mod_src)?;
    let target = client_mods.join(format!("llm-scout_{version}"));
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    copy_dir(mod_src, &target)?;

    let list_path = client_mods.join("mod-list.json");
    let mut list: ModList = fs::read_to_string(&list_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(ModList { mods: Vec::new() });
    match list.mods.iter_mut().find(|m| m.name == "llm-scout") {
        Some(m) => m.enabled = true,
        None => list.mods.push(ModEntry {
            name: "llm-scout".into(),
            enabled: true,
        }),
    }
    fs::write(
        &list_path,
        serde_json::to_string_pretty(&list)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
    )?;
    Ok(version)
}

pub struct MirrorReport {
    pub copied: Vec<String>,
    pub missing: Vec<String>,
}

pub fn mirror_mods(
    client_mods: &Path,
    server_dir: &Path,
    mod_src: &Path,
    version: &str,
) -> io::Result<MirrorReport> {
    let dst = server_dir.join("mods");
    if dst.exists() {
        for entry in fs::read_dir(&dst)?.flatten() {
            let p = entry.path();
            if p.is_dir() {
                fs::remove_dir_all(&p)?;
            } else {
                fs::remove_file(&p)?;
            }
        }
    }
    fs::create_dir_all(&dst)?;

    let list: ModList = fs::read_to_string(client_mods.join("mod-list.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(ModList { mods: Vec::new() });

    let mut enabled: Vec<String> = list
        .mods
        .iter()
        .filter(|m| m.enabled && m.name != "base")
        .map(|m| m.name.clone())
        .collect();
    if !enabled.iter().any(|n| n == "llm-scout") {
        enabled.push("llm-scout".into());
    }
    enabled.sort();
    enabled.dedup();

    let mut copied = Vec::new();
    let mut missing = Vec::new();

    for name in &enabled {
        if name == "llm-scout" {
            copy_dir(mod_src, &dst.join(format!("llm-scout_{version}")))?;
            copied.push(format!("llm-scout {version}"));
            continue;
        }
        let mut best: Option<PathBuf> = None;
        for entry in fs::read_dir(client_mods)?.flatten() {
            let p = entry.path();
            let Some(fname) = p.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if !fname.starts_with(&format!("{name}_")) {
                continue;
            }
            if p.is_file() && !fname.ends_with(".zip") {
                continue;
            }
            if best.as_ref().map(|b| fname > b.file_name().unwrap().to_str().unwrap()) != Some(false)
            {
                best = Some(p);
            }
        }
        match best {
            Some(p) if p.is_dir() => {
                let fname = p.file_name().unwrap().to_owned();
                copy_dir(&p, &dst.join(&fname))?;
                copied.push(fname.to_string_lossy().into_owned());
            }
            Some(p) => {
                let fname = p.file_name().unwrap().to_owned();
                fs::copy(&p, dst.join(&fname))?;
                copied.push(fname.to_string_lossy().trim_end_matches(".zip").to_string());
            }
            None => missing.push(name.clone()),
        }
    }

    let mut server_list = vec![ModEntry {
        name: "base".into(),
        enabled: true,
    }];
    for name in &enabled {
        server_list.push(ModEntry {
            name: name.clone(),
            enabled: true,
        });
    }
    fs::write(
        dst.join("mod-list.json"),
        serde_json::to_string_pretty(&ModList { mods: server_list })
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
    )?;

    let settings = client_mods.join("mod-settings.dat");
    if settings.exists() {
        fs::copy(settings, dst.join("mod-settings.dat"))?;
    }
    Ok(MirrorReport { copied, missing })
}
