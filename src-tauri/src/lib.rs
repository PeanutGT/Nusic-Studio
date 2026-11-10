pub mod studio;
pub mod engine;
pub mod settings;

use engine::{Engine, EngineError, EngineStatus, Launch, gpu};
use serde_json::Value;
use settings::Settings;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use studio::api::Studio;
use studio::plan::PlanInput;
use studio::queue::{Queue, QueueView, RunEnv};
use studio::takes::Library;
use tauri::{Emitter, Manager, State};

/// Version and build info shown in the window, so a tester can tell which build they run.
#[derive(serde::Serialize)]
pub struct AppInfo {
    pub version: &'static str,
    pub target: &'static str,
}

pub fn app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        target: std::env::consts::OS,
    }
}

struct AppState {
    engine: Engine,
    queue: Queue,
    studio: Arc<Studio>,
    settings: Arc<Mutex<Settings>>,
    settings_path: PathBuf,
    worker_script: PathBuf,
}

fn lock_settings(s: &Mutex<Settings>) -> Settings {
    s.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Check the ACE-Step folder and build the worker command line.
fn launch_for(s: &Settings, worker_script: &Path) -> Result<(Launch, PathBuf), EngineError> {
    if s.ace_step_dir.trim().is_empty() {
        return Err(EngineError::new("ace_dir_unset", "ACE-Step folder is not set", ""));
    }
    let ace_dir = PathBuf::from(s.ace_step_dir.trim());
    if !ace_dir.join("acestep").is_dir() {
        return Err(EngineError::new("ace_dir_invalid", format!("not an ACE-Step folder: {}", ace_dir.display()), ""));
    }
    let mut cmd = engine::python::find_ace_python(&ace_dir)
        .ok_or_else(|| EngineError::new("no_python", "cannot find ACE-Step's Python (.venv, python_embedded or uv)", ""))?;
    let program = cmd.remove(0);
    cmd.push(worker_script.to_string_lossy().into_owned());
    cmd.push("--serve".into());
    Ok((Launch { program, args: cmd, cwd: ace_dir.clone() }, ace_dir))
}

/// The hardware tier for this config: forced in `hardware.tier`, else from the card's VRAM.
fn current_tier(cfg: &Value) -> String {
    studio::config::resolve_tier(cfg, gpu::query().map(|g| g.total_gb)).0
}

fn err(e: String) -> EngineError {
    EngineError::new("studio", e, "")
}

impl AppState {
    fn settings(&self) -> Settings {
        lock_settings(&self.settings)
    }

    fn ws(&self) -> PathBuf {
        PathBuf::from(self.settings().workspace_dir)
    }

    fn tier(&self) -> Result<String, EngineError> {
        let (cfg, _) = self.studio.config(&self.ws()).map_err(err)?;
        Ok(current_tier(&cfg))
    }

    fn library(&self) -> Result<Library, EngineError> {
        let ws = self.ws();
        let (cfg, _) = self.studio.config(&ws).map_err(err)?;
        Ok(Library { out_root: studio::pipeline::output_root(&cfg, &ws), ws })
    }
}

#[tauri::command]
fn get_app_info() -> AppInfo {
    app_info()
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    state.settings()
}

#[tauri::command]
fn save_settings(state: State<AppState>, settings: Settings) -> Result<(), EngineError> {
    settings::save(&state.settings_path, &settings)
        .map_err(|e| EngineError::new("settings_save", format!("cannot save settings: {e}"), ""))?;
    *state.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings;
    Ok(())
}

#[tauri::command]
fn engine_status(state: State<AppState>) -> EngineStatus {
    state.engine.status()
}

#[tauri::command]
fn engine_log(state: State<AppState>) -> Vec<String> {
    state.engine.log_tail()
}

#[tauri::command]
fn gpu_info() -> Option<gpu::GpuInfo> {
    gpu::query()
}

/// Styles, moods, workflows, instruments, quality presets, tier and config problems.
#[tauri::command]
async fn studio_overview(state: State<'_, AppState>) -> Result<Value, EngineError> {
    let tier = state.tier()?;
    state.studio.overview(&state.ws(), &tier).map_err(err)
}

/// Preview a request: spec, caption, warnings and the engine parameters it would use.
#[tauri::command]
async fn plan_track(state: State<'_, AppState>, input: PlanInput) -> Result<Value, EngineError> {
    let tier = state.tier()?;
    state.studio.plan(&state.ws(), &input, &tier).map_err(err)
}

/// Plan the request and put it in the queue. Returns the job id.
#[tauri::command]
async fn enqueue(state: State<'_, AppState>, input: PlanInput) -> Result<String, EngineError> {
    let spec = state.studio.spec(&state.ws(), &input).map_err(err)?;
    state.queue.submit(spec)
}

#[tauri::command]
fn queue_status(state: State<AppState>) -> QueueView {
    state.queue.view()
}

#[tauri::command]
async fn cancel_job(state: State<'_, AppState>, id: String) -> Result<bool, ()> {
    Ok(state.queue.cancel(&id))
}

#[tauri::command]
async fn restart_engine(state: State<'_, AppState>) -> Result<(), EngineError> {
    let (launch, _) = launch_for(&state.settings(), &state.worker_script)?;
    state.engine.restart(&launch)
}

#[tauri::command]
async fn list_takes(state: State<'_, AppState>) -> Result<Vec<Value>, EngineError> {
    Ok(state.library()?.list())
}

#[tauri::command]
async fn get_take(state: State<'_, AppState>, id: String) -> Result<Option<Value>, EngineError> {
    Ok(state.library()?.get(&id))
}

#[tauri::command]
async fn annotate_take(state: State<'_, AppState>, id: String, fields: serde_json::Map<String, Value>) -> Result<Value, EngineError> {
    state.library()?.annotate(&id, &fields).map_err(err)
}

#[tauri::command]
async fn trash_take(state: State<'_, AppState>, id: String) -> Result<bool, EngineError> {
    state.library()?.trash(&id).map_err(|e| err(e.to_string()))
}

/// Every tunable generation setting for one quality preset: value, source layer, range, help.
#[tauri::command]
async fn knob_table(state: State<'_, AppState>, quality: Option<String>) -> Result<Value, EngineError> {
    let tier = state.tier()?;
    state.studio.knob_table(&state.ws(), quality.as_deref(), &tier).map_err(err)
}

/// Save a workspace override (dotted key). Returns the config problems after the change.
#[tauri::command]
async fn set_setting(state: State<'_, AppState>, key: String, value: Value) -> Result<Vec<String>, EngineError> {
    state.studio.set_config(&state.ws(), &key, Some(value)).map_err(err)
}

/// Remove a workspace override, falling back to the built-in value.
#[tauri::command]
async fn unset_setting(state: State<'_, AppState>, key: String) -> Result<Vec<String>, EngineError> {
    state.studio.set_config(&state.ws(), &key, None).map_err(err)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let settings_path = app.path().app_config_dir()?.join("settings.json");
            let resources = app.path().resource_dir()?.join("resources");
            let worker_script = resources.join("ace_worker.py");
            let studio = Arc::new(Studio { defaults_dir: resources.join("defaults") });
            let settings = Arc::new(Mutex::new(settings::load(&settings_path)));
            let handle = app.handle().clone();
            let engine = Engine::new(move |status: EngineStatus| {
                let _ = handle.emit("engine-status", status);
            });
            let env = {
                let (settings, studio, script) = (settings.clone(), studio.clone(), worker_script.clone());
                Box::new(move || {
                    let s = lock_settings(&settings);
                    let (launch, ace_dir) = launch_for(&s, &script)?;
                    let ws = PathBuf::from(&s.workspace_dir);
                    let (cfg, _) = studio.config(&ws).map_err(err)?;
                    let tier = current_tier(&cfg);
                    Ok(RunEnv { launch, ace_dir, ws, cfg, tier })
                })
            };
            let handle = app.handle().clone();
            let queue = Queue::new(engine.clone(), env, move |view: QueueView| {
                let _ = handle.emit("queue-status", view);
            });
            app.manage(AppState { engine, queue, studio, settings, settings_path, worker_script });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            get_settings,
            save_settings,
            engine_status,
            engine_log,
            gpu_info,
            studio_overview,
            plan_track,
            enqueue,
            queue_status,
            cancel_job,
            restart_engine,
            list_takes,
            get_take,
            annotate_take,
            trash_take,
            knob_table,
            set_setting,
            unset_setting
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // Free the GPU: never leave a Python worker behind after the window closes.
                app.state::<AppState>().engine.shutdown();
            }
        });
}

#[cfg(test)]
mod tests {
    #[test]
    fn app_info_reports_crate_version() {
        assert_eq!(super::app_info().version, env!("CARGO_PKG_VERSION"));
    }
}
