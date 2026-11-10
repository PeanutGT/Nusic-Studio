//! What the app's commands call: one place that loads defaults + workspace and runs the ports.

use super::catalog::Catalog;
use super::config::{self, Prov};
use super::json::{Obj, read_json, set_dotted, unset_dotted, write_json};
use super::plan::{self, PlanInput};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub struct Studio {
    /// Built-in data (`resources/defaults`).
    pub defaults_dir: PathBuf,
}

fn cap(s: &mut String, n: usize) {
    if s.chars().count() > n {
        *s = s.chars().take(n).collect();
    }
}

impl Studio {
    pub fn config(&self, ws: &Path) -> Result<(Value, Prov), String> {
        config::load_config(&self.defaults_dir, ws)
    }

    pub fn catalog(&self, ws: &Path) -> Catalog {
        Catalog::load(&self.defaults_dir, ws)
    }

    pub fn knobs(&self) -> Value {
        config::load_knobs(&self.defaults_dir)
    }

    /// Build the track spec for a request. Per-take engine settings are checked against knobs.json.
    pub fn spec(&self, ws: &Path, input: &PlanInput) -> Result<Value, String> {
        let mut input = input.clone();
        cap(&mut input.description, 2000);
        cap(&mut input.caption_extra, 1000);
        if let Some(c) = input.caption_override.as_mut() {
            cap(c, 1000);
        }
        let (cfg, _) = self.config(ws)?;
        let mut spec = plan::build_spec(&cfg, &self.catalog(ws), &input)?;
        if let Some(ep) = input.engine_params.as_ref().filter(|e| !e.is_empty()) {
            let (clean, errors) = config::check_engine_params(ep, &self.knobs(), true);
            if !errors.is_empty() {
                return Err(errors.join("; "));
            }
            if !clean.is_empty() {
                spec["engine_params"] = Value::Object(clean);
            }
        }
        Ok(spec)
    }

    /// Live preview: the spec plus the engine parameters it would run with (no generation).
    pub fn plan(&self, ws: &Path, input: &PlanInput, tier: &str) -> Result<Value, String> {
        let spec = self.spec(ws, input)?;
        let (cfg, _) = self.config(ws)?;
        let mut overrides = Obj::new();
        overrides.insert("duration".into(), spec["duration"].clone());
        overrides.insert("batch_size".into(), spec["variants"].clone());
        if let Some(ep) = spec.get("engine_params").and_then(Value::as_object) {
            overrides.extend(ep.clone());
        }
        let r = config::resolve_params(&cfg, spec["quality"].as_str(), Some(&overrides), tier);
        Ok(json!({"spec": spec, "params": r.params, "sources": r.sources, "tier": r.tier, "quality": r.quality,
                  "warnings": r.warnings}))
    }

    /// Everything the create screen offers: styles, moods, workflows, instruments, presets.
    pub fn overview(&self, ws: &Path, tier: &str) -> Result<Value, String> {
        let (cfg, _) = self.config(ws)?;
        let cat = self.catalog(ws);
        let pick = |v: &Value, keys: &[&str]| -> Value {
            Value::Object(keys.iter().map(|k| (k.to_string(), v.get(*k).cloned().unwrap_or(Value::Null))).collect())
        };
        let styles: Vec<Value> = cat
            .styles
            .values()
            .map(|s| pick(s, &["id", "name", "name_zh", "description_zh", "_source", "bpm", "duration", "loopable", "instruments"]))
            .collect();
        let moods: Vec<Value> = cat.moods.values().map(|m| pick(m, &["id", "label_zh", "caption", "_source"])).collect();
        let workflows: Vec<Value> = cat.workflows.values().map(|w| pick(w, &["id", "name", "name_zh", "description_zh", "_source"])).collect();
        let instruments: Vec<Value> = plan::instrument_catalog(&cat.lexicon)
            .iter()
            .map(|(k, v)| json!({"id": k, "label": plan::instrument_label(k, v)}))
            .collect();
        let wf_ids: Vec<String> = cat.workflows.keys().cloned().collect();
        let mut problems = cat.problems.clone();
        problems.extend(config::validate_config(&cfg, &self.knobs(), &wf_ids));
        Ok(json!({
            "styles": styles, "moods": moods, "workflows": workflows, "instruments": instruments,
            "presets": cfg["quality"]["presets"], "default_quality": cfg["quality"]["default"],
            "default_workflow": cfg["workflow"]["default"], "tier": tier,
            "tier_caps": cfg["hardware"]["tiers"].get(tier).cloned().unwrap_or(Value::Null),
            "problems": problems,
        }))
    }

    pub fn knob_table(&self, ws: &Path, quality: Option<&str>, tier: &str) -> Result<Value, String> {
        let (cfg, prov) = self.config(ws)?;
        Ok(config::knob_table(&cfg, &prov, &self.knobs(), quality, tier))
    }

    /// Write (`Some`) or remove (`None`) a dotted override in `<workspace>/config.json`.
    /// Returns the config problems after the change.
    pub fn set_config(&self, ws: &Path, key: &str, value: Option<Value>) -> Result<Vec<String>, String> {
        if key.is_empty() || key.split('.').any(|p| p.is_empty() || p.starts_with('_')) {
            return Err(format!("bad setting name '{key}'"));
        }
        let path = ws.join("config.json");
        let mut user = read_json(&path)?.unwrap_or_else(|| json!({}));
        match value {
            Some(v) => set_dotted(&mut user, key, v),
            None => {
                unset_dotted(&mut user, key);
            }
        }
        write_json(&path, &user).map_err(|e| e.to_string())?;
        let (cfg, _) = self.config(ws)?;
        let wf_ids: Vec<String> = self.catalog(ws).workflows.keys().cloned().collect();
        Ok(config::validate_config(&cfg, &self.knobs(), &wf_ids))
    }
}
