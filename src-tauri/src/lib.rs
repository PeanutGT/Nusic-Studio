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

#[tauri::command]
fn get_app_info() -> AppInfo {
    app_info()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![get_app_info])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    #[test]
    fn app_info_reports_crate_version() {
        assert_eq!(super::app_info().version, env!("CARGO_PKG_VERSION"));
    }
}
