//! The JSON-lines protocol spoken by `resources/ace_worker.py --serve`.
//! One request per stdin line, one event per stdout line. Anything else the
//! worker prints goes to stderr. See the docstring at the top of ace_worker.py.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // parsed once per line and consumed at once
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
        lm_metadata: Value,
        #[serde(default)]
        time_costs: Value,
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
            Event::Result { variants, warnings, elapsed_s, effective, lm_metadata, .. } => {
                assert_eq!(lm_metadata, Value::Null);
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
}
