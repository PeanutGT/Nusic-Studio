//! Engine supervisor against tests/fake_worker.py (same protocol as ace_worker.py, no GPU).

use nusic_studio_lib::engine::{Engine, EngineStatus, JobStatus, Launch, Submit, WorkerState};
use serde_json::json;
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

fn launch() -> Launch {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fake_worker.py");
    Launch { program: python(), args: vec![script.to_string_lossy().into_owned()], cwd: std::env::temp_dir() }
}

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nusic-engine-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// A job as `studio::pipeline::prepare` would send it (only the fields the workers read).
fn submit(ws: &Path, ace_dir: &Path, caption: &str) -> Submit {
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let out_dir = ws.join("output").join(format!("take-{n}"));
    std::fs::create_dir_all(&out_dir).unwrap();
    let request = json!({
        "op": "generate", "ace_step_dir": ace_dir.to_string_lossy(), "checkpoints_dir": ace_dir.join("checkpoints").to_string_lossy(),
        "params": {"dit_model": "acestep-v15-turbo", "lm_model": null, "thinking": false, "inference_steps": 8,
                   "guidance_scale": 7.0, "shift": 3.0, "duration": 10, "batch_size": 1, "quantization": null,
                   "offload_to_cpu": true, "offload_dit_to_cpu": true, "audio_format": "flac", "instrumental": true},
        "tier_dit_model": "acestep-v15-turbo", "explicit_keys": [], "vram_guard": true, "caption": caption,
        "instrumental": true, "lyrics": "", "seeds": [42], "out_dir": out_dir.to_string_lossy(), "warnings": []
    });
    Submit { id: format!("j{n}"), caption: caption.into(), out_dir, request }
}

/// Engine whose status updates arrive on a channel.
fn engine() -> (Engine, mpsc::Receiver<EngineStatus>) {
    let (tx, rx) = mpsc::channel();
    let e = Engine::new(move |s: EngineStatus| {
        let _ = tx.send(s);
    });
    (e, rx)
}

fn wait_until(rx: &mpsc::Receiver<EngineStatus>, what: &str, pred: impl Fn(&EngineStatus) -> bool) -> EngineStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(s) if pred(&s) => return s,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    panic!("timed out waiting for: {what}");
}

fn job_status(s: &EngineStatus) -> Option<JobStatus> {
    s.job.as_ref().map(|j| j.status)
}

#[test]
fn generates_one_track_and_reports_files_and_vram() {
    let (e, rx) = engine();
    let ws = workspace("ok");
    let id = e.generate(&launch(), submit(&ws, Path::new("/ace"), "ok")).unwrap();

    let s = wait_until(&rx, "job done and worker idle", |s| {
        job_status(s) == Some(JobStatus::Done) && s.state == WorkerState::Idle
    });
    let job = s.job.unwrap();
    assert_eq!(job.id, id);
    assert_eq!(job.variants.len(), 1);
    assert_eq!(job.variants[0].seed, 42);
    assert_eq!(job.warnings, vec!["fake warning".to_string()]);
    assert!(Path::new(&job.out_dir).join("v1.flac").is_file());
    assert!(job.out_dir.starts_with(&ws.join("output").to_string_lossy().into_owned()));
    assert_eq!(s.vram_total_gb, Some(4.0));
    assert_eq!(s.vram_free_gb, Some(3.2));
    assert!(s.models_loaded);
}

#[test]
fn reuses_the_same_worker_for_the_next_job() {
    let (e, rx) = engine();
    let ws = workspace("reuse");
    e.generate(&launch(), submit(&ws, Path::new("/ace"), "ok")).unwrap();
    let first = wait_until(&rx, "first done", |s| job_status(s) == Some(JobStatus::Done) && s.state == WorkerState::Idle);
    e.generate(&launch(), submit(&ws, Path::new("/ace"), "ok")).unwrap();
    let second = wait_until(&rx, "second done", |s| {
        job_status(s) == Some(JobStatus::Done) && s.state == WorkerState::Idle && s.job.as_ref().unwrap().id != first.job.as_ref().unwrap().id
    });
    assert_eq!(first.pid, second.pid);
    assert!(first.pid.is_some());
}

#[test]
fn rejects_a_second_job_while_busy() {
    let (e, rx) = engine();
    let ws = workspace("busy");
    e.generate(&launch(), submit(&ws, Path::new("/ace"), "slow")).unwrap();
    wait_until(&rx, "diffusing", |s| s.job.as_ref().is_some_and(|j| j.phase == "diffusing"));
    let err = e.generate(&launch(), submit(&ws, Path::new("/ace"), "ok")).unwrap_err();
    assert_eq!(err.code, "busy");
    e.cancel();
}

#[test]
fn cancel_kills_the_worker_quickly() {
    let (e, rx) = engine();
    let ws = workspace("cancel");
    e.generate(&launch(), submit(&ws, Path::new("/ace"), "slow")).unwrap();
    let running = wait_until(&rx, "diffusing", |s| s.job.as_ref().is_some_and(|j| j.phase == "diffusing"));
    assert!(running.job.is_some());

    let t0 = Instant::now();
    assert!(e.cancel());
    assert!(t0.elapsed() < Duration::from_secs(10), "cancel took {:?}", t0.elapsed());

    let s = e.status();
    assert_eq!(job_status(&s), Some(JobStatus::Cancelled));
    assert_eq!(s.state, WorkerState::Stopped);
    assert_eq!(s.pid, None);
    assert!(!e.cancel(), "nothing left to cancel");
}

#[test]
fn starts_a_fresh_worker_after_cancel() {
    let (e, rx) = engine();
    let ws = workspace("after-cancel");
    e.generate(&launch(), submit(&ws, Path::new("/ace"), "slow")).unwrap();
    wait_until(&rx, "diffusing", |s| s.job.as_ref().is_some_and(|j| j.phase == "diffusing"));
    e.cancel();
    e.generate(&launch(), submit(&ws, Path::new("/ace"), "ok")).unwrap();
    wait_until(&rx, "done after restart", |s| job_status(s) == Some(JobStatus::Done));
}

#[test]
fn worker_crash_fails_the_job_and_next_job_restarts_it() {
    let (e, rx) = engine();
    let ws = workspace("crash");
    e.generate(&launch(), submit(&ws, Path::new("/ace"), "crash")).unwrap();
    let s = wait_until(&rx, "crashed", |s| s.state == WorkerState::Crashed && job_status(s) == Some(JobStatus::Failed));
    let job = s.job.unwrap();
    assert_eq!(job.error.as_ref().unwrap().code, "engine_crashed");
    assert!(job.error.unwrap().message.contains('3'), "exit code is reported");

    e.generate(&launch(), submit(&ws, Path::new("/ace"), "ok")).unwrap();
    wait_until(&rx, "done after crash", |s| job_status(s) == Some(JobStatus::Done));
}

#[test]
fn worker_error_event_fails_the_job_but_keeps_the_worker() {
    let (e, rx) = engine();
    let ws = workspace("error");
    e.generate(&launch(), submit(&ws, Path::new("/ace"), "error")).unwrap();
    let s = wait_until(&rx, "failed and idle", |s| job_status(s) == Some(JobStatus::Failed) && s.state == WorkerState::Idle);
    let err = s.job.unwrap().error.unwrap();
    assert_eq!((err.code.as_str(), err.message.as_str(), err.hint.as_str()), ("generate", "boom", "try again"));
}

#[test]
fn missing_python_is_a_clear_error() {
    let (e, _rx) = engine();
    let bad = Launch { program: "definitely-not-a-python-xyz".into(), args: vec![], cwd: std::env::temp_dir() };
    let err = e.generate(&bad, submit(&workspace("nopy"), Path::new("/ace"), "ok")).unwrap_err();
    assert_eq!(err.code, "spawn_failed");
    assert_eq!(e.status().state, WorkerState::Stopped);
}

#[test]
fn keeps_worker_stderr_for_troubleshooting() {
    let (e, rx) = engine();
    e.generate(&launch(), submit(&workspace("log"), Path::new("/ace"), "ok")).unwrap();
    wait_until(&rx, "done", |s| job_status(s) == Some(JobStatus::Done));
    assert!(e.log_tail().iter().any(|l| l.contains("fake worker starting")));
}

/// The real bundled worker, without ACE-Step installed: it must speak the same protocol
/// and turn a bad ACE-Step folder into an error event, not a crash.
#[test]
fn real_worker_reports_missing_ace_step_as_job_error() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources").join("ace_worker.py");
    let real = Launch {
        program: python(),
        args: vec![script.to_string_lossy().into_owned(), "--serve".into()],
        cwd: std::env::temp_dir(),
    };
    let (e, rx) = engine();
    let missing = std::env::temp_dir().join("nusic-no-such-ace-step");
    e.generate(&real, submit(&workspace("real"), &missing, "calm piano")).unwrap();
    let s = wait_until(&rx, "failed and idle", |s| job_status(s) == Some(JobStatus::Failed) && s.state == WorkerState::Idle);
    assert_eq!(s.job.unwrap().error.unwrap().code, "ace_dir");
    assert!(s.pid.is_some(), "worker keeps running for the next job");
}
