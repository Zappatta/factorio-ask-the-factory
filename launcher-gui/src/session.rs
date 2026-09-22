use crate::config::Config;
use crate::factorio;
use crate::rcon::Rcon;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub enum Event {
    Log(String),
    Status(String),
    ServerUp,
    Finished,
    Failed(String),
}

pub struct LaunchOpts {
    pub root: PathBuf,
    pub binary: PathBuf,
    pub data_dir: PathBuf,
    pub save: Option<PathBuf>,
    pub backend: String,
    pub launch_client: bool,
    pub python: String,
    pub cfg: Config,
}

pub struct Session {
    pub events: Receiver<Event>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Session {
    pub fn start(opts: LaunchOpts) -> Session {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_worker = stop.clone();
        let join = thread::spawn(move || {
            if let Err(e) = run(opts, tx.clone(), stop_worker) {
                let _ = tx.send(Event::Failed(e));
            }
            let _ = tx.send(Event::Finished);
        });
        Session {
            events: rx,
            stop,
            join: Some(join),
        }
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn finish(&mut self) {
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

fn pump(mut child_out: impl std::io::Read + Send + 'static, tx: Sender<Event>, tag: &'static str,
        log_path: Option<PathBuf>, up_flag: Option<Arc<AtomicBool>>) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut file = log_path.and_then(|p| fs::File::create(p).ok());
        let reader = BufReader::new(&mut child_out);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(f) = file.as_mut() {
                let _ = writeln!(f, "{line}");
            }
            if let Some(flag) = &up_flag {
                if line.contains("Hosting game at") {
                    flag.store(true, Ordering::SeqCst);
                }
            }
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                let _ = tx.send(Event::Log(format!("[{tag}] {trimmed}")));
            }
        }
    })
}

fn run(opts: LaunchOpts, tx: Sender<Event>, stop: Arc<AtomicBool>) -> Result<(), String> {
    let server_dir = opts.root.join("serverdata");
    let session_save = server_dir.join("saves").join("session.zip");
    let mod_src = opts.root.join("mod");
    let client_mods = opts.data_dir.join("mods");

    tx.send(Event::Status("preparing server directory".into())).ok();
    factorio::write_server_config(&server_dir).map_err(|e| format!("server config: {e}"))?;

    let version = factorio::install_mod_into_client(&mod_src, &client_mods)
        .map_err(|e| format!("installing mod into client: {e}"))?;
    tx.send(Event::Log(format!("[setup] installed llm-scout {version} into client"))).ok();

    let report = factorio::mirror_mods(&client_mods, &server_dir, &mod_src, &version)
        .map_err(|e| format!("mirroring mods: {e}"))?;
    tx.send(Event::Log(format!("[setup] server mods: {}", report.copied.join(", ")))).ok();
    if !report.missing.is_empty() {
        tx.send(Event::Log(format!(
            "[setup] WARNING enabled but not found, the save may refuse to load: {}",
            report.missing.join(", ")
        ))).ok();
    }

    match &opts.save {
        Some(src) => {
            fs::copy(src, &session_save).map_err(|e| format!("copying save: {e}"))?;
            tx.send(Event::Log(format!(
                "[setup] session seeded from {}",
                src.file_name().unwrap_or_default().to_string_lossy()
            ))).ok();
        }
        None => {
            if !session_save.exists() {
                return Err("no existing session save to resume".into());
            }
            tx.send(Event::Log("[setup] resuming existing session save".into())).ok();
        }
    }

    tx.send(Event::Status("starting server".into())).ok();
    let rc = &opts.cfg.rcon;
    let mut server: Child = Command::new(&opts.binary)
        .arg("-c").arg(server_dir.join("config.ini"))
        .arg("--mod-directory").arg(server_dir.join("mods"))
        .arg("--start-server").arg(&session_save)
        .arg("--server-settings").arg(opts.root.join("server-settings.json"))
        .arg("--rcon-bind").arg(format!("{}:{}", rc.host, rc.port))
        .arg("--rcon-password").arg(&rc.password)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start Factorio server: {e}"))?;

    let up = Arc::new(AtomicBool::new(false));
    let out = server.stdout.take().expect("piped");
    let err = server.stderr.take().expect("piped");
    pump(out, tx.clone(), "server", Some(opts.root.join("server.log")), Some(up.clone()));
    pump(err, tx.clone(), "server", None, None);

    let deadline = Instant::now() + Duration::from_secs(150);
    while !up.load(Ordering::SeqCst) {
        if stop.load(Ordering::SeqCst) {
            let _ = server.kill();
            return Ok(());
        }
        if let Ok(Some(status)) = server.try_wait() {
            return Err(format!("server exited during startup ({status}), see server.log"));
        }
        if Instant::now() > deadline {
            let _ = server.kill();
            return Err("server did not report 'Hosting game at' within 150s".into());
        }
        thread::sleep(Duration::from_millis(250));
    }
    tx.send(Event::ServerUp).ok();
    tx.send(Event::Status("server up".into())).ok();

    let mut bridge = Command::new(&opts.python)
        .arg(opts.root.join("bridge").join("bridge.py"))
        .arg("--config").arg(opts.root.join("config.toml"))
        .arg("--backend").arg(&opts.backend)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start bridge ({}): {e}", opts.python))?;
    if let Some(o) = bridge.stdout.take() {
        pump(o, tx.clone(), "bridge", None, None);
    }
    if let Some(e) = bridge.stderr.take() {
        pump(e, tx.clone(), "bridge", None, None);
    }
    tx.send(Event::Log(format!("[setup] bridge started on backend {}", opts.backend))).ok();

    let mut client: Option<Child> = None;
    if opts.launch_client {
        match Command::new(&opts.binary)
            .arg("--mp-connect")
            .arg(format!("127.0.0.1:{}", opts.cfg.server.port))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => {
                client = Some(c);
                tx.send(Event::Log("[setup] client launched, connecting to localhost".into())).ok();
            }
            Err(e) => {
                tx.send(Event::Log(format!("[setup] could not launch client: {e}"))).ok();
            }
        }
    }

    tx.send(Event::Status("running".into())).ok();
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if let Ok(Some(status)) = server.try_wait() {
            tx.send(Event::Log(format!("[server] exited ({status})"))).ok();
            let _ = bridge.kill();
            if let Some(c) = client.as_mut() {
                let _ = c.kill();
            }
            return Ok(());
        }
        thread::sleep(Duration::from_millis(300));
    }

    // Quit over RCON so the server writes a final save.
    tx.send(Event::Status("shutting down".into())).ok();
    let quit = Rcon::connect(&rc.host, rc.port, &rc.password, Duration::from_secs(5))
        .and_then(|mut c| c.command("/quit"));
    match quit {
        Ok(_) => tx.send(Event::Log("[setup] sent /quit, server is saving".into())).ok(),
        Err(e) => {
            tx.send(Event::Log(format!("[setup] RCON quit failed ({e}), killing server"))).ok();
            let _ = server.kill();
            None
        }
    };

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match server.try_wait() {
            Ok(Some(_)) => break,
            _ if Instant::now() > deadline => {
                let _ = server.kill();
                break;
            }
            _ => thread::sleep(Duration::from_millis(300)),
        }
    }
    let _ = bridge.kill();
    if let Some(c) = client.as_mut() {
        let _ = c.kill();
    }
    tx.send(Event::Log("[setup] stopped, session save kept".into())).ok();
    Ok(())
}

pub fn detect_python() -> Option<String> {
    for cand in ["python3", "python"] {
        if Command::new(cand)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some(cand.to_string());
        }
    }
    None
}
