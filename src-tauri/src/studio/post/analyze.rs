//! Measurements and the automatic quality check (port of v3.7 `postprocess.py`:
//! `analyze_array`, `seam_metrics`, `brightness`, `inspect_audio`, `qc`).
//!
//! Loudness is EBU R128 / ITU BS.1770 from the `ebur128` crate instead of ffmpeg's `loudnorm`
//! meter; both follow the same standard, the golden tests hold them within 0.1 LU / dB.
//! The other checks are heuristics tuned to flag what a listener would notice; the ear decides.

use super::audio::Audio;
use crate::studio::json::py_float;
use serde::Serialize;
use serde_json::{Value, json};

/// Round like the skill's `round(x, d)` (ties to even).
pub fn round(x: f64, d: i32) -> f64 {
    let m = 10f64.powi(d);
    (x * m).round_ties_even() / m
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

fn db_rms(frames: &[[f32; 2]]) -> f64 {
    let ms = frames.iter().map(|f| (f[0] as f64).powi(2) + (f[1] as f64).powi(2)).sum::<f64>() / (2 * frames.len().max(1)) as f64;
    20.0 * (ms.sqrt() + 1e-9).log10()
}

/// Integrated loudness (LUFS), true peak (dBTP) and loudness range (LU). `None` for silence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Loudness {
    pub lufs: Option<f64>,
    pub true_peak_db: Option<f64>,
    pub lra: Option<f64>,
}

pub fn loudness(a: &Audio) -> Result<Loudness, String> {
    use ebur128::{EbuR128, Mode};
    let err = |e: ebur128::Error| format!("loudness meter: {e}");
    let mut m = EbuR128::new(2, a.sr, Mode::I | Mode::S | Mode::TRUE_PEAK).map_err(err)?;
    // Short-term loudness every 100 ms (EBU Tech 3342 asks for >= 10 Hz); the crate's own LRA
    // takes one block per second, which reads several LU off a standard meter on short clips.
    let step = (a.sr as usize / 10).max(1);
    let mut short_term = vec![];
    for (i, chunk) in a.frames.chunks(step).enumerate() {
        let flat: Vec<f32> = chunk.iter().flat_map(|f| *f).collect();
        m.add_frames_f32(&flat).map_err(err)?;
        if (i + 1) * step >= 3 * a.sr as usize && chunk.len() == step {
            short_term.push(m.loudness_shortterm().map_err(err)?);
        }
    }
    let fin = |v: f64| v.is_finite().then(|| round(v, 2));
    let tp = (0..2).filter_map(|c| m.true_peak(c).ok()).fold(0.0f64, f64::max);
    Ok(Loudness {
        lufs: m.loudness_global().ok().and_then(fin),
        true_peak_db: if tp > 0.0 { fin(20.0 * tp.log10()) } else { None },
        lra: loudness_range(short_term).and_then(fin),
    })
}

/// EBU Tech 3342: gate short-term values at -70 LUFS and 20 LU below their power mean,
/// LRA = 95th - 10th percentile.
fn loudness_range(st: Vec<f64>) -> Option<f64> {
    let mut gated: Vec<f64> = st.into_iter().filter(|l| *l > -70.0).collect();
    if gated.is_empty() {
        return None;
    }
    let mean_power = gated.iter().map(|l| 10f64.powf(l / 10.0)).sum::<f64>() / gated.len() as f64;
    let rel = 10.0 * mean_power.log10() - 20.0;
    gated.retain(|l| *l >= rel);
    gated.sort_by(f64::total_cmp);
    let at = |p: f64| gated[((gated.len() - 1) as f64 * p).round() as usize];
    Some(at(0.95) - at(0.10))
}

/// How audible would the wrap-around be if this file were looped? (see v3.7 `seam_metrics`)
/// `bar` = samples per bar when the tempo is known: interior bar lines are then the reference.
pub fn seam_metrics(a: &Audio, bar: Option<usize>) -> Value {
    let x = &a.frames;
    let sr = a.sr as usize;
    let mut w = sr;
    if let Some(b) = bar {
        w = w.min(b / 2);
    }
    if x.len() < 4 * w || w < sr / 20 {
        return json!({"click_ratio": null, "level_jump_db": null, "typical_step_db": null, "seam_vs_typical": null});
    }
    let step = x.windows(2).map(|p| ((p[1][0] - p[0][0]).abs() + (p[1][1] - p[0][1]).abs()) as f64).sum::<f64>()
        / (2 * (x.len() - 1)) as f64;
    let step = if step == 0.0 { 1e-9 } else { step };
    let (first, last) = (x[0], x[x.len() - 1]);
    let jump = (((first[0] - last[0]).abs() + (first[1] - last[1]).abs()) / 2.0) as f64;
    let mut steps: Vec<f64> = match bar {
        Some(b) if x.len() >= 3 * b => (b..x.len() - w).step_by(b).map(|i| (db_rms(&x[i - w..i]) - db_rms(&x[i..i + w])).abs()).collect(),
        _ => {
            let levels: Vec<f64> = (0..x.len() / w).map(|i| db_rms(&x[i * w..(i + 1) * w])).collect();
            levels.windows(2).map(|p| (p[1] - p[0]).abs()).collect()
        }
    };
    let typical = if steps.is_empty() { 0.5 } else { median(&mut steps) }.max(0.5);
    let seam_db = (db_rms(&x[..w]) - db_rms(&x[x.len() - w..])).abs();
    json!({"click_ratio": round(jump / step, 2), "level_jump_db": round(seam_db, 2),
           "typical_step_db": round(typical, 2), "seam_vs_typical": round(seam_db / typical, 2)})
}

/// Average spectrum of the audible frames: % of energy above 4 kHz and the 95 % rolloff.
pub fn brightness(mono: &[f32], sr: u32) -> Option<(f64, i64)> {
    const N: usize = 4096;
    let hop = N.max(sr as usize / 2);
    if mono.len() <= N {
        return None;
    }
    let hann: Vec<f64> = (0..N).map(|k| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * k as f64 / (N - 1) as f64).cos()).collect();
    let fft = rustfft::FftPlanner::<f64>::new().plan_fft_forward(N);
    let bins = N / 2 + 1;
    let mut power = vec![0.0f64; bins];
    let mut used = 0usize;
    let mut buf = vec![rustfft::num_complex::Complex::new(0.0, 0.0); N];
    for s in (0..mono.len() - N).step_by(hop) {
        let frame: Vec<f64> = (0..N).map(|k| mono[s + k] as f64 * hann[k]).collect();
        let rms = (frame.iter().map(|v| v * v).sum::<f64>() / N as f64).sqrt();
        if rms <= 10f64.powf(-50.0 / 20.0) {
            continue;
        }
        for (b, v) in buf.iter_mut().zip(&frame) {
            *b = rustfft::num_complex::Complex::new(*v, 0.0);
        }
        fft.process(&mut buf);
        for (p, c) in power.iter_mut().zip(&buf[..bins]) {
            *p += c.norm_sqr();
        }
        used += 1;
    }
    if used == 0 {
        return None;
    }
    let total = power.iter().sum::<f64>();
    let total = if total == 0.0 { 1e-12 } else { total };
    let freq = |k: usize| k as f64 * sr as f64 / N as f64;
    let hf: f64 = power.iter().enumerate().filter(|(k, _)| freq(*k) >= 4000.0).map(|(_, p)| p).sum();
    let mut cum = 0.0;
    let idx = power.iter().position(|p| {
        cum += p / total;
        cum >= 0.95
    });
    let k = idx.unwrap_or(bins).min(bins - 1);
    Some((round(100.0 * hf / total, 2), freq(k).round_ties_even() as i64))
}

pub const DULL_HF_PCT: f64 = 1.0; // healthy mixes sit around 5-20 % / 5-12 kHz
pub const DULL_ROLLOFF_HZ: f64 = 3000.0;

/// Automatic check of a freshly generated variant. Issues carry a time in seconds:
/// clipping, clicks, silence (an interior gap >= 2 s), cut_off (still at full level in the last
/// 0.1 s), near_silent, low_volume, dull (almost no energy above 4 kHz: sounds muffled).
/// Loopable takes skip cut_off: they are meant to keep going.
pub fn inspect(a: &Audio, loopable: bool) -> Vec<Value> {
    let sr = a.sr as usize;
    let x = &a.frames;
    let peak = x.iter().flat_map(|f| f.iter()).fold(0.0f32, |m, v| m.max(v.abs()));
    if x.is_empty() || (peak as f64) < 10f64.powf(-40.0 / 20.0) {
        return vec![json!({"code": "near_silent", "at": 0.0})];
    }
    let mut issues = vec![];

    // runs of >= 3 samples pinned at full scale, per channel
    let (mut runs, mut first) = (0usize, None::<usize>);
    for ch in 0..2 {
        let mut i = 0;
        while i < x.len() {
            if x[i][ch].abs() >= 0.999 {
                let s = i;
                while i < x.len() && x[i][ch].abs() >= 0.999 {
                    i += 1;
                }
                if i - s >= 3 {
                    runs += 1;
                    first = Some(first.map_or(s, |f: usize| f.min(s)));
                }
            } else {
                i += 1;
            }
        }
    }
    if let (true, Some(f)) = (runs > 0, first) {
        issues.push(json!({"code": "clipping", "at": round(f as f64 / sr as f64, 2), "count": runs}));
    }

    let mono = a.mono();
    if let Some(issue) = clicks(&mono, sr) {
        issues.push(issue);
    }

    let win = (sr / 10).max(1); // 100 ms
    let nw = mono.len() / win;
    if nw >= 5 {
        let db: Vec<f64> = mono
            .chunks_exact(win)
            .take(nw)
            .map(|c| 20.0 * ((c.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / win as f64).sqrt() + 1e-9).log10())
            .collect();
        let quiet: Vec<bool> = db.iter().map(|d| *d < -60.0).collect();
        let mut loud: Vec<f64> = db.iter().zip(&quiet).filter(|(_, q)| !**q).map(|(d, _)| *d).collect();
        let reference = if loud.is_empty() { -120.0 } else { median(&mut loud) };
        let mut i = 0;
        while i < nw {
            if !quiet[i] {
                i += 1;
                continue;
            }
            let mut j = i;
            while j < nw && quiet[j] {
                j += 1;
            }
            // a gap followed only by faint noise floor up to the end is tail silence, not mid-track
            let tail = j >= nw || db[j..].iter().cloned().fold(f64::MIN, f64::max) < reference - 24.0;
            if i > 0 && !tail && (j - i) as f64 * 0.1 >= 2.0 {
                issues.push(json!({"code": "silence", "at": round(i as f64 * 0.1, 2), "end": round(j as f64 * 0.1, 2)}));
                break;
            }
            i = j;
        }
        if !loopable && db[nw - 1] > reference - 6.0 {
            issues.push(json!({"code": "cut_off", "at": round(mono.len() as f64 / sr as f64 - 0.1, 2)}));
        }
        if reference < -32.0 {
            issues.push(json!({"code": "low_volume", "at": 0.0, "rms_db": round(reference, 1)}));
        }
    }
    if let Some((hf, rolloff)) = brightness(&mono, a.sr)
        && hf < DULL_HF_PCT
        && (rolloff as f64) < DULL_ROLLOFF_HZ
    {
        issues.push(json!({"code": "dull", "at": 0.0, "hf_pct": hf, "rolloff95_hz": rolloff}));
    }
    issues
}

/// A click: a 1 ms spike 10x the surrounding 50 ms, gone again 3 ms either side.
/// Kept in `f32` like the numpy original so thresholds land on the same frames.
fn clicks(mono: &[f32], sr: usize) -> Option<Value> {
    let frame = (sr / 1000).max(1);
    if mono.len() < 3 {
        return None;
    }
    let d1: Vec<f32> = mono.windows(2).map(|p| p[1] - p[0]).collect();
    let d2: Vec<f32> = d1.windows(2).map(|p| (p[1] - p[0]).abs()).collect();
    let nf = d2.len() / frame;
    if nf <= 60 {
        return None;
    }
    let fm: Vec<f32> = d2.chunks_exact(frame).take(nf).map(|c| c.iter().cloned().fold(f32::MIN, f32::max)).collect();
    let padded: Vec<f32> = (0..nf + 50).map(|i| fm[i.saturating_sub(25).min(nf - 1)]).collect();
    let mut win = [0f32; 51];
    let med: Vec<f32> = (0..nf)
        .map(|i| {
            win.copy_from_slice(&padded[i..i + 51]);
            win.sort_by(f32::total_cmp);
            win[25].max(1e-6)
        })
        .collect();
    let at = |i: isize| if i < 0 || i as usize >= nf { f32::INFINITY } else { fm[i as usize] };
    let idx: Vec<usize> = (0..nf)
        .filter(|&i| {
            let (m, ii) = (med[i], i as isize);
            fm[i] > 10.0 * m && fm[i] > 0.05 && at(ii - 3) < 3.0 * m && at(ii + 3) < 3.0 * m
        })
        .collect();
    let first = *idx.first()?;
    let events = 1 + idx.windows(2).filter(|p| p[1] - p[0] > 5).count(); // neighbouring frames are one click
    Some(json!({"code": "clicks", "at": round((first * frame) as f64 / sr as f64, 2), "count": events}))
}

/// The measurement report written next to every exported file (v3.7 `analyze_array`).
pub fn analyze(a: &Audio, loud: Option<Result<Loudness, String>>, bar: Option<usize>) -> Value {
    let sr = a.sr as f64;
    let x = &a.frames;
    let all = || x.iter().flat_map(|f| f.iter());
    let peak = all().fold(0.0f32, |m, v| m.max(v.abs())) as f64;
    let clipped = all().filter(|v| v.abs() >= 0.999).count();
    let dc = all().map(|v| *v as f64).sum::<f64>() / (2 * x.len().max(1)) as f64;
    let thr = 10f64.powf(-50.0 / 20.0) as f32;
    let loud_at = |f: &[f32; 2]| f[0].abs().max(f[1].abs()) > thr;
    let head = x.iter().position(loud_at);
    let tail = x.iter().rposition(loud_at);
    let mut out = json!({
        "duration": round(x.len() as f64 / sr, 3),
        "sample_rate": a.sr,
        "peak_dbfs": if peak > 0.0 { json!(round(20.0 * peak.log10(), 2)) } else { Value::Null },
        "clipped_samples": clipped,
        "dc_offset": round(dc, 5),
        "head_silence_s": head.map(|h| round(h as f64 / sr, 2)),
        "tail_silence_s": tail.map(|t| round((x.len() - 1 - t) as f64 / sr, 2)),
        "seam": seam_metrics(a, bar),
    });
    match loud {
        Some(Ok(l)) => {
            out["lufs"] = json!(l.lufs);
            out["true_peak_db"] = json!(l.true_peak_db);
            out["lra"] = json!(l.lra);
        }
        Some(Err(e)) => out["loudness_error"] = json!(e),
        None => {}
    }
    out
}

/// What the report should warn about (v3.7 `qc`). `lufs_target` is only checked when a
/// loudness step ran; `expected_duration` only when a fit step did.
pub fn qc_notes(report: &Value, lufs_target: Option<f64>, tp_target: f64, expected_duration: Option<&Value>, looped: bool) -> Vec<String> {
    let mut notes = vec![];
    let f = |k: &str| report.get(k).and_then(Value::as_f64);
    if let (Some(l), Some(t)) = (f("lufs"), lufs_target)
        && (l - t).abs() > 1.0
    {
        notes.push(format!("loudness {l:.1} LUFS is off target {}", py_float(t)));
    }
    if let Some(tp) = f("true_peak_db")
        && tp > tp_target + 0.2
    {
        notes.push(format!("true peak {tp:.1} dB above ceiling {}", py_float(tp_target)));
    }
    let clipped = report.get("clipped_samples").and_then(Value::as_u64).unwrap_or(0);
    if clipped > 0 {
        notes.push(format!("{clipped} clipped samples"));
    }
    if let Some(h) = f("head_silence_s").filter(|h| *h > 1.0) {
        notes.push(format!("{}s of silence at the start", py_float(h)));
    }
    if let Some(t) = f("tail_silence_s").filter(|t| *t > 3.0) {
        notes.push(format!("{}s of silence at the end", py_float(t)));
    }
    if let (Some(want), Some(d)) = (expected_duration.and_then(Value::as_f64).filter(|w| *w != 0.0), f("duration"))
        && (d - want).abs() > 0.5
    {
        let want = crate::studio::json::py_str(expected_duration.unwrap_or(&Value::Null));
        notes.push(format!("duration {}s differs from the requested {want}s", py_float(d)));
    }
    if f("dc_offset").unwrap_or(0.0).abs() > 0.01 {
        notes.push("noticeable DC offset".into());
    }
    let seam = &report["seam"];
    if let (true, Some(click)) = (looped, seam["click_ratio"].as_f64()) {
        if click > 4.0 {
            notes.push(format!("loop seam may click (click_ratio {}); audition the loop preview", py_float(click)));
        }
        if let Some(s) = seam["seam_vs_typical"].as_f64().filter(|s| *s > 4.0) {
            notes.push(format!("loop seam changes level {}x more than the music does; audition it", py_float(s)));
        }
    }
    notes
}
