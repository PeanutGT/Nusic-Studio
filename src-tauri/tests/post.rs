//! A3 parity with the skill v3.7 (`golden/v3.7/qc`): measurements, the automatic check, the
//! loudness gain and every post-processing op, plus finalizing a take end to end.
//! Tolerances are the ones proposed in golden/README.md.

use nusic_studio_lib::studio::post::{analyze, audio, ops};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../golden/v3.7")
}

fn load(rel: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(golden_dir().join(rel)).unwrap()).unwrap()
}

fn cfg() -> Value {
    serde_json::from_str(&std::fs::read_to_string(golden_dir().join("defaults/config.json")).unwrap()).unwrap()
}

/// How far a number may drift, by field: EBU R128 meter values ±0.1, times ±0.01 s,
/// everything else within the rounding of the stored precision.
fn tolerance(key: &str) -> f64 {
    match key {
        "lufs" | "true_peak_db" | "lra" | "gain_db" => 0.1,
        "at" | "end" => 0.01,
        "rolloff95_hz" => 12.0, // one FFT bin
        _ => 0.0101,
    }
}

fn close(path: &str, key: &str, want: &Value, got: &Value, errs: &mut Vec<String>) {
    match (want, got) {
        (Value::Object(w), Value::Object(g)) => {
            for (k, wv) in w {
                close(&format!("{path}.{k}"), k, wv, g.get(k).unwrap_or(&Value::Null), errs);
            }
            for k in g.keys().filter(|k| !w.contains_key(*k)) {
                errs.push(format!("{path}.{k}: unexpected field"));
            }
        }
        (Value::Array(w), Value::Array(g)) if w.len() == g.len() => {
            for (i, (wv, gv)) in w.iter().zip(g).enumerate() {
                close(&format!("{path}[{i}]"), key, wv, gv, errs);
            }
        }
        (Value::Number(w), Value::Number(g)) => {
            let (w, g) = (w.as_f64().unwrap(), g.as_f64().unwrap());
            if (w - g).abs() > tolerance(key) {
                errs.push(format!("{path}: want {w}, got {g}"));
            }
        }
        _ if want == got => {}
        _ => errs.push(format!("{path}: want {want}, got {got}")),
    }
}

/// Equal apart from numbers, which may differ by `tol`.
fn same_text(a: &str, b: &str, tol: f64) -> bool {
    let words = |s: &str| s.split(' ').map(|w| w.trim_matches(|c: char| "();,".contains(c)).to_string()).collect::<Vec<_>>();
    let (wa, wb) = (words(a), words(b));
    wa.len() == wb.len()
        && wa.iter().zip(&wb).all(|(x, y)| match (x.parse::<f64>(), y.parse::<f64>()) {
            (Ok(p), Ok(q)) => (p - q).abs() <= tol,
            _ => x == y,
        })
}

fn check(id: &str, what: &str, want: &Value, got: &Value, errs: &mut Vec<String>) {
    let mut e = vec![];
    close(what, what, want, got, &mut e);
    errs.extend(e.into_iter().map(|m| format!("{id}: {m}")));
}

fn finish(errs: Vec<String>) {
    assert!(errs.is_empty(), "{} mismatches:\n{}", errs.len(), errs.join("\n"));
}

#[test]
fn measurements_and_checks_match_v3_7() {
    let doc = load("qc/cases.json");
    let meter: serde_json::Map<String, Value> =
        load("qc/r128.json")["cases"].as_array().unwrap().iter().map(|c| (c["id"].as_str().unwrap().to_string(), c["expected"].clone())).collect();
    let mut errs = vec![];
    for c in doc["cases"].as_array().unwrap() {
        let id = c["id"].as_str().unwrap();
        let input = &c["input"];
        let want = &c["expected"];
        let a = audio::decode(&golden_dir().join(c["audio"].as_str().unwrap())).unwrap();
        let bar = input["bar_samples"].as_u64().map(|b| b as usize);
        let loopable = input["loopable"].as_bool().unwrap();

        check(id, "inspect_audio", &want["inspect_audio"], &json!(analyze::inspect(&a, loopable)), &mut errs);
        let report = analyze::analyze(&a, Some(analyze::loudness(&a)), bar);
        // loudness against the standard meter (r128.json), the rest against v3.7 itself
        let r128 = &meter[id];
        for k in ["lufs", "lra", "true_peak_db"] {
            let got = report[k].as_f64().unwrap_or(-70.0); // below the gate: ffmpeg prints -70
            let w = r128[k].as_f64().unwrap();
            // LRA is only reported (no check uses it) and meters differ in how they treat the
            // first seconds and long silences, so it gets a wider margin
            let tol = if k == "lra" { 0.6 } else { 0.11 };
            let below = r128["lufs"] == json!(-70.0) || w < -70.0;
            if !below && (got - w).abs() > tol {
                errs.push(format!("{id}: {k} want {w} (standard meter), got {got}"));
            }
            // and never far from what v3.7's loudnorm read
            let old = want["analyze_array"][k].as_f64().unwrap_or(-70.0);
            if k != "lra" && old > -70.0 && (got - old).abs() > 0.5 {
                errs.push(format!("{id}: {k} {got} is far from v3.7's {old}"));
            }
        }
        let mut sample_stats = want["analyze_array"].clone();
        let mut ours = report.clone();
        for k in ["lufs", "lra", "true_peak_db"] {
            sample_stats.as_object_mut().unwrap().remove(k);
            ours.as_object_mut().unwrap().remove(k);
        }
        check(id, "analyze_array", &sample_stats, &ours, &mut errs);
        // qc notes from the golden report, so a 0.01 LU meter difference cannot flip a threshold
        let notes = analyze::qc_notes(&want["analyze_array"], Some(-16.0), -1.5, None, input["looped"].as_bool().unwrap());
        check(id, "qc_notes", &want["qc_notes"], &json!(notes), &mut errs);
        let b = analyze::brightness(&a.mono(), a.sr).map(|(hf, r)| json!({"hf_pct": hf, "rolloff95_hz": r}));
        check(id, "brightness", &want["brightness"], &json!(b), &mut errs);
        check(id, "seam_metrics", &want["seam_metrics"], &analyze::seam_metrics(&a, bar), &mut errs);

        let mut ctx = ops::Ctx::new(&cfg());
        let z = ops::loudnorm(a.clone(), &input["loudnorm"], &mut ctx).unwrap();
        let ratios: Vec<f64> = a
            .frames
            .iter()
            .zip(&z.frames)
            .flat_map(|(x, y)| (0..2).map(move |c| (x[c], y[c])))
            .filter(|(x, _)| x.abs() > 1e-4)
            .map(|(x, y)| (y / x).abs() as f64)
            .collect();
        let gain = if ratios.is_empty() { 0.0 } else { 20.0 * ratios[ratios.len() / 2].log10() };
        let got = json!({"gain_db": (gain * 1000.0).round() / 1000.0, "notes": ctx.notes, "warnings": ctx.warnings});
        // the note text carries numbers rounded to 0.1, so compare it only when the meter agrees closely
        // v3.7's gain rode on loudnorm's reading; ours must equal target - standard meter (capped
        // by the true peak the same way), and stay within 0.5 dB of v3.7's
        let w = &want["loudnorm"];
        let (target, ceiling) = (input["loudnorm"]["target_lufs"].as_f64().unwrap(), input["loudnorm"]["true_peak_db"].as_f64().unwrap());
        let expect = match r128["lufs"].as_f64().filter(|l| *l > -69.0) {
            None => 0.0,
            Some(l) => (target - l).min(ceiling - ops::TP_MARGIN_DB - r128["true_peak_db"].as_f64().unwrap()),
        };
        if (expect - gain).abs() > 0.11 || (w["gain_db"].as_f64().unwrap() - gain).abs() > 0.5 {
            errs.push(format!("{id}: loudnorm gain {gain:.3}, standard meter says {expect:.2}, v3.7 {}", w["gain_db"]));
        }
        // same messages; their numbers (rounded to 0.1) may differ by the meter difference
        for k in ["notes", "warnings"] {
            let (a, b) = (w[k].as_array().unwrap(), got[k].as_array().unwrap());
            if a.len() != b.len() || a.iter().zip(b).any(|(x, y)| !same_text(x.as_str().unwrap(), y.as_str().unwrap(), 0.45)) {
                errs.push(format!("{id}: loudnorm.{k} want {a:?}, got {b:?}"));
            }
        }
    }
    finish(errs);
}

#[test]
fn every_op_matches_v3_7() {
    let doc = load("qc/ops.json");
    let mut errs = vec![];
    for c in doc["cases"].as_array().unwrap() {
        let id = c["id"].as_str().unwrap();
        let a = audio::decode(&golden_dir().join(c["audio"].as_str().unwrap())).unwrap();
        let mut ctx = ops::Ctx::new(&cfg());
        ctx.bpm = c["ctx"]["bpm"].as_f64();
        ctx.duration = c["ctx"].get("duration").cloned();
        ctx.beats_per_bar = c["ctx"]["beats_per_bar"].as_u64().unwrap_or(4) as usize;
        let z = ops::apply(c["op"].as_str().unwrap(), a, &c["params"], &mut ctx).unwrap();
        let want = &c["expected"];
        let all = || z.frames.iter().flat_map(|f| f.iter().map(|v| *v as f64));
        let samples: serde_json::Map<String, Value> = want["samples"]
            .as_object()
            .unwrap()
            .keys()
            .map(|k| {
                let f = z.frames.get(k.parse::<usize>().unwrap()).copied().unwrap_or([f32::NAN; 2]);
                (k.clone(), json!([f[0] as f64, f[1] as f64]))
            })
            .collect();
        if want["frames"] != json!(z.len()) {
            errs.push(format!("{id}: frames want {}, got {}", want["frames"], z.len()));
            continue;
        }
        let sum: f64 = all().sum();
        let abs_sum: f64 = all().map(f64::abs).sum();
        // checksums over ~2M samples: allow float noise of 1e-6 per sample
        let n = 2.0 * z.len() as f64;
        for (k, got) in [("sum", sum), ("abs_sum", abs_sum)] {
            if (want[k].as_f64().unwrap() - got).abs() > 1e-6 * n + 1e-3 {
                errs.push(format!("{id}: {k} want {}, got {got}", want[k]));
            }
        }
        for (k, got) in &samples {
            for c in 0..2 {
                let (w, g) = (want["samples"][k][c].as_f64().unwrap(), got[c].as_f64().unwrap());
                if (w - g).abs() > 2e-6 {
                    errs.push(format!("{id}: sample {k}[{c}] want {w}, got {g}"));
                }
            }
        }
        check(id, "notes", &want["notes"], &json!(ctx.notes), &mut errs);
        check(id, "warnings", &want["warnings"], &json!(ctx.warnings), &mut errs);
        check(id, "loop", &want["loop"], &json!(ctx.looped), &mut errs);
    }
    finish(errs);
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nusic-post-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A take folder the way the queue leaves it: meta.json + one variant file.
fn fake_take(dir: &Path, signal: &str, spec: Value) -> Value {
    std::fs::copy(golden_dir().join(format!("audio/{signal}.flac")), dir.join("v1.flac")).unwrap();
    json!({"id": "t1", "title": "測試 Take", "dir": dir.to_string_lossy(), "spec": spec,
           "variants": [{"index": 1, "file": "v1.flac"}], "annotations": {}})
}

#[test]
fn finalize_runs_the_workflow_exports_and_records_final_json() {
    let dir = temp("finalize");
    let take = fake_take(&dir, "clean", json!({"style": "lofi", "duration": 12, "workflow": "video-score"}));
    let wf: Value = serde_json::from_str(&std::fs::read_to_string(golden_dir().join("defaults/workflows/video-score.json")).unwrap()).unwrap();
    let workflows = json!({"video-score": wf}).as_object().unwrap().clone();
    let rec = ops::run_for_take(&take, &cfg(), &workflows, &ops::FinalizeInput::default()).unwrap();

    assert_eq!(rec["applied"], json!(["trim_silence", "fit", "fade", "loudnorm"]));
    assert_eq!(rec["files"], json!(["測試-take_v1.wav", "測試-take_v1.mp3"]));
    for f in rec["files"].as_array().unwrap() {
        assert!(dir.join("final").join(f.as_str().unwrap()).is_file(), "{f}");
    }
    let report = &rec["report"];
    assert!((report["duration"].as_f64().unwrap() - 12.0).abs() < 0.01);
    assert!((report["lufs"].as_f64().unwrap() + 18.0).abs() < 0.3, "{report}");
    assert!(report["qc"].as_array().unwrap().is_empty(), "{report}");
    // the wav24 export reads back at the same level and length
    let back = audio::decode(&dir.join("final/測試-take_v1.wav")).unwrap();
    assert_eq!(back.len(), 12 * 48000);

    let finals: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("final.json")).unwrap()).unwrap();
    assert_eq!(finals.as_array().unwrap().len(), 1);
    // finalizing again under the same name replaces the record instead of piling up
    let again = ops::FinalizeInput { loudness: Some(json!("video")), ..Default::default() };
    let rec2 = ops::run_for_take(&take, &cfg(), &workflows, &again).unwrap();
    assert!((rec2["report"]["lufs"].as_f64().unwrap() + 14.0).abs() < 0.3, "{}", rec2["report"]);
    let finals: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("final.json")).unwrap()).unwrap();
    assert_eq!(finals.as_array().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn loopable_take_gets_a_loop_and_a_preview() {
    let dir = temp("loop");
    let take = fake_take(&dir, "periodic_loop", json!({"bpm": 110, "loopable": true}));
    let input = ops::FinalizeInput { steps: Some(ops::workflow_steps(None)), preview: true, ..Default::default() };
    let rec = ops::run_for_take(&take, &cfg(), &Default::default(), &input).unwrap();
    assert_eq!(rec["applied"], json!(["loop", "loudnorm"]));
    assert_eq!(rec["files"], json!(["測試-take_v1.flac", "測試-take_v1.loop-preview.flac"]));
    let one = audio::decode(&dir.join("final/測試-take_v1.flac")).unwrap();
    let two = audio::decode(&dir.join("final/測試-take_v1.loop-preview.flac")).unwrap();
    assert_eq!(two.len(), 2 * one.len());
    // a bare 220 Hz tone has no highs; looped, it is never "cut off"
    assert_eq!(rec["report"]["issues"].as_array().unwrap().iter().map(|i| i["code"].clone()).collect::<Vec<_>>(), vec![json!("dull")]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_requests_explain_themselves() {
    let dir = temp("bad");
    let take = fake_take(&dir, "clean", json!({}));
    let wfs = Default::default();
    let e = ops::run_for_take(&take, &cfg(), &wfs, &ops::FinalizeInput { variant: Some(3), ..Default::default() }).unwrap_err();
    assert_eq!(e, "take has no variant 3 (have [1])");
    let e = ops::run_for_take(&take, &cfg(), &wfs, &ops::FinalizeInput { loudness: Some(json!("loud")), ..Default::default() }).unwrap_err();
    assert!(e.starts_with("unknown loudness 'loud'. Presets: video, game, background"), "{e}");
    let steps = Some(vec![json!({"op": "reverb"})]);
    let e = ops::run_for_take(&take, &cfg(), &wfs, &ops::FinalizeInput { steps, ..Default::default() }).unwrap_err();
    assert_eq!(e, "unknown step 'reverb'");
    let steps = Some(vec![json!({"op": "export", "formats": ["opus"]})]);
    let e = ops::run_for_take(&take, &cfg(), &wfs, &ops::FinalizeInput { steps, ..Default::default() }).unwrap_err();
    assert!(e.starts_with("unknown export format 'opus'"), "{e}");
    assert!(!dir.join("final.json").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_variant_gets_checked_after_generation() {
    let dir = temp("check");
    std::fs::copy(golden_dir().join("audio/click.flac"), dir.join("a.flac")).unwrap();
    std::fs::copy(golden_dir().join("audio/cut_off.flac"), dir.join("b.flac")).unwrap();
    let mut variants = json!([{"index": 1, "file": "a.flac"}, {"index": 2, "file": "b.flac"}, {"index": 3, "file": "missing.flac"}]);
    let warning = ops::check_variants(&dir, &mut variants, false).unwrap();
    assert!(warning.starts_with("automatic audio check skipped: could not decode missing.flac"), "{warning}");
    assert_eq!(variants[0]["qc"][0]["code"], "clicks");
    assert_eq!(variants[1]["qc"][0]["code"], "cut_off");
    assert!(variants[2].get("qc").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn library_shows_checks_and_exports_after_finalize() {
    use nusic_studio_lib::studio::{api::Studio, takes::Library};
    let ws = temp("library");
    let take = ws.join("output").join("20261110-120000-rain");
    std::fs::create_dir_all(&take).unwrap();
    std::fs::copy(golden_dir().join("audio/click.flac"), take.join("v1.flac")).unwrap();
    let meta = json!({"id": "20261110-120000-rain", "title": "rain", "created": "2026-11-10T12:00:00",
                      "spec": {"style": "lofi", "duration": 20, "workflow": "quick"},
                      "variants": [{"index": 1, "file": "v1.flac", "qc": [{"code": "clicks", "at": 5.0, "count": 1}]}]});
    std::fs::write(take.join("meta.json"), meta.to_string()).unwrap();
    let studio = Studio { defaults_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/defaults") };
    let lib = Library { ws: ws.clone(), out_root: ws.join("output") };

    let rec = studio.finalize(&ws, &lib, "20261110-120000-rain", &ops::FinalizeInput { loudness: Some(json!("background")), ..Default::default() }).unwrap();
    assert_eq!(rec["files"], json!(["rain_v1.flac"]));
    let row = &lib.list()[0];
    assert_eq!(row["checks"][0]["qc"][0]["code"], "clicks");
    let export = &row["exports"][0];
    assert_eq!(export["files"], json!(["rain_v1.flac"]));
    assert!((export["lufs"].as_f64().unwrap() + 18.0).abs() < 0.3, "{export}");
    assert!(studio.finalize(&ws, &lib, "nope", &Default::default()).unwrap_err().contains("unknown take 'nope'"));

    let overview = studio.overview(&ws, "low").unwrap();
    assert_eq!(overview["loudness_presets"][0]["id"], "video");
    assert_eq!(overview["workflow_steps"]["sketch"], json!([{"op": "export", "formats": ["flac"]}]));
    let _ = std::fs::remove_dir_all(&ws);
}
