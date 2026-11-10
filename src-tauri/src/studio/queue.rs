//! Job queue (port of the v3.7 `jobs.py` queue): one queue, one worker, one GPU, so two
//! generations never fight over the same card. Jobs run in order on a background thread:
//! prepare the take folder -> send to the engine -> wait -> write meta.json, or move the
//! folder to trash when the job failed or was cancelled.

use super::pipeline::{self, Outcome};
use crate::engine::{Engine, EngineError, JobStatus, Launch, Submit};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

const KEEP_JOBS: usize = 100;

/// What a job needs from the outside world, read when it starts (so settings changes apply).
pub struct RunEnv {
    pub launch: Launch,
    pub ace_dir: PathBuf,
    pub ws: PathBuf,
    pub cfg: Value,
    pub tier: String,
}

pub type EnvFn = dyn Fn() -> Result<RunEnv, EngineError> + Send + Sync;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QStatus {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueJob {
    pub id: String,
    pub title: String,
    pub status: QStatus,
    /// Title, style, caption, duration, bpm, key, quality, variants, workflow.
    pub spec: Value,
    pub created: String,
    pub phase: String,
    pub pct: f64,
    pub warnings: Vec<String>,
    pub error: Option<EngineError>,
    pub take_id: Option<String>,
    pub tier: Option<String>,
    pub quality: Option<String>,
    pub params: Value,
    pub elapsed_s: Option<f64>,
    pub cancelling: bool,
    pub queue_position: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueView {
    pub jobs: Vec<QueueJob>,
    pub current: Option<String>,
}

struct State {
    jobs: Vec<QueueJob>,
    specs: std::collections::HashMap<String, Value>,
    current: Option<String>,
    cancel_current: bool,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
    engine: Engine,
    env: Box<EnvFn>,
    notify: Box<dyn Fn(QueueView) + Send + Sync>,
}

#[derive(Clone)]
pub struct Queue {
    s: Arc<Shared>,
}

impl Queue {
    /// Starts the runner thread. `notify` gets a snapshot after every change.
    pub fn new(engine: Engine, env: Box<EnvFn>, notify: impl Fn(QueueView) + Send + Sync + 'static) -> Self {
        let state = State { jobs: vec![], specs: Default::default(), current: None, cancel_current: false };
        let s = Arc::new(Shared { state: Mutex::new(state), cv: Condvar::new(), engine, env, notify: Box::new(notify) });
        let q = Self { s };
        let runner = q.clone();
        std::thread::spawn(move || runner.run());
        q
    }

    pub fn submit(&self, spec: Value) -> Result<String, EngineError> {
        if !spec.get("caption").and_then(Value::as_str).is_some_and(|c| !c.trim().is_empty()) {
            return Err(EngineError::new("empty_caption", "spec has no caption", ""));
        }
        let id = format!("j{}{:04x}", chrono::Local::now().format("%m%d%H%M%S"), rand_u16());
        let summary: serde_json::Map<String, Value> =
            ["title", "style", "caption", "duration", "bpm", "key", "quality", "variants", "workflow"]
                .iter()
                .map(|k| (k.to_string(), spec.get(*k).cloned().unwrap_or(Value::Null)))
                .collect();
        let title = ["title", "style"].iter().find_map(|k| spec.get(*k).and_then(Value::as_str)).unwrap_or("track").to_string();
        let mut g = self.lock();
        g.jobs.push(QueueJob {
            id: id.clone(),
            title,
            status: QStatus::Queued,
            spec: Value::Object(summary),
            created: chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
            phase: "queued".into(),
            pct: 0.0,
            warnings: vec![],
            error: None,
            take_id: None,
            tier: None,
            quality: None,
            params: Value::Null,
            elapsed_s: None,
            cancelling: false,
            queue_position: None,
        });
        g.specs.insert(id.clone(), spec);
        trim(&mut g);
        self.s.cv.notify_all();
        self.publish(g);
        Ok(id)
    }

    /// Cancel a queued job, or stop the running one (kills the worker). `false` for unknown/finished jobs.
    pub fn cancel(&self, id: &str) -> bool {
        let mut g = self.lock();
        let running = g.current.as_deref() == Some(id);
        let Some(job) = g.jobs.iter_mut().find(|j| j.id == id) else { return false };
        match job.status {
            QStatus::Queued => {
                job.status = QStatus::Cancelled;
                job.phase = "cancelled".into();
                self.publish(g);
                true
            }
            QStatus::Running if running => {
                job.cancelling = true;
                g.cancel_current = true;
                self.s.cv.notify_all();
                self.publish(g);
                self.s.engine.cancel();
                true
            }
            _ => false,
        }
    }

    pub fn view(&self) -> QueueView {
        view(&self.lock())
    }

    // ------------------------------------------------------------------ runner

    fn lock(&self) -> MutexGuard<'_, State> {
        self.s.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn publish(&self, g: MutexGuard<'_, State>) {
        let v = view(&g);
        drop(g);
        (self.s.notify)(v);
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut QueueJob)) {
        let mut g = self.lock();
        if let Some(job) = g.jobs.iter_mut().find(|j| j.id == id) {
            f(job);
        }
        self.publish(g);
    }

    fn run(&self) {
        loop {
            let (id, spec) = {
                let mut g = self.lock();
                loop {
                    if let Some(job) = g.jobs.iter_mut().find(|j| j.status == QStatus::Queued) {
                        job.status = QStatus::Running;
                        job.phase = "preparing".into();
                        let id = job.id.clone();
                        let spec = g.specs.get(&id).cloned().unwrap_or(Value::Null);
                        g.current = Some(id.clone());
                        g.cancel_current = false;
                        break (id, spec);
                    }
                    g = self.s.cv.wait(g).unwrap_or_else(|e| e.into_inner());
                }
            };
            self.update(&id, |_| {});
            let end = self.run_one(&id, &spec);
            let mut g = self.lock();
            if let Some(job) = g.jobs.iter_mut().find(|j| j.id == id) {
                end(job);
                job.cancelling = false;
            }
            g.current = None;
            g.cancel_current = false;
            g.specs.remove(&id);
            self.publish(g);
        }
    }

    fn cancelled(&self) -> bool {
        self.lock().cancel_current
    }

    /// Runs one job; returns how to finalise its record.
    fn run_one(&self, id: &str, spec: &Value) -> Box<dyn FnOnce(&mut QueueJob)> {
        let fail = |e: EngineError| -> Box<dyn FnOnce(&mut QueueJob)> {
            Box::new(move |j: &mut QueueJob| {
                j.status = QStatus::Failed;
                j.phase = "failed".into();
                j.error = Some(e);
            })
        };
        let cancelled: Box<dyn FnOnce(&mut QueueJob)> = Box::new(|j: &mut QueueJob| {
            j.status = QStatus::Cancelled;
            j.phase = "cancelled".into();
        });
        let env = match (self.s.env)() {
            Ok(env) => env,
            Err(e) => return fail(e),
        };
        let p = match pipeline::prepare(&env.cfg, &env.ws, spec, &env.tier, &env.ace_dir, id) {
            Ok(p) => p,
            Err(e) => return fail(EngineError::new("prepare", e, "")),
        };
        let (tier, quality, params, warns) =
            (p.resolved.tier.clone(), p.resolved.quality.clone(), Value::Object(p.resolved.params.clone()), p.warnings.clone());
        self.update(id, move |j| {
            j.tier = Some(tier);
            j.quality = Some(quality);
            j.params = params;
            j.warnings = warns;
        });
        let discard = |reason: &str| {
            let _ = pipeline::discard(&env.ws, &p.out_dir, reason, Some(id));
        };
        if self.cancelled() {
            discard("cancelled");
            return cancelled;
        }
        let caption = spec["caption"].as_str().unwrap_or_default().to_string();
        let submit = Submit { id: id.to_string(), caption, out_dir: p.out_dir.clone(), request: p.request.clone() };
        if let Err(e) = self.s.engine.generate(&env.launch, submit) {
            discard(&e.message);
            return fail(e);
        }
        // Follow the engine until this job ends.
        let ended = loop {
            let status = self.s.engine.status();
            match status.job.filter(|j| j.id == id) {
                Some(j) if j.status == JobStatus::Running => {
                    let (phase, pct, w) = (j.phase.clone(), j.pct, j.warnings.clone());
                    let mut g = self.lock();
                    if let Some(job) = g.jobs.iter_mut().find(|q| q.id == id)
                        && (job.phase != phase || job.pct != pct || w.iter().any(|x| !job.warnings.contains(x))) {
                            job.phase = phase;
                            job.pct = pct;
                            for x in w {
                                if !job.warnings.contains(&x) {
                                    job.warnings.push(x);
                                }
                            }
                            self.publish(g);
                        }
                }
                Some(j) => break Some(j),
                None => break None,
            }
            let g = self.lock();
            let _ = self.s.cv.wait_timeout(g, Duration::from_millis(250));
        };
        let Some(j) = ended else {
            discard("the engine lost track of the job");
            return fail(EngineError::new("lost", "the engine lost track of this job", ""));
        };
        match j.status {
            JobStatus::Done => {
                let out = Outcome {
                    variants: serde_json::to_value(&j.variants).unwrap_or(json!([])),
                    warnings: j.warnings.clone(),
                    effective: j.effective.clone(),
                    lm_metadata: j.lm_metadata.clone(),
                    time_costs: j.time_costs.clone(),
                    elapsed_s: j.elapsed_s.unwrap_or(0.0),
                };
                match pipeline::finish(&p, id, &out) {
                    Ok(meta) => {
                        let take_id = p.take_id.clone();
                        Box::new(move |q: &mut QueueJob| {
                            q.status = QStatus::Done;
                            q.phase = "done".into();
                            q.pct = 1.0;
                            q.take_id = Some(take_id);
                            q.elapsed_s = meta["elapsed_s"].as_f64();
                            q.warnings = meta["warnings"].as_array().into_iter().flatten().filter_map(|w| w.as_str().map(String::from)).collect();
                        })
                    }
                    Err(e) => {
                        discard("cannot write meta.json");
                        fail(EngineError::new("finish", format!("cannot write meta.json: {e}"), ""))
                    }
                }
            }
            JobStatus::Cancelled => {
                discard("cancelled");
                cancelled
            }
            _ => {
                let e = j.error.unwrap_or_else(|| EngineError::new("failed", "generation failed", ""));
                discard(&e.message);
                fail(e)
            }
        }
    }
}

fn view(g: &State) -> QueueView {
    let mut pos = 0;
    let jobs = g
        .jobs
        .iter()
        .map(|j| {
            let mut j = j.clone();
            if j.status == QStatus::Queued {
                pos += 1;
                j.queue_position = Some(pos);
            }
            j
        })
        .collect();
    QueueView { jobs, current: g.current.clone() }
}

/// Keep the newest finished jobs; never drop queued or running ones.
fn trim(g: &mut State) {
    while g.jobs.len() > KEEP_JOBS {
        let Some(i) = g.jobs.iter().position(|j| matches!(j.status, QStatus::Done | QStatus::Failed | QStatus::Cancelled)) else {
            break;
        };
        let id = g.jobs.remove(i).id;
        g.specs.remove(&id);
    }
}

fn rand_u16() -> u16 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| (d.subsec_nanos() >> 8) as u16)
}
