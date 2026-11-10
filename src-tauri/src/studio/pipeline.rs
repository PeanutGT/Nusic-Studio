//! One generation from spec to finished take (port of v3.7 `pipeline.py`).
//!
//! `prepare` resolves the engine parameters, creates the take folder and builds the worker request;
//! `finish` writes `meta.json` (the take now exists in the library); `discard` moves a failed or
//! cancelled folder to `<workspace>/trash`, so the library never shows half a take.

use super::config::{Resolved, resolve_params};
use super::json::{Obj, num, py_str, read_json, truthy, write_json};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Everything about one job the queue needs to run and finish it.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub take_id: String,
    pub out_dir: PathBuf,
    pub title: String,
    pub spec: Value,
    pub resolved: Resolved,
    pub warnings: Vec<String>,
    pub seeds: Vec<i64>,
    pub request: Value,
}

/// Where takes go: `output.dir`, relative paths inside the workspace.
pub fn output_root(cfg: &Value, ws: &Path) -> PathBuf {
    let dir = cfg["output"]["dir"].as_str().unwrap_or("output");
    let p = PathBuf::from(dir);
    if p.is_absolute() { p } else { ws.join(p) }
}

/// Filesystem-safe slug that keeps CJK letters (v3.7 `common.slugify`).
pub fn slugify(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.trim().chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() || c == '_' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed: String = out.trim_matches(['-', '_']).chars().take(max).collect();
    let trimmed = trimmed.trim_matches(['-', '_']);
    if trimmed.is_empty() { "track".into() } else { trimmed.into() }
}

pub fn make_seeds(base: i64, n: usize) -> Vec<i64> {
    if base < 0 {
        let mut x = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) | 1;
        (0..n)
            .map(|_| {
                // xorshift64*: plenty for picking diffusion seeds
                x ^= x >> 12;
                x ^= x << 25;
                x ^= x >> 27;
                1 + (x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 33) as i64 % 2_147_483_646
            })
            .collect()
    } else {
        (0..n as i64).map(|i| base + i).collect()
    }
}

fn has_key(v: &Value, key: &str) -> bool {
    v.as_object().is_some_and(|o| o.contains_key(key) || o.values().any(|v| has_key(v, key)))
}

pub fn checkpoints_dir(cfg: &Value, ace_dir: &Path) -> PathBuf {
    let custom = cfg.get("ace_checkpoints_dir").and_then(Value::as_str).map(str::to_string).filter(|s| !s.is_empty());
    match custom.or_else(|| std::env::var("ACESTEP_CHECKPOINTS_DIR").ok().filter(|s| !s.is_empty())) {
        Some(p) => PathBuf::from(p),
        None => ace_dir.join("checkpoints"),
    }
}

fn now_local() -> chrono::DateTime<chrono::Local> {
    chrono::Local::now()
}

/// Resolve parameters, create `<output>/<timestamp>-<slug>` with `spec.json`, build the worker request.
pub fn prepare(cfg: &Value, ws: &Path, spec: &Value, tier: &str, ace_dir: &Path, job: &str) -> Result<Prepared, String> {
    let mut overrides = Obj::new();
    overrides.insert("duration".into(), spec.get("duration").cloned().unwrap_or(Value::Null));
    overrides.insert("batch_size".into(), spec.get("variants").cloned().unwrap_or(Value::Null));
    overrides.insert("instrumental".into(), spec.get("instrumental").cloned().unwrap_or(Value::Null));
    if let Some(ep) = spec.get("engine_params").and_then(Value::as_object) {
        overrides.extend(ep.clone());
    }
    let resolved = resolve_params(cfg, spec.get("quality").and_then(Value::as_str), Some(&overrides), tier);
    let mut warnings = resolved.warnings.clone();
    warnings.extend(spec.get("warnings").and_then(Value::as_array).into_iter().flatten().map(py_str));

    let title = ["title", "style"].iter().find_map(|k| spec.get(*k).filter(|v| truthy(v))).map_or("track".into(), py_str);
    let root = output_root(cfg, ws);
    std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let base = format!("{}-{}", now_local().format("%Y%m%d-%H%M%S"), slugify(&title, 40));
    let (mut out_dir, mut n) = (root.join(&base), 2);
    while out_dir.exists() {
        out_dir = root.join(format!("{base}-{n}"));
        n += 1;
    }
    std::fs::create_dir(&out_dir).map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;
    write_json(&out_dir.join("spec.json"), spec).map_err(|e| e.to_string())?;

    let batch = num(resolved.params.get("batch_size")).unwrap_or(1.0).max(1.0) as usize;
    let seeds = make_seeds(spec.get("seed").and_then(Value::as_i64).unwrap_or(-1), batch);
    let user_cfg = read_json(&ws.join("config.json")).ok().flatten().unwrap_or(Value::Null);
    let explicit: Vec<&str> = ["offload_dit_to_cpu"].into_iter().filter(|k| has_key(&user_cfg, k)).collect();
    let params = Value::Object(resolved.params.clone());
    let request = json!({
        "op": "generate",
        "job": job,
        "ace_step_dir": ace_dir.to_string_lossy(),
        "checkpoints_dir": checkpoints_dir(cfg, ace_dir).to_string_lossy(),
        "params": params,
        "tier_dit_model": cfg["hardware"]["tiers"].get(&resolved.tier).and_then(|t| t.get("dit_model")),
        "explicit_keys": explicit,
        "vram_guard": cfg.get("engine_vram_guard").cloned().unwrap_or(json!(true)),
        "caption": spec["caption"],
        "bpm": spec.get("bpm"),
        "key": spec.get("key"),
        "time_signature": spec.get("time_signature"),
        "instrumental": params.get("instrumental").cloned().unwrap_or(json!(true)),
        "lyrics": spec.get("lyrics").cloned().unwrap_or(json!("")),
        "seeds": seeds,
        "out_dir": out_dir.to_string_lossy(),
        "warnings": warnings,
    });
    let take_id = out_dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
    Ok(Prepared { take_id, out_dir, title, spec: spec.clone(), resolved, warnings, seeds, request })
}

/// What the worker's `result` event reported.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub variants: Value,
    pub warnings: Vec<String>,
    pub effective: Value,
    pub lm_metadata: Value,
    pub time_costs: Value,
    pub elapsed_s: f64,
}

/// Write `meta.json`; from now on the take is in the library. Returns the meta.
pub fn finish(p: &Prepared, job: &str, out: &Outcome) -> std::io::Result<Value> {
    let mut warnings = p.warnings.clone();
    warnings.extend(out.warnings.iter().filter(|w| !p.warnings.contains(w)).cloned());
    let meta = json!({
        "id": p.take_id,
        "title": p.title,
        "spec": p.spec,
        "engine": "ace",
        "tier": p.resolved.tier,
        "quality": p.resolved.quality,
        "params": p.resolved.params,
        "warnings": warnings,
        "effective": out.effective,
        "lm_metadata": out.lm_metadata,
        "time_costs": out.time_costs,
        "job": job,
        "created": now_local().format("%Y-%m-%dT%H:%M:%S").to_string(),
        "elapsed_s": (out.elapsed_s * 10.0).round() / 10.0,
        "variants": out.variants,
    });
    write_json(&p.out_dir.join("meta.json"), &meta)?;
    Ok(meta)
}

/// Failed or cancelled: keep nothing that looks like a take. The folder goes to trash for debugging.
pub fn discard(ws: &Path, out_dir: &Path, reason: &str, job: Option<&str>) -> std::io::Result<()> {
    if !out_dir.exists() {
        return Ok(());
    }
    let at = now_local().format("%Y-%m-%dT%H:%M:%S").to_string();
    write_json(&out_dir.join("failed.json"), &json!({"reason": reason, "job": job, "at": at}))?;
    move_to_trash(ws, out_dir).map(|_| ())
}

/// Move a folder into `<workspace>/trash`, suffixing `-2`, `-3`... on name clashes.
pub fn move_to_trash(ws: &Path, path: &Path) -> std::io::Result<PathBuf> {
    let trash = ws.join("trash");
    std::fs::create_dir_all(&trash)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let (mut dst, mut n) = (trash.join(&name), 2);
    while dst.exists() {
        dst = trash.join(format!("{name}-{n}"));
        n += 1;
    }
    std::fs::rename(path, &dst)?;
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_keeps_cjk_and_ascii_words() {
        assert_eq!(slugify("Calm Piano, rain & cafe!", 40), "calm-piano-rain-cafe");
        assert_eq!(slugify("戰敗的安魂曲", 40), "戰敗的安魂曲");
        assert_eq!(slugify("!!!", 40), "track");
        assert_eq!(slugify(&"ab".repeat(50), 40).chars().count(), 40);
    }

    #[test]
    fn fixed_seed_counts_up_and_random_is_31_bit() {
        assert_eq!(make_seeds(42, 3), vec![42, 43, 44]);
        assert!(make_seeds(-1, 4).iter().all(|s| (1..=2_147_483_646).contains(s)));
    }
}
