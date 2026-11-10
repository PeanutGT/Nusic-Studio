//! Post-processing steps and the finalize pipeline (port of v3.7 `postprocess.py` ops,
//! `run_steps`, `resolve_loudness`, `workflow_steps`, `run_for_take`).
//!
//! Step vocabulary (a workflow's `postprocess.steps`):
//!   {"op":"trim_silence", "threshold_db":-50, "keep_head":0, "keep_tail":0.05}
//!   {"op":"fit", "duration": 90 | "spec", "snap":"none"|"bars"}
//!   {"op":"fade", "in":0.0, "out":2.0}
//!   {"op":"loop", "crossfade":2.0, "snap_bars":true}
//!   {"op":"loudnorm", "target_lufs":-16, "true_peak_db":-1.5}
//!   {"op":"export", "formats":["flac","ogg"]}
//! Any step may carry "when": "loopable" | "not_loopable".
//!
//! Loudness is one plain gain, capped so the true peak stays under the ceiling: no compression,
//! the dynamics stay intact (v3.7's fix for the muffled, "blocked" highs of ffmpeg's dynamic mode).

use super::analyze;
use super::audio::{self, Audio, Tags};
use crate::studio::json::{Obj, num, py_float, py_str, read_json, truthy, write_json};
use crate::studio::pipeline::slugify;
use serde_json::{Value, json};
use std::f64::consts::FRAC_PI_2;
use std::path::Path;

/// Lossy export (mp3/ogg) overshoots the measured true peak by a few tenths of a dB.
pub const TP_MARGIN_DB: f64 = 0.5;
pub const OPS: [&str; 6] = ["trim_silence", "fit", "fade", "loop", "loudnorm", "export"];

/// What the steps know about the take, and what they report back.
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    pub bpm: Option<f64>,
    pub duration: Option<Value>,
    pub beats_per_bar: usize,
    pub loop_crossfade: f64,
    pub loudness_lufs: f64,
    pub true_peak_db: f64,
    pub warnings: Vec<String>,
    pub notes: Vec<String>,
    pub looped: bool,
    pub lufs_target: Option<f64>,
    pub tp_target: Option<f64>,
}

impl Ctx {
    pub fn new(cfg: &Value) -> Self {
        let pp = &cfg["postprocess"];
        Self {
            beats_per_bar: 4,
            loop_crossfade: pp["loop_crossfade"].as_f64().unwrap_or(2.0),
            loudness_lufs: pp["loudness_lufs"].as_f64().unwrap_or(-16.0),
            true_peak_db: pp["true_peak_db"].as_f64().unwrap_or(-1.5),
            ..Default::default()
        }
    }

    pub fn bar_samples(&self, sr: u32) -> Option<usize> {
        let bpm = self.bpm.filter(|b| *b > 0.0)?;
        Some((60.0 / bpm * self.beats_per_bar as f64 * sr as f64).round_ties_even() as usize)
    }
}

fn param(p: &Value, key: &str, default: f64) -> f64 {
    num(p.get(key)).unwrap_or(default)
}

fn secs(p: &Value, key: &str, default: f64, sr: u32) -> usize {
    (param(p, key, default) * sr as f64).max(0.0) as usize
}

/// numpy `linspace(0, stop, n)[i]`.
fn linspace(i: usize, n: usize, stop: f64) -> f64 {
    if n <= 1 { 0.0 } else if i == n - 1 { stop } else { i as f64 * (stop / (n - 1) as f64) }
}

fn scale(f: &mut [f32; 2], g: f64) {
    f[0] = (f[0] as f64 * g) as f32;
    f[1] = (f[1] as f64 * g) as f32;
}

pub fn trim_silence(mut a: Audio, p: &Value, ctx: &mut Ctx) -> Audio {
    let thr = 10f64.powf(param(p, "threshold_db", -50.0) / 20.0) as f32;
    let loud = |f: &[f32; 2]| f[0].abs().max(f[1].abs()) > thr;
    let (Some(first), Some(last)) = (a.frames.iter().position(loud), a.frames.iter().rposition(loud)) else {
        ctx.warnings.push("trim_silence: file is silent, left unchanged".into());
        return a;
    };
    let start = first.saturating_sub(secs(p, "keep_head", 0.0, a.sr));
    let end = a.len().min(last + 1 + secs(p, "keep_tail", 0.05, a.sr));
    a.frames.truncate(end);
    a.frames.drain(..start);
    a
}

pub fn fit(mut a: Audio, p: &Value, ctx: &mut Ctx) -> Audio {
    let dur = match p.get("duration") {
        None => ctx.duration.clone(),
        Some(Value::String(s)) if s == "spec" => ctx.duration.clone(),
        Some(v) => Some(v.clone()),
    };
    let Some(dur) = dur.filter(truthy).and_then(|d| num(Some(&d)).or_else(|| d.as_str().and_then(|s| s.parse().ok()))) else {
        ctx.warnings.push("fit: no target duration known, skipped".into());
        return a;
    };
    let mut n = (dur * a.sr as f64) as usize;
    if p.get("snap").and_then(Value::as_str) == Some("bars") {
        match ctx.bar_samples(a.sr) {
            Some(bar) => n = bar.max(n / bar * bar),
            None => ctx.warnings.push("fit: snap=bars needs a bpm, cutting at the exact duration instead".into()),
        }
    }
    if a.len() < n {
        let sr = a.sr as f64;
        ctx.warnings.push(format!("fit: track is {:.1}s, shorter than {:.1}s; not extended", a.len() as f64 / sr, n as f64 / sr));
        return a;
    }
    a.frames.truncate(n);
    a
}

pub fn fade(mut a: Audio, p: &Value, _ctx: &mut Ctx) -> Audio {
    let n = a.len();
    let fin = secs(p, "in", 0.0, a.sr).min(n);
    let fout = secs(p, "out", 0.0, a.sr).min(n);
    for i in 0..fin {
        scale(&mut a.frames[i], linspace(i, fin, FRAC_PI_2).sin());
    }
    for i in 0..fout {
        scale(&mut a.frames[n - fout + i], linspace(i, fout, FRAC_PI_2).cos());
    }
    a
}

/// Make the file loop seamlessly by folding its tail back over its head. The body ends where
/// the folded tail began, so the wrap from the last sample to the first is the natural continuation.
pub fn make_loop(a: Audio, p: &Value, ctx: &mut Ctx) -> Audio {
    let x_len = (param(p, "crossfade", ctx.loop_crossfade) * a.sr as f64).max(0.0) as usize;
    let n = a.len();
    if x_len == 0 || n < 6 * x_len {
        ctx.warnings.push("loop: track too short for that crossfade; skipped".into());
        return a;
    }
    let mut l = n - x_len;
    if p.get("snap_bars").is_none_or(truthy) {
        match ctx.bar_samples(a.sr) {
            Some(bar) if l >= bar => {
                l = l / bar * bar;
                ctx.notes.push(format!("loop length snapped to {} bars", l / bar));
            }
            Some(_) => {}
            None => ctx.notes.push("loop: no bpm known, length not snapped to bars".into()),
        }
    }
    let tail: Vec<[f32; 2]> = a.frames[l..l + x_len].to_vec();
    let mut body = a.frames;
    body.truncate(l);
    // Equal-power gains for uncorrelated material (evolving pads), equal-gain for repeating
    // material (loop-based drums, arps) which adds coherently; blend by how alike they are.
    let mean = |f: &[f32; 2]| ((f[0] + f[1]) / 2.0) as f64;
    let (mut dot, mut na, mut nb) = (0.0, 0.0, 0.0);
    for (h, t) in body[..x_len].iter().zip(&tail) {
        let (u, v) = (mean(h), mean(t));
        dot += u * v;
        na += u * u;
        nb += v * v;
    }
    let corr = (dot / (na.sqrt() * nb.sqrt() + 1e-12)).clamp(0.0, 1.0);
    for (i, (h, t)) in body[..x_len].iter_mut().zip(&tail).enumerate() {
        let u = linspace(i, x_len, 1.0);
        let g_in = (1.0 - corr) * (u * FRAC_PI_2).sin() + corr * u;
        let g_out = (1.0 - corr) * (u * FRAC_PI_2).cos() + corr * (1.0 - u);
        for c in 0..2 {
            h[c] = (h[c] as f64 * g_in + t[c] as f64 * g_out) as f32;
        }
    }
    ctx.notes.push(format!("loop crossfade {:.1}s, passage similarity {corr:.2}", x_len as f64 / a.sr as f64));
    ctx.looped = true;
    Audio { sr: a.sr, frames: body }
}

pub fn loudnorm(mut a: Audio, p: &Value, ctx: &mut Ctx) -> Result<Audio, String> {
    let target = param(p, "target_lufs", ctx.loudness_lufs);
    let tp = param(p, "true_peak_db", ctx.true_peak_db);
    ctx.lufs_target = Some(target); // the report checks against what was asked for
    ctx.tp_target = Some(tp);
    let m = analyze::loudness(&a)?;
    let Some(measured) = m.lufs.filter(|l| *l >= -69.0) else {
        ctx.warnings.push("loudnorm: audio is (almost) silent, skipped".into());
        return Ok(a);
    };
    let mut gain = target - measured;
    if let Some(peak) = m.true_peak_db
        && peak + gain > tp - TP_MARGIN_DB
    {
        gain = tp - TP_MARGIN_DB - peak;
        ctx.warnings.push(format!(
            "loudnorm: peaks allow only {:.1} LUFS (target {}); kept the dynamics instead of compressing",
            measured + gain,
            py_float(target)
        ));
    }
    ctx.notes.push(format!("loudness {measured:.1} -> {:.1} LUFS (linear gain {gain:+.1} dB)", measured + gain));
    let g = 10f64.powf(gain / 20.0);
    a.frames.iter_mut().for_each(|f| scale(f, g));
    Ok(a)
}

/// Run one non-export op by name.
pub fn apply(op: &str, a: Audio, p: &Value, ctx: &mut Ctx) -> Result<Audio, String> {
    Ok(match op {
        "trim_silence" => trim_silence(a, p, ctx),
        "fit" => fit(a, p, ctx),
        "fade" => fade(a, p, ctx),
        "loop" => make_loop(a, p, ctx),
        "loudnorm" => loudnorm(a, p, ctx)?,
        "export" => a,
        other => return Err(format!("unknown step '{other}'")),
    })
}

/// A preset name from `postprocess.loudness_presets`, or a number in LUFS -> loudnorm parameters.
pub fn resolve_loudness(cfg: &Value, value: Option<&Value>) -> Result<Option<Obj>, String> {
    let Some(v) = value.filter(|v| !v.is_null() && v.as_str() != Some("")) else { return Ok(None) };
    let pp = &cfg["postprocess"];
    let default_tp = pp["true_peak_db"].as_f64().unwrap_or(-1.5);
    let presets: Vec<(&String, &Value)> =
        pp["loudness_presets"].as_object().into_iter().flatten().filter(|(k, _)| !k.starts_with('_')).collect();
    let key = py_str(v);
    let mut out = Obj::new();
    if let Some((_, p)) = presets.iter().find(|(k, _)| **k == key) {
        out.insert("target_lufs".into(), json!(num(p.get("lufs")).unwrap_or(-16.0)));
        out.insert("true_peak_db".into(), json!(num(p.get("true_peak_db")).unwrap_or(default_tp)));
        return Ok(Some(out));
    }
    match num(Some(v)).or_else(|| key.trim().parse::<f64>().ok()).filter(|f| f.is_finite()) {
        Some(lufs) => {
            out.insert("target_lufs".into(), json!(lufs));
            out.insert("true_peak_db".into(), json!(default_tp));
            Ok(Some(out))
        }
        None => {
            let names: Vec<&str> = presets.iter().map(|(k, _)| k.as_str()).collect();
            Err(format!("unknown loudness '{key}'. Presets: {}; or a number such as -14", names.join(", ")))
        }
    }
}

/// The workflow's post-processing steps, or the v3.7 fallback.
pub fn workflow_steps(workflow: Option<&Value>) -> Vec<Value> {
    workflow
        .and_then(|w| w["stages"].as_array())
        .into_iter()
        .flatten()
        .find(|st| st["id"] == "postprocess" && st["steps"].as_array().is_some_and(|s| !s.is_empty()))
        .and_then(|st| st["steps"].as_array().cloned())
        .unwrap_or_else(|| vec![json!({"op": "loop", "snap_bars": true, "when": "loopable"}), json!({"op": "loudnorm"}), json!({"op": "export"})])
}

/// Where the exported files go: `<dir>/<base>.<ext>`, plus a 2x loop preview when asked for.
pub struct Target<'a> {
    pub dir: &'a Path,
    pub base: &'a str,
    pub preview: bool,
}

/// Decode `src`, run the steps, export, and measure the result.
/// Returns `{files, applied, notes, warnings, report}` like v3.7.
pub fn run_steps(src: &Path, steps: &[Value], out: &Target, cfg: &Value, spec: &Value, loudness: Option<&Obj>) -> Result<Value, String> {
    let (out_dir, base, preview) = (out.dir, out.base, out.preview);
    let mut steps = steps.to_vec();
    if let Some(l) = loudness {
        // a chosen preset wins over the workflow's own target; add the step if it had none
        let mut found = false;
        for st in steps.iter_mut().filter(|s| s["op"] == "loudnorm") {
            found = true;
            if let Some(o) = st.as_object_mut() {
                o.extend(l.clone());
            }
        }
        if !found {
            let at = steps.iter().position(|s| s["op"] == "export").unwrap_or(steps.len());
            let mut st = Obj::new();
            st.insert("op".into(), json!("loudnorm"));
            st.extend(l.clone());
            steps.insert(at, Value::Object(st));
        }
    }
    let sr = cfg["postprocess"]["sample_rate"].as_u64().unwrap_or(48000) as u32;
    let mut x = audio::decode(src)?;
    if x.sr != sr {
        return Err(format!("{} is {} Hz; this version only processes {sr} Hz audio", src.display(), x.sr));
    }
    let mut ctx = Ctx::new(cfg);
    ctx.bpm = num(spec.get("bpm"));
    ctx.duration = spec.get("duration").cloned();
    let ts = spec.get("time_signature").filter(|v| truthy(v)).map_or("4".into(), py_str);
    ctx.beats_per_bar = ts.split('/').next().and_then(|s| s.trim().parse().ok()).filter(|b: &usize| *b > 0).unwrap_or(4);
    let loopable = spec.get("loopable").is_some_and(truthy);
    let mut formats: Vec<String> = cfg["postprocess"]["export_formats"].as_array().into_iter().flatten().map(py_str).collect();
    let mut applied: Vec<String> = vec![];
    for step in &steps {
        let op = step["op"].as_str().unwrap_or_default();
        if !OPS.contains(&op) {
            return Err(format!("unknown step '{}'", py_str(&step["op"])));
        }
        match step["when"].as_str() {
            Some("loopable") if !loopable => continue,
            Some("not_loopable") if loopable => continue,
            _ => {}
        }
        if op == "export" {
            if let Some(f) = step["formats"].as_array().filter(|f| !f.is_empty()) {
                formats = f.iter().map(py_str).collect();
            }
            continue;
        }
        x = apply(op, x, step, &mut ctx)?;
        applied.push(op.into());
    }
    if formats.is_empty() {
        return Err("no export format".into());
    }
    if let Some(bad) = formats.iter().find(|f| !audio::FORMATS.contains(&f.as_str())) {
        return Err(format!("unknown export format '{bad}'. Known: {}", audio::FORMATS.join(", ")));
    }
    let title = spec.get("title").filter(|t| truthy(t)).map_or(base.to_string(), py_str);
    let style = spec.get("style").filter(|s| !s.is_null()).map_or("None".into(), py_str);
    let caption = spec.get("caption").map_or(String::new(), py_str);
    let comment: String = format!("nusic-studio | style={style} | caption={caption}").chars().take(500).collect();
    let tags = Tags { title, comment };
    std::fs::create_dir_all(out_dir).map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;
    let mut files = vec![];
    for fmt in &formats {
        let name = format!("{base}.{}", audio::extension(fmt));
        audio::encode(&x, &out_dir.join(&name), fmt, &tags)?;
        files.push(name);
    }
    if preview && ctx.looped {
        let twice = Audio { sr: x.sr, frames: [x.frames.as_slice(), x.frames.as_slice()].concat() };
        let name = format!("{base}.loop-preview.{}", audio::extension(&formats[0]));
        audio::encode(&twice, &out_dir.join(&name), &formats[0], &tags)?;
        files.push(name);
    }

    // Loudness is measured on what was written (lossy codecs move the peak); the rest on the samples.
    let written = audio::decode(&out_dir.join(&files[0])).and_then(|w| analyze::loudness(&w));
    let mut report = analyze::analyze(&x, Some(written), ctx.bar_samples(sr));
    let ran = |op: &str| applied.iter().any(|a| a == op);
    let lufs = ctx.lufs_target.unwrap_or(ctx.loudness_lufs);
    let tp = ctx.tp_target.unwrap_or(ctx.true_peak_db);
    let expected = if ran("fit") { ctx.duration.clone() } else { None };
    report["qc"] = json!(analyze::qc_notes(&report, ran("loudnorm").then_some(lufs), tp, expected.as_ref(), ctx.looped));
    report["issues"] = json!(analyze::inspect(&x, ctx.looped || loopable));
    Ok(json!({"files": files, "applied": applied, "notes": ctx.notes, "warnings": ctx.warnings, "report": report}))
}

/// What a finalize request asks for (all optional: the take's workflow and picked variant).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct FinalizeInput {
    pub variant: Option<i64>,
    pub workflow: Option<String>,
    pub steps: Option<Vec<Value>>,
    pub name: Option<String>,
    #[serde(default)]
    pub preview: bool,
    pub loudness: Option<Value>,
}

/// Finalize one variant of a take into `<take>/final/` and record it in `final.json`.
/// `take` is the library's view of the take (it carries `dir`, `variants`, `annotations`).
pub fn run_for_take(take: &Value, cfg: &Value, workflows: &Obj, input: &FinalizeInput) -> Result<Value, String> {
    let id = py_str(&take["id"]);
    let dir = Path::new(take["dir"].as_str().ok_or("take has no folder")?);
    let variants = take["variants"].as_array().cloned().unwrap_or_default();
    let picked = take["annotations"].get("picked").and_then(Value::as_i64).filter(|p| *p > 0);
    let variant = input.variant.or(picked).unwrap_or(1);
    let Some(m) = variants.iter().find(|v| v["index"].as_i64() == Some(variant)) else {
        let have: Vec<String> = variants.iter().map(|v| py_str(&v["index"])).collect();
        return Err(format!("take has no variant {variant} (have [{}])", have.join(", ")));
    };
    let spec = take.get("spec").cloned().unwrap_or(json!({}));
    let steps = match &input.steps {
        Some(s) => s.clone(),
        None => {
            let wf_id = input
                .workflow
                .clone()
                .filter(|w| !w.is_empty())
                .or_else(|| spec.get("workflow").and_then(Value::as_str).filter(|w| !w.is_empty()).map(String::from))
                .or_else(|| cfg["workflow"]["default"].as_str().map(String::from))
                .unwrap_or_default();
            workflow_steps(workflows.get(&wf_id))
        }
    };
    let title = take.get("title").filter(|t| truthy(t)).map_or(id.clone(), py_str);
    let base = input.name.clone().filter(|n| !n.trim().is_empty()).map_or_else(|| format!("{}_v{variant}", slugify(&title, 48)), |n| slugify(&n, 80));
    let src = dir.join(m["file"].as_str().ok_or("variant has no file")?);
    let loud = resolve_loudness(cfg, input.loudness.as_ref())?;
    let final_dir = dir.join("final");
    let target = Target { dir: &final_dir, base: &base, preview: input.preview };
    let result = run_steps(&src, &steps, &target, cfg, &spec, loud.as_ref())?;
    let mut record = json!({"variant": variant, "base": base, "created": chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()});
    if let (Some(r), Some(extra)) = (record.as_object_mut(), result.as_object()) {
        r.extend(extra.clone());
    }
    let path = dir.join("final.json");
    let mut finals: Vec<Value> = read_json(&path)?.and_then(|v| v.as_array().cloned()).unwrap_or_default();
    finals.retain(|f| f["base"].as_str() != Some(&base));
    finals.push(record.clone());
    write_json(&path, &Value::Array(finals)).map_err(|e| format!("cannot write final.json: {e}"))?;
    Ok(record)
}

/// Automatic check of every generated variant (`qc` on each). Never fails the take: a variant
/// that cannot be read gets no `qc` and the returned warning says why.
pub fn check_variants(out_dir: &Path, variants: &mut Value, loopable: bool) -> Option<String> {
    let mut problems = vec![];
    for v in variants.as_array_mut().into_iter().flatten() {
        let Some(file) = v["file"].as_str().map(String::from) else { continue };
        match audio::decode(&out_dir.join(&file)) {
            Ok(a) => v["qc"] = json!(analyze::inspect(&a, loopable)),
            Err(e) => problems.push(e),
        }
    }
    (!problems.is_empty()).then(|| format!("automatic audio check skipped: {}", problems.join("; ")))
}
