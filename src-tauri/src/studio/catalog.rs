//! Style, mood, workflow and lexicon libraries: built-in defaults layered with the workspace's own
//! files (port of v3.7 `msconfig.load_*`). Entries keep their JSON shape; `_source` says where
//! each came from (`builtin`, `builtin+user`, `user`).

use super::config::merge;
use super::json::{Obj, read_json, truthy, write_json};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// One loaded library: id -> entry, in load order.
pub type Collection = Obj;

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub styles: Collection,
    pub moods: Collection,
    pub workflows: Collection,
    pub lexicon: Collection,
    /// Files that could not be read (broken JSON, not an object).
    pub problems: Vec<String>,
}

impl Catalog {
    pub fn load(defaults_dir: &Path, ws: &Path) -> Self {
        let mut problems = Vec::new();
        let mut dir = |p: PathBuf| load_dir(&p, &mut problems);
        let styles = layer(dir(defaults_dir.join("styles")), dir(ws.join("styles")));
        let workflows = layer(dir(defaults_dir.join("workflows")), dir(ws.join("workflows")));
        let mut file = |p: PathBuf| load_file(&p, &mut problems);
        let moods = layer(file(defaults_dir.join("moods.json")), file(ws.join("moods.json")));
        let lexicon = layer(file(defaults_dir.join("lexicon.json")), file(ws.join("lexicon.json")));
        Self { styles, moods, workflows, lexicon, problems }
    }
}

fn load_dir(path: &Path, problems: &mut Vec<String>) -> Collection {
    let mut files: Vec<PathBuf> = std::fs::read_dir(path)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    files.retain(|f| {
        f.extension().is_some_and(|e| e == "json") && !f.file_name().is_some_and(|n| n.to_string_lossy().starts_with('_'))
    });
    files.sort();
    let mut out = Collection::new();
    for f in files {
        match read_json(&f) {
            Ok(Some(Value::Object(obj))) => {
                let id = obj.get("id").filter(|v| truthy(v)).map(super::json::py_str);
                let id = id.unwrap_or_else(|| f.file_stem().unwrap_or_default().to_string_lossy().into_owned());
                out.insert(id, Value::Object(obj));
            }
            Ok(_) => problems.push(format!("{}: expected a JSON object", f.display())),
            Err(e) => problems.push(format!("{}: invalid JSON ({e})", f.display())),
        }
    }
    out
}

fn load_file(path: &Path, problems: &mut Vec<String>) -> Collection {
    match read_json(path) {
        Ok(Some(Value::Object(obj))) => obj.into_iter().filter(|(k, _)| !k.starts_with('_')).collect(),
        Ok(None) => Collection::new(),
        Ok(Some(_)) => {
            problems.push(format!("{}: expected a JSON object", path.display()));
            Collection::new()
        }
        Err(e) => {
            problems.push(format!("{}: invalid JSON ({e})", path.display()));
            Collection::new()
        }
    }
}

fn with_id(mut v: Value, id: &str, source: &str) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.insert("id".into(), json!(id));
        o.insert("_source".into(), json!(source));
    }
    v
}

fn layer(base: Collection, user: Collection) -> Collection {
    let mut out = Collection::new();
    for (k, v) in &base {
        out.insert(k.clone(), with_id(v.clone(), k, "builtin"));
    }
    for (k, u) in user {
        let replace = u.get("$replace").is_some_and(truthy);
        if let (Some(b), false) = (base.get(&k), replace) {
            let merged = merge(b, &u, "user", &mut None, "");
            out.insert(k.clone(), with_id(merged, &k, "builtin+user"));
        } else {
            let mut u = u;
            if let Some(o) = u.as_object_mut() {
                o.shift_remove("$replace");
            }
            out.insert(k.clone(), with_id(u, &k, "user"));
        }
    }
    out
}

/// `kind` is `styles` or `workflows`.
pub fn save_user_item(ws: &Path, kind: &str, obj: &Value) -> Result<PathBuf, String> {
    let raw = obj.get("id").map(super::json::py_str).unwrap_or_default();
    let mut id = String::new();
    for c in raw.chars() {
        if c.is_alphanumeric() || c == '_' || c == '-' {
            id.push(c);
        } else if !id.ends_with('-') {
            id.push('-');
        }
    }
    let id = id.trim_matches('-').to_string();
    if id.is_empty() {
        return Err("item needs an 'id'".into());
    }
    let mut clean: Obj =
        obj.as_object().into_iter().flatten().filter(|(k, _)| !k.starts_with('_') || *k == "_doc").map(|(k, v)| (k.clone(), v.clone())).collect();
    clean.insert("id".into(), json!(id));
    let path = ws.join(kind).join(format!("{id}.json"));
    write_json(&path, &Value::Object(clean)).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Move a user style / workflow to `<workspace>/trash`. `false` when there was none.
pub fn delete_user_item(ws: &Path, kind: &str, id: &str) -> std::io::Result<bool> {
    let path = ws.join(kind).join(format!("{id}.json"));
    if !path.is_file() {
        return Ok(false);
    }
    let trash = ws.join("trash");
    std::fs::create_dir_all(&trash)?;
    std::fs::rename(&path, trash.join(format!("{kind}-{id}.json")))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_files_layer_over_builtins() {
        let root = std::env::temp_dir().join(format!("nusic-catalog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (d, ws) = (root.join("defaults"), root.join("ws"));
        write_json(&d.join("styles/a.json"), &json!({"name": "A", "caption": "x", "bpm": 90})).unwrap();
        write_json(&ws.join("styles/a.json"), &json!({"bpm": 100})).unwrap();
        write_json(&ws.join("styles/b.json"), &json!({"id": "bee", "name": "B"})).unwrap();
        std::fs::write(ws.join("styles/broken.json"), "{").unwrap();
        write_json(&d.join("moods.json"), &json!({"_doc": "d", "calm": {"caption": "calm"}})).unwrap();
        let c = Catalog::load(&d, &ws);
        assert_eq!(c.styles["a"]["bpm"], 100);
        assert_eq!(c.styles["a"]["name"], "A");
        assert_eq!(c.styles["a"]["_source"], "builtin+user");
        assert_eq!(c.styles["bee"]["_source"], "user");
        assert_eq!(c.moods.keys().collect::<Vec<_>>(), vec!["calm"]);
        assert_eq!(c.problems.len(), 1);
    }
}
