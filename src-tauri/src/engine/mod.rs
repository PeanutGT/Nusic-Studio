//! Engine bridge: owns the long-lived Python ACE-Step worker (`resources/ace_worker.py --serve`).
//!
//! One worker, one job at a time. The worker keeps models loaded between jobs.
//! Cancel kills the worker (the only way to stop ACE-Step mid-diffusion and
//! free the VRAM); the next job starts a fresh worker. A worker that exits on
//! its own fails the running job and is restarted by the next job.

pub mod gpu;
pub mod protocol;
pub mod python;

pub use protocol::{GenerateInput, Variant};

use protocol::Event;
use serde::Serialize;
use serde_json::Value;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LOG_LINES: usize = 300;

/// How to start the worker: program + args, run in `cwd` (ACE-Step's folder).
#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    Stopped,
    Starting,
    Idle,
    Busy,
    Crashed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EngineError {
    pub code: String,
    pub message: String,
    pub hint: String,
}

impl EngineError {
    pub fn new(code: &str, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), hint: hint.into() }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JobView {
    pub id: String,
    pub caption: String,
    pub status: JobStatus,
    pub phase: String,
    pub pct: f64,
    pub detail: String,
    pub warnings: Vec<String>,
    pub error: Option<EngineError>,
    pub out_dir: String,
    pub variants: Vec<Variant>,
    pub effective: Value,
    pub elapsed_s: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineStatus {
    pub state: WorkerState,
    pub pid: Option<u32>,
    pub models_loaded: bool,
    pub model: Option<String>,
    /// As last reported by the worker (torch's view). Live numbers come from `gpu::query`.
    pub vram_total_gb: Option<f64>,
    pub vram_free_gb: Option<f64>,
    pub job: Option<JobView>,
}

struct Proc {
    child: Child,
    stdin: ChildStdin,
    launch: Launch,
}

struct Inner {
    proc: Option<Proc>,
    /// Bumped whenever this side stops a worker, so its reader thread knows the exit was intended.
    generation: u64,
    status: EngineStatus,
    log: VecDeque<String>,
}

type Notify = dyn Fn(EngineStatus) + Send + Sync;

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Mutex<Inner>>,
    notify: Arc<Notify>,
}

impl Engine {
    /// `notify` receives a status snapshot after every change (called outside the engine lock).
    pub fn new(notify: impl Fn(EngineStatus) + Send + Sync + 'static) -> Self {
        let status = EngineStatus {
            state: WorkerState::Stopped,
            pid: None,
            models_loaded: false,
            model: None,
            vram_total_gb: None,
            vram_free_gb: None,
            job: None,
        };
        let inner = Inner { proc: None, generation: 0, status, log: VecDeque::new() };
        Self { inner: Arc::new(Mutex::new(inner)), notify: Arc::new(notify) }
    }

    pub fn status(&self) -> EngineStatus {
        self.lock().status.clone()
    }

    pub fn log_tail(&self) -> Vec<String> {
        self.lock().log.iter().cloned().collect()
    }

    /// Start one generation into a new take folder under `<workspace>/output`. Returns the job id.
    pub fn generate(&self, launch: &Launch, ace_dir: &Path, workspace: &Path, input: GenerateInput) -> Result<String, EngineError> {
        if input.caption.trim().is_empty() {
            return Err(EngineError::new("empty_caption", "caption is empty", ""));
        }
        let mut g = self.lock();
        if g.status.job.as_ref().is_some_and(|j| j.status == JobStatus::Running) {
            return Err(EngineError::new("busy", "a generation is already running", "Wait for it or cancel it first."));
        }
        if g.proc.as_ref().is_none_or(|p| p.launch != *launch) {
            self.stop_locked(&mut g);
            self.spawn_locked(&mut g, launch)?;
        }
        let out_dir = new_take_dir(&workspace.join("output"), &input.caption)
            .map_err(|e| EngineError::new("output_dir", format!("cannot create the output folder: {e}"), ""))?;
        let job = format!("j{}", now_millis());
        let seed = if input.seed < 0 { random_seed() } else { input.seed };
        let req = protocol::generate_request(&job, &input, seed, ace_dir, &out_dir);
        let line = format!("{req}\n");
        let proc = g.proc.as_mut().expect("worker was just started");
        if let Err(e) = proc.stdin.write_all(line.as_bytes()).and_then(|_| proc.stdin.flush()) {
            let _ = std::fs::remove_dir_all(&out_dir);
            self.stop_locked(&mut g);
            return Err(EngineError::new("worker_unavailable", format!("cannot send the job to the worker: {e}"), ""));
        }
        g.status.state = WorkerState::Busy;
        g.status.job = Some(JobView {
            id: job.clone(),
            caption: input.caption,
            status: JobStatus::Running,
            phase: "queued".into(),
            pct: 0.0,
            detail: String::new(),
            warnings: Vec::new(),
            error: None,
            out_dir: out_dir.to_string_lossy().into_owned(),
            variants: Vec::new(),
            effective: Value::Null,
            elapsed_s: None,
        });
        self.publish(g);
        Ok(job)
    }

    /// Cancel the running job by killing the worker. `false` when nothing was running.
    pub fn cancel(&self) -> bool {
        let mut g = self.lock();
        let running = g.status.job.as_ref().is_some_and(|j| j.status == JobStatus::Running);
        if !running {
            return false;
        }
        self.kill_locked(&mut g);
        if let Some(job) = g.status.job.as_mut() {
            job.status = JobStatus::Cancelled;
            job.phase = "cancelled".into();
            let _ = std::fs::remove_dir_all(&job.out_dir);
        }
        self.publish(g);
        true
    }

    /// Stop the worker (if any) and start a fresh one. Cancels a running job.
    pub fn restart(&self, launch: &Launch) -> Result<(), EngineError> {
        if !self.cancel() {
            let mut g = self.lock();
            self.stop_locked(&mut g);
        }
        let mut g = self.lock();
        let res = self.spawn_locked(&mut g, launch);
        self.publish(g);
        res
    }

    /// Ask the worker to quit (models unloaded), killing it if it does not. Used on app exit.
    pub fn shutdown(&self) {
        if !self.cancel() {
            let mut g = self.lock();
            self.stop_locked(&mut g);
            self.publish(g);
        }
    }

    // ------------------------------------------------------------------ internals

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn publish(&self, g: MutexGuard<'_, Inner>) {
        let snapshot = g.status.clone();
        drop(g);
        (self.notify)(snapshot);
    }

    fn spawn_locked(&self, g: &mut Inner, launch: &Launch) -> Result<(), EngineError> {
        let mut cmd = Command::new(&launch.program);
        cmd.args(&launch.args)
            .current_dir(&launch.cwd)
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUNBUFFERED", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        no_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| {
            EngineError::new("spawn_failed", format!("cannot start the ACE-Step worker ({}): {e}", launch.program), "")
        })?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        g.generation += 1;
        let generation = g.generation;
        g.status.state = WorkerState::Starting;
        g.status.pid = Some(child.id());
        g.status.models_loaded = false;
        g.status.model = None;
        g.proc = Some(Proc { child, stdin, launch: launch.clone() });

        let me = self.clone();
        std::thread::spawn(move || me.read_stdout(generation, stdout));
        let me = self.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                let mut g = me.lock();
                if g.log.len() == LOG_LINES {
                    g.log.pop_front();
                }
                g.log.push_back(line);
            }
        });
        Ok(())
    }

    fn read_stdout(&self, generation: u64, stdout: std::process::ChildStdout) {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Some(event) = protocol::parse_line(&line) else { continue };
            let mut g = self.lock();
            if g.generation != generation {
                return; // this worker was stopped on purpose
            }
            apply(&mut g.status, event);
            self.publish(g);
        }
        // stdout closed: the worker exited. Unexpected unless we stopped it.
        let mut g = self.lock();
        if g.generation != generation {
            return;
        }
        let code = g.proc.take().map(|mut p| p.child.wait().ok().and_then(|s| s.code()));
        let code = code.flatten().map_or("unknown".to_string(), |c| c.to_string());
        g.status.state = WorkerState::Crashed;
        g.status.pid = None;
        g.status.models_loaded = false;
        g.status.model = None;
        let tail: Vec<String> = g.log.iter().rev().take(5).rev().cloned().collect();
        if let Some(job) = g.status.job.as_mut().filter(|j| j.status == JobStatus::Running) {
            job.status = JobStatus::Failed;
            job.error = Some(EngineError::new(
                "engine_crashed",
                format!("the ACE-Step worker exited with code {code} without a result"),
                tail.join("\n"),
            ));
            let _ = std::fs::remove_dir_all(&job.out_dir);
        }
        self.publish(g);
    }

    /// Graceful stop: `quit`, then kill after a few seconds.
    fn stop_locked(&self, g: &mut Inner) {
        let Some(proc) = g.proc.as_mut() else { return };
        let _ = proc.stdin.write_all(b"{\"op\":\"quit\"}\n").and_then(|_| proc.stdin.flush());
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if matches!(proc.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.kill_locked(g);
    }

    fn kill_locked(&self, g: &mut Inner) {
        g.generation += 1;
        if let Some(mut proc) = g.proc.take() {
            kill_tree(&mut proc.child);
        }
        g.status.state = WorkerState::Stopped;
        g.status.pid = None;
        g.status.models_loaded = false;
        g.status.model = None;
    }
}

fn apply(status: &mut EngineStatus, event: Event) {
    let job = status.job.as_mut().filter(|j| j.status == JobStatus::Running);
    match event {
        Event::Ready { models_loaded, model, vram_total_gb, vram_free_gb } => {
            status.models_loaded = models_loaded;
            status.model = model;
            if vram_total_gb.is_some() {
                status.vram_total_gb = vram_total_gb;
                status.vram_free_gb = vram_free_gb;
            }
            // The start-up `ready` can arrive after a job was already sent.
            status.state = if job.is_some() { WorkerState::Busy } else { WorkerState::Idle };
        }
        Event::Phase { name, pct, detail } => {
            if let Some(job) = job {
                job.phase = name;
                job.pct = pct;
                job.detail = detail;
            }
        }
        Event::Warn { message, .. } => {
            if let Some(job) = job {
                push_unique(&mut job.warnings, message);
            }
        }
        Event::Retry { reason } => {
            if let Some(job) = job {
                push_unique(&mut job.warnings, format!("retrying: {reason}"));
            }
        }
        Event::Result { variants, effective, warnings, elapsed_s } => {
            if let Some(job) = job {
                job.status = JobStatus::Done;
                job.phase = "done".into();
                job.pct = 1.0;
                job.variants = variants;
                job.effective = effective;
                job.elapsed_s = elapsed_s;
                for w in warnings {
                    push_unique(&mut job.warnings, w);
                }
            }
        }
        Event::Error { code, message, hint } => {
            if let Some(job) = job {
                job.status = JobStatus::Failed;
                job.error = Some(EngineError { code, message, hint });
                let _ = std::fs::remove_dir_all(&job.out_dir);
            }
        }
        Event::Other => {}
    }
}

fn push_unique(list: &mut Vec<String>, item: String) {
    if !list.contains(&item) {
        list.push(item);
    }
}

/// Kill the worker and everything it started (`uv run` spawns the real python as a child).
fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/T", "/F", "/PID", &child.id().to_string()]).stdout(Stdio::null()).stderr(Stdio::null());
        no_window(&mut cmd);
        let _ = cmd.status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Keep child processes from opening a console window on Windows.
pub(crate) fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// `<root>/<YYYYmmdd-HHMMSS>-<slug>`, made unique with `-2`, `-3`, ... (same naming as v3.7 takes).
pub fn new_take_dir(root: &Path, caption: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(root)?;
    let base = format!("{}-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"), slugify(caption));
    let mut dir = root.join(&base);
    let mut n = 2;
    while dir.exists() {
        dir = root.join(format!("{base}-{n}"));
        n += 1;
    }
    std::fs::create_dir(&dir)?;
    Ok(dir)
}

pub fn slugify(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 32 {
            break;
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() { "track".into() } else { out.into() }
}

fn now_millis() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

fn random_seed() -> i64 {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos() as i64 ^ d.as_secs() as i64);
    1 + nanos.rem_euclid(2_147_483_646)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_keeps_ascii_words() {
        assert_eq!(slugify("Calm Piano, rain & cafe!"), "calm-piano-rain-cafe");
        assert_eq!(slugify("戰敗的安魂曲"), "track");
        assert_eq!(slugify("lofi 戰鬥 beat"), "lofi-beat");
        assert!(slugify(&"a".repeat(100)).len() <= 32);
    }

    #[test]
    fn take_dirs_never_collide() {
        let root = std::env::temp_dir().join(format!("nusic-take-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let a = new_take_dir(&root, "x").unwrap();
        let b = new_take_dir(&root, "x").unwrap();
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
    }

    #[test]
    fn random_seed_is_positive_31_bit() {
        let s = random_seed();
        assert!((1..=2_147_483_646).contains(&s));
    }
}
