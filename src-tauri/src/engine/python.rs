//! Finding the Python that can import ACE-Step (port of v3.7 `jobs.find_ace_python`).

use std::path::{Path, PathBuf};

/// The command prefix that runs ACE-Step's Python: its `.venv`, the Windows
/// portable `python_embedded`, or `uv run --project <ace_dir> python`.
pub fn find_ace_python(ace_dir: &Path) -> Option<Vec<String>> {
    find_ace_python_with(ace_dir, find_on_path("uv"))
}

pub fn find_ace_python_with(ace_dir: &Path, uv: Option<PathBuf>) -> Option<Vec<String>> {
    for rel in [".venv/Scripts/python.exe", ".venv/bin/python", "python_embedded/python.exe"] {
        let py = ace_dir.join(rel);
        if py.is_file() {
            return Some(vec![py.to_string_lossy().into_owned()]);
        }
    }
    let uv = uv?;
    Some(vec![
        uv.to_string_lossy().into_owned(),
        "run".into(),
        "--project".into(),
        ace_dir.to_string_lossy().into_owned(),
        "python".into(),
    ])
}

/// `shutil.which` for one program name.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };
    std::env::split_paths(&path)
        .flat_map(|dir| exts.iter().map(move |ext| dir.join(format!("{name}{ext}"))))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nusic-py-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(p: PathBuf) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "").unwrap();
    }

    #[test]
    fn prefers_windows_venv() {
        let d = temp_dir("venv");
        touch(d.join(".venv/Scripts/python.exe"));
        touch(d.join("python_embedded/python.exe"));
        let got = find_ace_python_with(&d, Some("uv".into())).unwrap();
        assert_eq!(got, vec![d.join(".venv/Scripts/python.exe").to_string_lossy().into_owned()]);
    }

    #[test]
    fn uses_embedded_python_when_no_venv() {
        let d = temp_dir("embedded");
        touch(d.join("python_embedded/python.exe"));
        let got = find_ace_python_with(&d, None).unwrap();
        assert_eq!(got, vec![d.join("python_embedded/python.exe").to_string_lossy().into_owned()]);
    }

    #[test]
    fn falls_back_to_uv_run() {
        let d = temp_dir("uv");
        let got = find_ace_python_with(&d, Some(PathBuf::from("/bin/uv"))).unwrap();
        assert_eq!(got[1..4], ["run".to_string(), "--project".into(), d.to_string_lossy().into_owned()]);
        assert_eq!(got.last().unwrap(), "python");
    }

    #[test]
    fn none_without_venv_or_uv() {
        assert_eq!(find_ace_python_with(&temp_dir("none"), None), None);
    }
}
