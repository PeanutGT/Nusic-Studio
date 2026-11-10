//! Engine supervisor against tests/fake_worker.py (same protocol as ace_worker.py, no GPU).

use nusic_studio_lib::engine::{Engine, EngineStatus, GenerateInput, JobStatus, Launch, WorkerState};
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

fn input(caption: &str) -> GenerateInput {
    GenerateInput { caption: caption.into(), duration: 10.0, seed: 42 }
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
    let id = e.generate(&launch(), Path::new("/ace"), &ws, input("ok")).unwrap();

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
    e.generate(&launch(), Path::new("/ace"), &ws, input("ok")).unwrap();
    let first = wait_until(&rx, "first done", |s| job_status(s) == Some(JobStatus::Done) && s.state == WorkerState::Idle);
    e.generate(&launch(), Path::new("/ace"), &ws, input("ok")).unwrap();
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
    e.generate(&launch(), Path::new("/ace"), &ws, input("slow")).unwrap();
    wait_until(&rx, "diffusing", |s| s.job.as_ref().is_some_and(|j| j.phase == "diffusing"));
    let err = e.generate(&launch(), Path::new("/ace"), &ws, input("ok")).unwrap_err();
    assert_eq!(err.code, "busy");
    e.cancel();
}

#[test]
fn cancel_kills_the_worker_and_removes_the_half_take() {
    let (e, rx) = engine();
    let ws = workspace("cancel");
    e.generate(&launch(), Path::new("/ace"), &ws, input("slow")).unwrap();
    let running = wait_until(&rx, "diffusing", |s| s.job.as_ref().is_some_and(|j| j.phase == "diffusing"));
    let out_dir = PathBuf::from(&running.job.unwrap().out_dir);

    let t0 = Instant::now();
    assert!(e.cancel());
    assert!(t0.elapsed() < Duration::from_secs(10), "cancel took {:?}", t0.elapsed());

    let s = e.status();
    assert_eq!(job_status(&s), Some(JobStatus::Cancelled));
    assert_eq!(s.state, WorkerState::Stopped);
    assert_eq!(s.pid, None);
    assert!(!out_dir.exists());
    assert!(!e.cancel(), "nothing left to cancel");
}

#[test]
fn starts_a_fresh_worker_after_cancel() {
    let (e, rx) = engine();
    let ws = workspace("after-cancel");
    e.generate(&launch(), Path::new("/ace"), &ws, input("slow")).unwrap();
    wait_until(&rx, "diffusing", |s| s.job.as_ref().is_some_and(|j| j.phase == "diffusing"));
    e.cancel();
    e.generate(&launch(), Path::new("/ace"), &ws, input("ok")).unwrap();
    wait_until(&rx, "done after restart", |s| job_status(s) == Some(JobStatus::Done));
}

#[test]
fn worker_crash_fails_the_job_and_next_job_restarts_it() {
    let (e, rx) = engine();
    let ws = workspace("crash");
    e.generate(&launch(), Path::new("/ace"), &ws, input("crash")).unwrap();
    let s = wait_until(&rx, "crashed", |s| s.state == WorkerState::Crashed && job_status(s) == Some(JobStatus::Failed));
    let job = s.job.unwrap();
    assert_eq!(job.error.as_ref().unwrap().code, "engine_crashed");
    assert!(job.error.unwrap().message.contains('3'), "exit code is reported");
    assert!(!Path::new(&job.out_dir).exists());

    e.generate(&launch(), Path::new("/ace"), &ws, input("ok")).unwrap();
    wait_until(&rx, "done after crash", |s| job_status(s) == Some(JobStatus::Done));
}

#[test]
fn worker_error_event_fails_the_job_but_keeps_the_worker() {
    let (e, rx) = engine();
    let ws = workspace("error");
    e.generate(&launch(), Path::new("/ace"), &ws, input("error")).unwrap();
    let s = wait_until(&rx, "failed and idle", |s| job_status(s) == Some(JobStatus::Failed) && s.state == WorkerState::Idle);
    let err = s.job.unwrap().error.unwrap();
    assert_eq!((err.code.as_str(), err.message.as_str(), err.hint.as_str()), ("generate", "boom", "try again"));
}

#[test]
fn missing_python_is_a_clear_error() {
    let (e, _rx) = engine();
    let bad = Launch { program: "definitely-not-a-python-xyz".into(), args: vec![], cwd: std::env::temp_dir() };
    let err = e.generate(&bad, Path::new("/ace"), &workspace("nopy"), input("ok")).unwrap_err();
    assert_eq!(err.code, "spawn_failed");
    assert_eq!(e.status().state, WorkerState::Stopped);
}

#[test]
fn keeps_worker_stderr_for_troubleshooting() {
    let (e, rx) = engine();
    e.generate(&launch(), Path::new("/ace"), &workspace("log"), input("ok")).unwrap();
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
    e.generate(&real, &missing, &workspace("real"), input("calm piano")).unwrap();
    let s = wait_until(&rx, "failed and idle", |s| job_status(s) == Some(JobStatus::Failed) && s.state == WorkerState::Idle);
    assert_eq!(s.job.unwrap().error.unwrap().code, "ace_dir");
    assert!(s.pid.is_some(), "worker keeps running for the next job");
}
