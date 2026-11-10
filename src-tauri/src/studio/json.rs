//! Small helpers for working with JSON the way v3.7's Python does (dicts, truthiness, str()).

use serde_json::{Map, Value};
use std::path::Path;

pub type Obj = Map<String, Value>;

/// Python truthiness: null, false, 0, "", [] and {} are false.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

pub fn opt_truthy(v: Option<&Value>) -> bool {
    v.is_some_and(truthy)
}

/// Python `str()` of a JSON value: `400`, `7.0`, `None`, `True`, plain text.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) if n.is_f64() => py_float(n.as_f64().unwrap_or(0.0)),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Python `repr()` for the values that appear in v3.7 messages.
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{s}'"),
        other => py_str(other),
    }
}

pub fn py_float(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e16 { format!("{f:.1}") } else { format!("{f}") }
}

pub fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
}

/// `str` items of a JSON array (others skipped); empty for anything that is not an array.
pub fn strs(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

pub fn get_dotted<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.').try_fold(v, |cur, part| cur.as_object()?.get(part))
}

pub fn set_dotted(v: &mut Value, key: &str, value: Value) {
    let parts: Vec<&str> = key.split('.').collect();
    let mut cur = v;
    for part in &parts[..parts.len() - 1] {
        if !cur.is_object() {
            *cur = Value::Object(Obj::new());
        }
        let obj = cur.as_object_mut().expect("object");
        let next = obj.entry(part.to_string()).or_insert_with(|| Value::Object(Obj::new()));
        if !next.is_object() {
            *next = Value::Object(Obj::new());
        }
        cur = next;
    }
    if !cur.is_object() {
        *cur = Value::Object(Obj::new());
    }
    cur.as_object_mut().expect("object").insert(parts[parts.len() - 1].to_string(), value);
}

/// Remove a dotted key. `true` when something was removed.
pub fn unset_dotted(v: &mut Value, key: &str) -> bool {
    let parts: Vec<&str> = key.split('.').collect();
    let mut cur = v;
    for part in &parts[..parts.len() - 1] {
        match cur.as_object_mut().and_then(|o| o.get_mut(*part)) {
            Some(next) if next.is_object() => cur = next,
            _ => return false,
        }
    }
    cur.as_object_mut().and_then(|o| o.shift_remove(parts[parts.len() - 1])).is_some_and(|v| !v.is_null())
}

/// Missing file = `None`. Broken JSON = `Err` with the parser message.
pub fn read_json(path: &Path) -> Result<Option<Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(text.trim_start_matches('\u{feff}')).map(Some).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Write pretty JSON atomically (temp file + rename), creating the folder.
pub fn write_json(path: &Path, value: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text + "\n")?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn python_str_of_numbers() {
        assert_eq!(py_str(&json!(400)), "400");
        assert_eq!(py_str(&json!(7.0)), "7.0");
        assert_eq!(py_str(&json!(0.25)), "0.25");
        assert_eq!(py_str(&Value::Null), "None");
        assert_eq!(py_repr(&json!("x")), "'x'");
    }

    #[test]
    fn truthiness_matches_python() {
        for v in [json!(null), json!(false), json!(0), json!(0.0), json!(""), json!([]), json!({})] {
            assert!(!truthy(&v), "{v}");
        }
        for v in [json!(true), json!(-1), json!("a"), json!([0]), json!({"a": 1})] {
            assert!(truthy(&v), "{v}");
        }
    }

    #[test]
    fn dotted_keys_round_trip() {
        let mut v = json!({});
        set_dotted(&mut v, "quality.presets.draft.inference_steps", json!(12));
        assert_eq!(get_dotted(&v, "quality.presets.draft.inference_steps"), Some(&json!(12)));
        assert!(unset_dotted(&mut v, "quality.presets.draft.inference_steps"));
        assert!(!unset_dotted(&mut v, "quality.presets.draft.inference_steps"));
        assert!(!unset_dotted(&mut v, "nope.deeper"));
    }
}
