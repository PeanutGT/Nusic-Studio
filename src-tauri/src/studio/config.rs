//! Layered configuration and engine-parameter resolution (port of v3.7 `msconfig.py`).
//!
//! Layers, lowest to highest: built-in defaults (`resources/defaults/config.json`)
//! -> workspace `config.json` -> the request. Engine parameters resolve as
//! generation < tier < quality preset < engine_overrides < request.

use super::json::{Obj, get_dotted, num, opt_truthy, py_repr, py_str, read_json, truthy};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// Which layer each leaf value came from, by dotted path.
pub type Prov = BTreeMap<String, String>;

/// Deep-merge `over` onto a copy of `base`, recording the source of every replaced leaf.
/// Dicts merge key by key; lists and scalars replace. A dict with `"$replace": true` replaces wholesale.
/// Keys starting with `_` (documentation) are skipped.
pub fn merge(base: &Value, over: &Value, source: &str, prov: &mut Option<&mut Prov>, prefix: &str) -> Value {
    let mut out = base.clone();
    let (Some(out_obj), Some(over_obj)) = (out.as_object_mut(), over.as_object()) else {
        return out;
    };
    for (key, val) in over_obj {
        if key.starts_with('_') {
            continue;
        }
        let path = format!("{prefix}{key}");
        let replace = val.get("$replace").is_some_and(truthy);
        match (val.as_object(), out_obj.get(key)) {
            (Some(_), Some(Value::Object(_))) if !replace => {
                let merged = merge(&out_obj[key], val, source, prov, &format!("{path}."));
                out_obj.insert(key.clone(), merged);
            }
            _ => {
                let mut val = val.clone();
                if let Some(o) = val.as_object_mut() {
                    o.shift_remove("$replace");
                }
                if let Some(p) = prov.as_deref_mut() {
                    mark(p, &path, &val, source);
                }
                out_obj.insert(key.clone(), val);
            }
        }
    }
    out
}

fn mark(prov: &mut Prov, path: &str, val: &Value, source: &str) {
    match val.as_object() {
        Some(o) => {
            for (k, v) in o.iter().filter(|(k, _)| !k.starts_with('_')) {
                mark(prov, &format!("{path}.{k}"), v, source);
            }
        }
        None => {
            prov.insert(path.to_string(), source.to_string());
        }
    }
}

pub fn strip_docs(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            Value::Object(o.iter().filter(|(k, _)| !k.starts_with('_')).map(|(k, v)| (k.clone(), strip_docs(v))).collect())
        }
        other => other.clone(),
    }
}

fn clean(v: &Value) -> Value {
    match v.as_object() {
        Some(o) => Value::Object(o.iter().filter(|(k, _)| !k.starts_with('_')).map(|(k, v)| (k.clone(), v.clone())).collect()),
        None => json!({}),
    }
}

/// Effective config and the provenance (`default` / `user`) of every leaf.
pub fn load_config(defaults_dir: &Path, ws: &Path) -> Result<(Value, Prov), String> {
    let base = read_json(&defaults_dir.join("config.json"))
        .map_err(|e| format!("built-in config.json: {e}"))?
        .ok_or("built-in config.json is missing")?;
    let base = strip_docs(&base);
    let mut prov = Prov::new();
    mark(&mut prov, "", &base, "default");
    let mut prov: Prov = prov.into_iter().map(|(k, v)| (k.trim_start_matches('.').to_string(), v)).collect();
    let user = read_json(&ws.join("config.json")).map_err(|e| format!("{}: {e}", ws.join("config.json").display()))?;
    let cfg = merge(&base, &user.unwrap_or_else(|| json!({})), "user", &mut Some(&mut prov), "");
    Ok((cfg, prov))
}

/// Upper VRAM bounds (GB, exclusive) per tier, as in v3.7 `gpu.py`; above the last is `ultra`.
const TIER_BOUNDS: [(&str, f64); 3] = [("low", 6.0), ("mid", 12.0), ("high", 20.0)];

pub fn tier_for(vram_gb: Option<f64>) -> &'static str {
    // No NVIDIA GPU: ACE-Step would run on CPU; use the lightest tier.
    let Some(gb) = vram_gb else { return "low" };
    TIER_BOUNDS.iter().find(|(_, bound)| gb < *bound).map_or("ultra", |(name, _)| name)
}

/// `hardware.tier` when forced to a defined tier, else the tier for the detected VRAM.
pub fn resolve_tier(cfg: &Value, vram_gb: Option<f64>) -> (String, Vec<String>) {
    let wanted = cfg["hardware"]["tier"].as_str().unwrap_or("auto");
    if wanted != "auto" {
        if cfg["hardware"]["tiers"].get(wanted).is_some() {
            return (wanted.to_string(), vec![]);
        }
        return (tier_for(vram_gb).into(), vec![format!("hardware.tier '{wanted}' is not defined; using detected tier")]);
    }
    (tier_for(vram_gb).into(), vec![])
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Resolved {
    pub params: Obj,
    pub tier: String,
    pub quality: String,
    pub warnings: Vec<String>,
    pub sources: Prov,
}

/// Engine parameters for one generation. `overrides` are the request's own values (null = not set).
pub fn resolve_params(cfg: &Value, quality: Option<&str>, overrides: Option<&Obj>, tier: &str) -> Resolved {
    let mut warnings = Vec::new();
    let tiers = cfg["hardware"]["tiers"].as_object().cloned().unwrap_or_default();
    let tier_cfg = tiers
        .get(tier)
        .filter(|t| truthy(t))
        .or_else(|| tiers.values().next())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let default_q = cfg["quality"]["default"].as_str().unwrap_or("standard").to_string();
    let mut quality = quality.filter(|q| !q.is_empty()).unwrap_or(&default_q).to_string();
    let presets = &cfg["quality"]["presets"];
    if presets.get(&quality).is_none() {
        warnings.push(format!("quality '{quality}' unknown, using '{default_q}'"));
        quality = default_q;
    }
    let tier_layer: Obj = tier_cfg
        .as_object()
        .map(|o| o.iter().filter(|(k, _)| *k != "max_duration" && *k != "max_batch").map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    let request: Obj = overrides
        .map(|o| o.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    let layers: [(String, Value); 5] = [
        ("generation".into(), cfg["generation"].clone()),
        (format!("tier:{tier}"), Value::Object(tier_layer)),
        (format!("preset:{quality}"), presets[&quality].clone()),
        ("engine_overrides".into(), cfg.get("engine_overrides").cloned().unwrap_or_else(|| json!({}))),
        ("request".into(), Value::Object(request)),
    ];
    let mut prov = Prov::new();
    let mut params = json!({});
    for (name, layer) in &layers {
        params = merge(&params, &clean(layer), name, &mut Some(&mut prov), "");
    }
    let mut params = params.as_object().cloned().unwrap_or_default();
    params.insert("lm_backend".into(), cfg["hardware"].get("lm_backend").cloned().unwrap_or_else(|| json!("pt")));

    let ov = |k: &str| overrides.and_then(|o| o.get(k));
    if let Some(max_dur) = tier_cfg.get("max_duration").filter(|v| truthy(v)) {
        if num(params.get("duration")).unwrap_or(0.0) > num(Some(max_dur)).unwrap_or(f64::MAX) {
            warnings.push(format!(
                "duration {}s exceeds the {tier} tier cap ({}s); clamped. Raise hardware.tiers.{tier}.max_duration if your card copes.",
                py_str(&params["duration"]),
                py_str(max_dur)
            ));
            params.insert("duration".into(), max_dur.clone());
        }
    }
    if let Some(max_batch) = tier_cfg.get("max_batch").filter(|v| truthy(v)) {
        if num(params.get("batch_size")).unwrap_or(1.0) > num(Some(max_batch)).unwrap_or(f64::MAX) {
            if opt_truthy(ov("batch_size")) {
                warnings.push(format!(
                    "you asked for {} variants but the {tier} tier allows {}; using {}.",
                    py_str(&params["batch_size"]),
                    py_str(max_batch),
                    py_str(max_batch)
                ));
            }
            params.insert("batch_size".into(), max_batch.clone());
        }
    }
    if opt_truthy(params.get("thinking")) && !opt_truthy(params.get("lm_model")) {
        if opt_truthy(ov("thinking")) {
            warnings.push(format!("the {tier} tier has no language model, so 'thinking' is off."));
        }
        params.insert("thinking".into(), json!(false));
    }
    Resolved { params, tier: tier.to_string(), quality, warnings, sources: prov }
}

// ------------------------------------------------------------------ tunable settings (knobs.json)

pub fn load_knobs(defaults_dir: &Path) -> Value {
    read_json(&defaults_dir.join("knobs.json")).ok().flatten().unwrap_or_else(|| json!({}))
}

fn knobs_where(knobs: &Value, pred: impl Fn(&str) -> bool) -> BTreeMap<String, Value> {
    knobs["knobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|k| pred(k.get("layer").and_then(Value::as_str).unwrap_or("engine")))
        .filter_map(|k| Some((k["key"].as_str()?.to_string(), k.clone())))
        .collect()
}

pub fn engine_knobs(knobs: &Value) -> BTreeMap<String, Value> {
    knobs_where(knobs, |layer| layer == "engine")
}

/// Coerce `value` to the knob's type. Returns the value and an error message when it does not fit.
pub fn check_knob(knob: &Value, value: &Value) -> (Value, Option<String>) {
    let key = knob["key"].as_str().unwrap_or("?");
    match knob["type"].as_str().unwrap_or("") {
        "bool" => {
            let v = match value.as_str().map(str::to_lowercase).as_deref() {
                Some("true") => json!(true),
                Some("false") => json!(false),
                _ => value.clone(),
            };
            let err = (!v.is_boolean()).then(|| format!("{key} must be true or false"));
            (v, err)
        }
        "choice" => {
            let choices = knob["choices"].as_array().cloned().unwrap_or_default();
            let err = (!choices.contains(value))
                .then(|| format!("{key} must be one of {}", choices.iter().map(py_str).collect::<Vec<_>>().join(", ")));
            (value.clone(), err)
        }
        "string" => {
            let err = (!(value.is_null() || value.is_string())).then(|| format!("{key} must be text"));
            (value.clone(), err)
        }
        t => {
            let parsed = match value {
                Value::Number(n) => n.as_f64(),
                Value::String(s) => s.trim().parse::<f64>().ok(),
                _ => None,
            };
            let is_int = t == "int";
            let Some(f) = parsed.filter(|f| !is_int || f.fract() == 0.0) else {
                let what = if is_int { "an integer" } else { "a number" };
                return (value.clone(), Some(format!("{key} must be {what}, got {}", py_repr(value))));
            };
            let v = if is_int { json!(f as i64) } else { json!(f) };
            let (lo, hi) = (num(knob.get("min")), num(knob.get("max")));
            if lo.is_some_and(|lo| f < lo) || hi.is_some_and(|hi| f > hi) {
                let err = format!("{key} must be between {} and {}, got {}", py_str(&knob["min"]), py_str(&knob["max"]), py_str(&v));
                return (v, Some(err));
            }
            (v, None)
        }
    }
}

/// Validate per-take engine parameters against the catalog. Unknown keys are errors.
pub fn check_engine_params(params: &Obj, knobs: &Value, per_request_only: bool) -> (Obj, Vec<String>) {
    let cat = engine_knobs(knobs);
    let mut clean = Obj::new();
    let mut errors = Vec::new();
    for (key, value) in params {
        let Some(knob) = cat.get(key).filter(|k| !per_request_only || opt_truthy(k.get("per_request"))) else {
            errors.push(format!("{key} is not a per-take setting"));
            continue;
        };
        if value.is_null() || value.as_str() == Some("") {
            continue;
        }
        match check_knob(knob, value) {
            (_, Some(err)) => errors.push(err),
            (v, None) => {
                clean.insert(key.clone(), v);
            }
        }
    }
    (clean, errors)
}

fn layer_path(source: &str, key: &str) -> Option<String> {
    if source == "generation" || source == "engine_overrides" {
        return Some(format!("{source}.{key}"));
    }
    if let Some(t) = source.strip_prefix("tier:") {
        return Some(format!("hardware.tiers.{t}.{key}"));
    }
    source.strip_prefix("preset:").map(|q| format!("quality.presets.{q}.{key}"))
}

/// Every tunable setting with its effective value for one quality preset and where it came from.
pub fn knob_table(cfg: &Value, prov: &Prov, knobs: &Value, quality: Option<&str>, tier: &str) -> Value {
    let r = resolve_params(cfg, quality, None, tier);
    let tier_cfg = cfg["hardware"]["tiers"].get(&r.tier).cloned().unwrap_or_else(|| json!({}));
    let presets: Vec<String> = cfg["quality"]["presets"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
    let tiers: Vec<String> = cfg["hardware"]["tiers"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
    let mut rows = Vec::new();
    for k in knobs["knobs"].as_array().into_iter().flatten() {
        let key = k["key"].as_str().unwrap_or_default();
        let (source, value) = if k.get("layer").and_then(Value::as_str) == Some("tier") {
            let v = tier_cfg.get(key).cloned();
            (v.as_ref().map(|_| format!("tier:{}", r.tier)), v.unwrap_or(Value::Null))
        } else {
            (r.sources.get(key).cloned(), r.params.get(key).cloned().unwrap_or(Value::Null))
        };
        let path = source.as_deref().and_then(|s| layer_path(s, key));
        let raw = path.as_deref().and_then(|p| get_dotted(cfg, p)).cloned().unwrap_or(Value::Null);
        let mut candidates = vec![format!("generation.{key}"), format!("engine_overrides.{key}")];
        candidates.extend(presets.iter().map(|q| format!("quality.presets.{q}.{key}")));
        candidates.extend(tiers.iter().map(|t| format!("hardware.tiers.{t}.{key}")));
        let mut row = k.as_object().cloned().unwrap_or_default();
        row.insert("value".into(), value.clone());
        row.insert("source".into(), json!(source));
        row.insert("path".into(), json!(path));
        row.insert("from_user".into(), json!(path.as_ref().is_some_and(|p| prov.get(p).is_some_and(|s| s == "user"))));
        row.insert("adjusted_from".into(), if path.is_some() && raw != value { raw } else { Value::Null });
        let user_paths: Vec<&String> = candidates.iter().filter(|c| prov.get(*c).is_some_and(|s| s == "user")).collect();
        row.insert("user_paths".into(), json!(user_paths));
        rows.push(Value::Object(row));
    }
    json!({
        "quality": r.quality,
        "tier": r.tier,
        "groups": knobs.get("groups").cloned().unwrap_or_else(|| json!({})),
        "knobs": rows,
        "warnings": r.warnings,
        "presets": cfg["quality"]["presets"],
        "tiers": cfg["hardware"]["tiers"],
        "engine_overrides": cfg.get("engine_overrides").cloned().unwrap_or_else(|| json!({})),
        "not_passed": knobs.get("not_passed").and_then(|n| n.get("items")).cloned().unwrap_or_else(|| json!([])),
        "precedence": ["generation", "tier", "preset", "engine_overrides", "request"],
    })
}

/// Problems with the effective config (knob types and ranges, presets, tier, default workflow).
pub fn validate_config(cfg: &Value, knobs: &Value, workflow_ids: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    let presets = cfg["quality"]["presets"].as_object().cloned().unwrap_or_default();
    let tiers = cfg["hardware"]["tiers"].as_object().cloned().unwrap_or_default();
    let qd = py_str(&cfg["quality"]["default"]);
    if !presets.contains_key(&qd) {
        problems.push(format!("quality.default '{qd}' is not a defined preset"));
    }
    let tier = py_str(&cfg["hardware"]["tier"]);
    if tier != "auto" && !tiers.contains_key(&tier) {
        problems.push(format!("hardware.tier '{tier}' is not 'auto' or a defined tier"));
    }
    let wd = py_str(&cfg["workflow"]["default"]);
    if !workflow_ids.contains(&wd) {
        problems.push(format!("workflow.default '{wd}' does not exist"));
    }
    for key in ["duration", "guidance_scale", "shift", "inference_steps"] {
        let v = &cfg["generation"][key];
        if v.is_boolean() || num(Some(v)).is_none_or(|f| f <= 0.0) {
            problems.push(format!("generation.{key} must be a positive number, got {}", py_repr(v)));
        }
    }
    let engine_cat = engine_knobs(knobs);
    let tier_cat = knobs_where(knobs, |l| l == "tier");
    let mut layers = vec![("generation".to_string(), cfg["generation"].clone())];
    layers.push(("engine_overrides".into(), cfg.get("engine_overrides").cloned().unwrap_or_else(|| json!({}))));
    layers.extend(presets.iter().map(|(q, p)| (format!("quality.presets.{q}"), p.clone())));
    for (wher, layer) in &layers {
        for (key, value) in layer.as_object().into_iter().flatten() {
            if let (Some(knob), false) = (engine_cat.get(key), value.is_null()) {
                if let (_, Some(err)) = check_knob(knob, value) {
                    problems.push(format!("{wher}.{err}"));
                }
            }
        }
    }
    for (t, tc) in &tiers {
        for (key, value) in tc.as_object().into_iter().flatten() {
            if let (Some(knob), false) = (tier_cat.get(key), value.is_null()) {
                if let (_, Some(err)) = check_knob(knob, value) {
                    problems.push(format!("hardware.tiers.{t}.{err}"));
                }
            }
        }
    }
    for (name, preset) in &presets {
        let b = preset.get("batch_size").cloned().unwrap_or(json!(1));
        if !b.as_i64().is_some_and(|b| (1..=8).contains(&b)) {
            problems.push(format!("quality.presets.{name}.batch_size must be an integer 1-8"));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_records_user_leaves_and_honours_replace() {
        let base = json!({"a": {"x": 1, "y": 2}, "b": [1]});
        let over = json!({"a": {"y": 3}, "b": [2], "_doc": "skip", "c": {"$replace": true, "z": 1}});
        let mut prov = Prov::new();
        let out = merge(&base, &over, "user", &mut Some(&mut prov), "");
        assert_eq!(out, json!({"a": {"x": 1, "y": 3}, "b": [2], "c": {"z": 1}}));
        assert_eq!(prov.get("a.y").map(String::as_str), Some("user"));
        assert_eq!(prov.get("c.z").map(String::as_str), Some("user"));
        assert!(!prov.contains_key("a.x"));
    }

    #[test]
    fn tiers_follow_vram() {
        assert_eq!(tier_for(Some(4.0)), "low");
        assert_eq!(tier_for(Some(8.0)), "mid");
        assert_eq!(tier_for(Some(16.0)), "high");
        assert_eq!(tier_for(Some(24.0)), "ultra");
        assert_eq!(tier_for(None), "low");
    }

    #[test]
    fn knob_checks_coerce_and_reject() {
        let k = json!({"key": "inference_steps", "type": "int", "min": 1, "max": 200});
        assert_eq!(check_knob(&k, &json!("12")), (json!(12), None));
        assert!(check_knob(&k, &json!(2.5)).1.unwrap().contains("an integer"));
        assert_eq!(check_knob(&k, &json!(500)).1.unwrap(), "inference_steps must be between 1 and 200, got 500");
        let b = json!({"key": "thinking", "type": "bool"});
        assert_eq!(check_knob(&b, &json!("TRUE")), (json!(true), None));
    }
}
