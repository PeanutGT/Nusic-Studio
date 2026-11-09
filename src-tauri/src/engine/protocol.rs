//! The JSON-lines protocol spoken by `resources/ace_worker.py --serve`.
//! One request per stdin line, one event per stdout line. Anything else the
//! worker prints goes to stderr. See the docstring at the top of ace_worker.py.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Ready {
        #[serde(default)]
        models_loaded: bool,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        vram_total_gb: Option<f64>,
        #[serde(default)]
        vram_free_gb: Option<f64>,
    },
    Phase {
        name: String,
        #[serde(default)]
        pct: f64,
        #[serde(default)]
        detail: String,
    },
    Warn {
        message: String,
        #[serde(default)]
        code: Option<String>,
    },
    Retry {
        #[serde(default)]
        reason: String,
    },
    Result {
        #[serde(default)]
        variants: Vec<Variant>,
        #[serde(default)]
        effective: Value,
        #[serde(default)]
        warnings: Vec<String>,
        #[serde(default)]
        elapsed_s: Option<f64>,
    },
    Error {
        code: String,
        message: String,
        #[serde(default)]
        hint: String,
    },
    /// Events this app does not know yet; ignored instead of treated as garbage.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Variant {
    pub index: u32,
    pub file: String,
    pub seed: i64,
    #[serde(default)]
    pub sample_rate: Option<u32>,
}

/// Parse one stdout line. `None` for blank lines, stray prints and broken JSON.
pub fn parse_line(line: &str) -> Option<Event> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    serde_json::from_str(line).ok()
}

/// What the user asks for in A1's minimal screen.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GenerateInput {
    pub caption: String,
    pub duration: f64,
    /// Negative = random.
    #[serde(default = "random_seed")]
    pub seed: i64,
}

fn random_seed() -> i64 {
    -1
}

/// Build a `generate` request with the v3.7 "low" tier defaults
/// (turbo, 8 steps, no LM, no quantisation, both offload flags), one variant.
/// The worker's own VRAM guard still adapts it to the card. A2 replaces these
/// fixed values with the ported config / quality presets.
pub fn generate_request(job: &str, input: &GenerateInput, seed: i64, ace_dir: &Path, out_dir: &Path) -> Value {
    json!({
        "op": "generate",
        "job": job,
        "ace_step_dir": ace_dir.to_string_lossy(),
        "checkpoints_dir": ace_dir.join("checkpoints").to_string_lossy(),
        "params": {
            "dit_model": "acestep-v15-turbo",
            "lm_model": null,
            "thinking": false,
            "inference_steps": 8,
            "guidance_scale": 7.0,
            "shift": 3.0,
            "duration": input.duration,
            "batch_size": 1,
            "quantization": null,
            "offload_to_cpu": true,
            "offload_dit_to_cpu": true,
            "audio_format": "flac",
            "instrumental": true
        },
        "tier_dit_model": "acestep-v15-turbo",
        "explicit_keys": [],
        "vram_guard": true,
        "caption": input.caption,
        "instrumental": true,
        "lyrics": "",
        "seeds": [seed],
        "out_dir": out_dir.to_string_lossy(),
        "warnings": []
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ready_after_job() {
        let ev = parse_line(r#"{"event":"ready","models_loaded":true,"model":"acestep-v15-turbo","vram_total_gb":4.0,"vram_free_gb":3.21}"#);
        assert_eq!(
            ev,
            Some(Event::Ready {
                models_loaded: true,
                model: Some("acestep-v15-turbo".into()),
                vram_total_gb: Some(4.0),
                vram_free_gb: Some(3.21)
            })
        );
    }

    #[test]
    fn parses_startup_ready_without_vram() {
        let ev = parse_line(r#"{"event": "ready", "models_loaded": false}"#);
        assert_eq!(ev, Some(Event::Ready { models_loaded: false, model: None, vram_total_gb: None, vram_free_gb: None }));
    }

    #[test]
    fn parses_phase_with_job_field() {
        let ev = parse_line(r#"{"event":"phase","name":"diffusing","pct":0.55,"detail":"","job":"j1"}"#);
        assert_eq!(ev, Some(Event::Phase { name: "diffusing".into(), pct: 0.55, detail: String::new() }));
    }

    #[test]
    fn parses_error_with_hint() {
        let ev = parse_line(r#"{"event":"error","code":"oom","message":"GPU ran out of memory","hint":"Close other GPU apps"}"#);
        assert_eq!(
            ev,
            Some(Event::Error { code: "oom".into(), message: "GPU ran out of memory".into(), hint: "Close other GPU apps".into() })
        );
    }

    #[test]
    fn parses_result_variants() {
        let ev = parse_line(
            r#"{"event":"result","ok":true,"variants":[{"index":1,"file":"v1.flac","seed":42,"sample_rate":48000}],"effective":{"device":"cuda"},"warnings":["w"],"elapsed_s":17.2,"lm_metadata":null}"#,
        )
        .unwrap();
        match ev {
            Event::Result { variants, warnings, elapsed_s, effective } => {
                assert_eq!(variants, vec![Variant { index: 1, file: "v1.flac".into(), seed: 42, sample_rate: Some(48000) }]);
                assert_eq!(warnings, vec!["w".to_string()]);
                assert_eq!(elapsed_s, Some(17.2));
                assert_eq!(effective["device"], "cuda");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn unknown_event_is_other_and_noise_is_none() {
        assert_eq!(parse_line(r#"{"event":"heartbeat"}"#), Some(Event::Other));
        assert_eq!(parse_line("Loading checkpoint shards: 100%"), None);
        assert_eq!(parse_line("{not json"), None);
        assert_eq!(parse_line(""), None);
    }

    #[test]
    fn request_carries_caption_seed_and_low_tier_defaults() {
        let input = GenerateInput { caption: "calm piano".into(), duration: 30.0, seed: 7 };
        let req = generate_request("j1", &input, 7, Path::new("/ace"), Path::new("/ws/output/t1"));
        assert_eq!(req["op"], "generate");
        assert_eq!(req["job"], "j1");
        assert_eq!(req["caption"], "calm piano");
        assert_eq!(req["seeds"], json!([7]));
        assert_eq!(req["params"]["duration"], 30.0);
        assert_eq!(req["params"]["batch_size"], 1);
        assert_eq!(req["params"]["inference_steps"], 8);
        assert_eq!(req["params"]["offload_dit_to_cpu"], true);
        assert_eq!(req["params"]["quantization"], Value::Null);
        assert_eq!(req["checkpoints_dir"], Path::new("/ace").join("checkpoints").to_string_lossy().as_ref());
    }
}
