//! Job queue + pipeline against tests/fake_worker.py: order, meta.json, trash on cancel / failure.

use nusic_studio_lib::engine::{Engine, EngineError, Launch};
use nusic_studio_lib::studio::api::Studio;
use nusic_studio_lib::studio::plan::PlanInput;
use nusic_studio_lib::studio::queue::{QStatus, Queue, QueueView, RunEnv};
use nusic_studio_lib::studio::takes::Library;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn python() -> String {
    for name in ["python3", "python"] {
        if Command::new(name).arg("--version").output().is_ok_and(|o| o.status.success()) {
            return name.into();
        }
    }
    panic!("tests need python3 or python on PATH");
}

fn studio() -> Studio {
    Studio { defaults_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("resources").join("defaults") }
}

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nusic-queue-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// Queue on the fake worker; `ok_env = false` makes every job fail before it reaches the engine.
fn queue(ws: &Path, ok_env: bool) -> (Queue, mpsc::Receiver<QueueView>) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fake_worker.py");
    let launch = Launch { program: python(), args: vec![script.to_string_lossy().into_owned()], cwd: std::env::temp_dir() };
    let ws = ws.to_path_buf();
    let env = Box::new(move || {
        if !ok_env {
            return Err(EngineError::new("ace_dir_unset", "ACE-Step folder is not set", ""));
        }
        let (cfg, _) = studio().config(&ws).map_err(|e| EngineError::new("studio", e, ""))?;
        Ok(RunEnv { launch: launch.clone(), ace_dir: PathBuf::from("/ace"), ws: ws.clone(), cfg, tier: "low".into() })
    });
    let (tx, rx) = mpsc::channel();
    let q = Queue::new(Engine::new(|_| {}), env, move |v| {
        let _ = tx.send(v);
    });
    (q, rx)
}

/// The fake worker reacts to the caption ("ok", "slow", "error"), so override it.
fn spec(ws: &Path, caption: &str, quality: &str) -> Value {
    let input = PlanInput {
        style: "lofi".into(),
        caption_override: Some(caption.into()),
        quality: Some(quality.into()),
        seed: Some(7),
        ..Default::default()
    };
    studio().spec(ws, &input).unwrap()
}

fn wait(rx: &mpsc::Receiver<QueueView>, what: &str, pred: impl Fn(&QueueView) -> bool) -> QueueView {
    let deadline = Instant::now() + Duration::from_secs(20);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(v) if pred(&v) => return v,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    panic!("timed out waiting for: {what}");
}

fn status(v: &QueueView, id: &str) -> Option<QStatus> {
    v.jobs.iter().find(|j| j.id == id).map(|j| j.status)
}

fn lib(ws: &Path) -> Library {
    Library { ws: ws.to_path_buf(), out_root: ws.join("output") }
}

#[test]
fn runs_jobs_in_order_and_writes_takes_with_preset_params() {
    let ws = workspace("order");
    let (q, rx) = queue(&ws, true);
    let a = q.submit(spec(&ws, "ok", "draft")).unwrap();
    let b = q.submit(spec(&ws, "ok", "standard")).unwrap();
    let v = wait(&rx, "both done", |v| status(v, &a) == Some(QStatus::Done) && status(v, &b) == Some(QStatus::Done));
    let ja = v.jobs.iter().find(|j| j.id == a).unwrap();
    assert_eq!(ja.tier.as_deref(), Some("low"));
    assert_eq!(ja.params["inference_steps"], 8);
    assert_eq!(ja.params["batch_size"], 1, "low tier caps the draft preset's 3 variants");

    let takes = lib(&ws).list();
    assert_eq!(takes.len(), 2);
    let take = lib(&ws).get(ja.take_id.as_ref().unwrap()).unwrap();
    assert_eq!(take["quality"], "draft");
    assert_eq!(take["spec"]["caption"], "ok");
    assert_eq!(take["variants"][0]["seed"], 7);
    assert_eq!(take["variants"][0]["exists"], true);
    assert_eq!(take["job"], a.as_str());
}

#[test]
fn cancelling_a_queued_job_skips_it() {
    let ws = workspace("cancel-queued");
    let (q, rx) = queue(&ws, true);
    let slow = q.submit(spec(&ws, "slow", "draft")).unwrap();
    let next = q.submit(spec(&ws, "ok", "draft")).unwrap();
    wait(&rx, "slow running", |v| v.current.as_deref() == Some(slow.as_str()));
    assert!(q.cancel(&next));
    assert!(q.cancel(&slow));
    let v = wait(&rx, "slow cancelled", |v| status(v, &slow) == Some(QStatus::Cancelled) && v.current.is_none());
    assert_eq!(status(&v, &next), Some(QStatus::Cancelled));
    assert!(lib(&ws).list().is_empty());
    assert!(!q.cancel(&next), "already finished");
}

#[test]
fn cancelling_the_running_job_moves_its_folder_to_trash() {
    let ws = workspace("cancel-running");
    let (q, rx) = queue(&ws, true);
    let id = q.submit(spec(&ws, "slow", "draft")).unwrap();
    wait(&rx, "diffusing", |v| v.jobs.iter().any(|j| j.id == id && j.phase == "diffusing"));
    let t0 = Instant::now();
    assert!(q.cancel(&id));
    wait(&rx, "cancelled", |v| status(v, &id) == Some(QStatus::Cancelled));
    assert!(t0.elapsed() < Duration::from_secs(10));
    let output: Vec<_> = std::fs::read_dir(ws.join("output")).unwrap().collect();
    assert!(output.is_empty(), "no half take left in output");
    let trashed: Vec<PathBuf> = std::fs::read_dir(ws.join("trash")).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(trashed.len(), 1);
    assert!(trashed[0].join("failed.json").is_file());
}

#[test]
fn a_failed_job_reports_the_error_and_the_next_one_still_runs() {
    let ws = workspace("fail");
    let (q, rx) = queue(&ws, true);
    let bad = q.submit(spec(&ws, "error", "draft")).unwrap();
    let good = q.submit(spec(&ws, "ok", "draft")).unwrap();
    let v = wait(&rx, "good done", |v| status(v, &good) == Some(QStatus::Done));
    let jb = v.jobs.iter().find(|j| j.id == bad).unwrap();
    assert_eq!(jb.status, QStatus::Failed);
    assert_eq!(jb.error.as_ref().unwrap().message, "boom");
    assert_eq!(lib(&ws).list().len(), 1);
    assert!(ws.join("trash").read_dir().unwrap().count() == 1);
}

#[test]
fn setup_problems_fail_the_job_without_a_folder() {
    let ws = workspace("noenv");
    let (q, rx) = queue(&ws, false);
    let id = q.submit(spec(&ws, "ok", "draft")).unwrap();
    let v = wait(&rx, "failed", |v| status(v, &id) == Some(QStatus::Failed));
    assert_eq!(v.jobs[0].error.as_ref().unwrap().code, "ace_dir_unset");
    assert!(!ws.join("output").exists());
}

#[test]
fn rejects_a_spec_without_caption() {
    let ws = workspace("nocap");
    let (q, _rx) = queue(&ws, true);
    assert_eq!(q.submit(serde_json::json!({"style": "lofi"})).unwrap_err().code, "empty_caption");
}
