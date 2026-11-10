pub mod studio;
pub mod engine;
pub mod settings;

use engine::{Engine, EngineError, EngineStatus, GenerateInput, Launch, gpu};
use settings::Settings;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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
    settings: Mutex<Settings>,
    settings_path: PathBuf,
    worker_script: PathBuf,
}

impl AppState {
    fn settings(&self) -> Settings {
        self.settings.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Check the ACE-Step folder and build the worker command line.
    fn launch(&self, s: &Settings) -> Result<(Launch, PathBuf), EngineError> {
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
        cmd.push(self.worker_script.to_string_lossy().into_owned());
        cmd.push("--serve".into());
        Ok((Launch { program, args: cmd, cwd: ace_dir.clone() }, ace_dir))
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

#[tauri::command]
async fn generate(state: State<'_, AppState>, input: GenerateInput) -> Result<String, EngineError> {
    let s = state.settings();
    let (launch, ace_dir) = state.launch(&s)?;
    state.engine.generate(&launch, &ace_dir, Path::new(&s.workspace_dir), input)
}

#[tauri::command]
async fn cancel(state: State<'_, AppState>) -> Result<bool, ()> {
    Ok(state.engine.cancel())
}

#[tauri::command]
async fn restart_engine(state: State<'_, AppState>) -> Result<(), EngineError> {
    let (launch, _) = state.launch(&state.settings())?;
    state.engine.restart(&launch)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let settings_path = app.path().app_config_dir()?.join("settings.json");
            let worker_script = app.path().resource_dir()?.join("resources").join("ace_worker.py");
            let handle = app.handle().clone();
            let engine = Engine::new(move |status: EngineStatus| {
                let _ = handle.emit("engine-status", status);
            });
            app.manage(AppState {
                engine,
                settings: Mutex::new(settings::load(&settings_path)),
                settings_path,
                worker_script,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            get_settings,
            save_settings,
            engine_status,
            engine_log,
            gpu_info,
            generate,
            cancel,
            restart_engine
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
