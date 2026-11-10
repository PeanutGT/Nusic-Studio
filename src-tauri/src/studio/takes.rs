//! Take library (port of v3.7 `library.py`). Each take's own folder (`meta.json`, audio) is the
//! source of truth; the person's annotations (rating, favourite, notes, tags, picked variant, title)
//! live in `<workspace>/library.json`, so rescanning never loses them and deleting that file
//! never deletes audio.

use super::json::{Obj, read_json, write_json};
use super::pipeline::move_to_trash;
use serde_json::{Value, json};
use std::path::PathBuf;

const ANN_FIELDS: [&str; 6] = ["title", "rating", "favorite", "notes", "tags", "picked"];

pub struct Library {
    pub ws: PathBuf,
    pub out_root: PathBuf,
}

impl Library {
    fn annotations(&self) -> Obj {
        let data = read_json(&self.ws.join("library.json")).ok().flatten().unwrap_or(Value::Null);
        data.get("annotations").and_then(Value::as_object).cloned().unwrap_or_default()
    }

    fn folder(&self, id: &str) -> Option<PathBuf> {
        // ids are folder names: refuse anything that could climb out of the output folder
        let ok = !id.is_empty() && !id.contains(['/', '\\']) && id != "." && id != "..";
        ok.then(|| self.out_root.join(id))
    }

    pub fn get(&self, id: &str) -> Option<Value> {
        let folder = self.folder(id)?;
        let mut meta = read_json(&folder.join("meta.json")).ok().flatten()?;
        let variants: Vec<Value> = meta["variants"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| {
                let f = folder.join(v["file"].as_str().unwrap_or(""));
                let bytes = std::fs::metadata(&f).ok().filter(|m| m.is_file()).map(|m| m.len());
                let mut v = v.clone();
                v["exists"] = json!(bytes.is_some());
                v["bytes"] = json!(bytes.unwrap_or(0));
                v
            })
            .collect();
        let finals = read_json(&folder.join("final.json")).ok().flatten().unwrap_or(json!([]));
        let ann = self.annotations().get(id).cloned().unwrap_or(json!({}));
        let title = ann.get("title").filter(|t| t.as_str().is_some_and(|s| !s.is_empty())).cloned().unwrap_or(meta["title"].clone());
        let m = meta.as_object_mut()?;
        m.insert("dir".into(), json!(folder.to_string_lossy()));
        m.insert("variants".into(), json!(variants));
        m.insert("finals".into(), finals);
        m.insert("annotations".into(), ann);
        m.insert("title".into(), title);
        Some(meta)
    }

    /// Newest first.
    pub fn list(&self) -> Vec<Value> {
        let Ok(rd) = std::fs::read_dir(&self.out_root) else { return vec![] };
        let mut out: Vec<Value> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("meta.json").is_file())
            .filter_map(|e| self.get(&e.file_name().to_string_lossy()))
            .map(|t| {
                let spec = &t["spec"];
                let ann = &t["annotations"];
                json!({
                    "id": t["id"], "title": t["title"], "created": t["created"],
                    "style": spec["style"], "moods": spec.get("moods").cloned().unwrap_or(json!([])),
                    "duration": spec["duration"], "bpm": spec["bpm"], "key": spec["key"],
                    "engine": t["engine"], "quality": t["quality"],
                    "variants": t["variants"].as_array().map_or(0, Vec::len),
                    "finals": t["finals"].as_array().map_or(0, Vec::len),
                    "rating": ann.get("rating").cloned().unwrap_or(json!(0)),
                    "favorite": ann.get("favorite").and_then(Value::as_bool).unwrap_or(false),
                    "tags": ann.get("tags").cloned().unwrap_or(json!([])),
                    "picked": ann.get("picked").cloned().unwrap_or(Value::Null),
                    "dir": t["dir"],
                    "files": t["variants"].as_array().into_iter().flatten().map(|v| v["file"].clone()).collect::<Vec<_>>(),
                    // automatic check per variant, and the finished exports (A3)
                    "checks": t["variants"].as_array().into_iter().flatten()
                        .map(|v| json!({"index": v["index"], "file": v["file"], "qc": v.get("qc").cloned().unwrap_or(Value::Null)}))
                        .collect::<Vec<_>>(),
                    "exports": t["finals"].as_array().into_iter().flatten()
                        .map(|f| json!({"variant": f["variant"], "base": f["base"], "created": f["created"], "files": f["files"],
                                        "lufs": f["report"]["lufs"], "true_peak_db": f["report"]["true_peak_db"],
                                        "qc": f["report"]["qc"], "issues": f["report"]["issues"], "warnings": f["warnings"]}))
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        out.sort_by(|a, b| b["created"].as_str().unwrap_or("").cmp(a["created"].as_str().unwrap_or("")));
        out
    }

    pub fn annotate(&self, id: &str, fields: &Obj) -> Result<Value, String> {
        if self.get(id).is_none() {
            return Err(format!("unknown take '{id}'"));
        }
        let mut clean = Obj::new();
        for (k, v) in fields {
            let v = match k.as_str() {
                "rating" => json!(as_int(v)?.clamp(0, 5)),
                "favorite" => json!(match v {
                    Value::Bool(b) => *b,
                    Value::String(s) => !s.is_empty(),
                    Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
                    _ => false,
                }),
                "picked" => json!(as_int(v)?),
                "tags" => {
                    let items: Vec<String> = match v {
                        Value::Array(a) => a.iter().map(super::json::py_str).collect(),
                        other => super::json::py_str(other).split(',').map(str::to_string).collect(),
                    };
                    json!(items.iter().map(|t| t.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>())
                }
                k if ANN_FIELDS.contains(&k) => json!(super::json::py_str(v).chars().take(2000).collect::<String>()),
                _ => {
                    let mut allowed = ANN_FIELDS.to_vec();
                    allowed.sort();
                    return Err(format!("cannot set '{k}' (allowed: {})", allowed.join(", ")));
                }
            };
            clean.insert(k.clone(), v);
        }
        let path = self.ws.join("library.json");
        let mut data = read_json(&path).map_err(|e| e.to_string())?.unwrap_or(json!({}));
        if !data.is_object() {
            data = json!({});
        }
        let ann = data.as_object_mut().unwrap().entry("annotations").or_insert(json!({}));
        let entry = ann.as_object_mut().ok_or("library.json: annotations is not an object")?.entry(id).or_insert(json!({}));
        entry.as_object_mut().ok_or("library.json: bad annotation")?.extend(clean);
        let result = entry.clone();
        write_json(&path, &data).map_err(|e| e.to_string())?;
        Ok(result)
    }

    /// Move a take to `<workspace>/trash` (never deletes audio outright).
    pub fn trash(&self, id: &str) -> std::io::Result<bool> {
        let Some(folder) = self.folder(id).filter(|f| f.join("meta.json").is_file()) else { return Ok(false) };
        move_to_trash(&self.ws, &folder).map(|_| true)
    }
}

fn as_int(v: &Value) -> Result<i64, String> {
    match v {
        Value::Number(n) => n.as_f64().map(|f| f as i64).ok_or_else(|| "not a number".into()),
        Value::String(s) => s.trim().parse().map_err(|_| format!("not a number: {s}")),
        Value::Bool(b) => Ok(i64::from(*b)),
        _ => Err("not a number".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib(name: &str) -> Library {
        let ws = std::env::temp_dir().join(format!("nusic-takes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        Library { out_root: ws.join("output"), ws }
    }

    fn add(lib: &Library, id: &str, created: &str) {
        let d = lib.out_root.join(id);
        write_json(&d.join("meta.json"), &json!({"id": id, "title": "T", "created": created, "spec": {"style": "lofi"},
            "variants": [{"index": 1, "file": "v1.flac", "seed": 1}]}))
        .unwrap();
        std::fs::write(d.join("v1.flac"), b"x").unwrap();
    }

    #[test]
    fn lists_newest_first_and_keeps_annotations() {
        let l = lib("list");
        add(&l, "a", "2026-01-01T00:00:00");
        add(&l, "b", "2026-02-01T00:00:00");
        std::fs::create_dir_all(l.out_root.join("half")).unwrap(); // no meta.json: not a take
        let ids: Vec<Value> = l.list().iter().map(|t| t["id"].clone()).collect();
        assert_eq!(ids, vec![json!("b"), json!("a")]);
        let mut f = Obj::new();
        f.insert("rating".into(), json!(9));
        f.insert("tags".into(), json!("boss, loop ,"));
        f.insert("title".into(), json!("Mine"));
        assert_eq!(l.annotate("a", &f).unwrap(), json!({"rating": 5, "tags": ["boss", "loop"], "title": "Mine"}));
        let a = l.get("a").unwrap();
        assert_eq!(a["title"], "Mine");
        assert_eq!(a["variants"][0]["exists"], true);
        let mut bad = Obj::new();
        bad.insert("color".into(), json!("red"));
        assert!(l.annotate("a", &bad).unwrap_err().contains("cannot set 'color'"));
    }

    #[test]
    fn trash_moves_the_folder_and_refuses_paths() {
        let l = lib("trash");
        add(&l, "a", "2026-01-01T00:00:00");
        assert!(l.trash("a").unwrap());
        assert!(l.ws.join("trash/a/v1.flac").is_file());
        assert!(!l.trash("a").unwrap());
        assert!(!l.trash("../a").unwrap());
        assert!(l.get("..").is_none());
    }
}
