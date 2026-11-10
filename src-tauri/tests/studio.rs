//! The app-facing studio API: plan preview, per-take settings, workspace overrides, overview.

use nusic_studio_lib::studio::api::Studio;
use nusic_studio_lib::studio::plan::PlanInput;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn studio() -> Studio {
    Studio { defaults_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("resources").join("defaults") }
}

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nusic-studio-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn input(style: &str, engine_params: Value) -> PlanInput {
    PlanInput { style: style.into(), engine_params: engine_params.as_object().cloned(), ..Default::default() }
}

#[test]
fn plan_preview_applies_per_take_settings_as_the_request_layer() {
    let ws = workspace("plan");
    let p = studio().plan(&ws, &input("rpg-town", json!({"inference_steps": "12", "shift": 2.5})), "low").unwrap();
    assert_eq!(p["params"]["inference_steps"], 12);
    assert_eq!(p["sources"]["inference_steps"], "request");
    assert_eq!(p["params"]["shift"], 2.5);
    assert_eq!(p["spec"]["engine_params"], json!({"inference_steps": 12, "shift": 2.5}));
    assert_eq!(p["tier"], "low");
    assert!(p["spec"]["caption"].as_str().unwrap().contains("instrumental"));
}

#[test]
fn bad_per_take_settings_are_refused() {
    let ws = workspace("bad");
    let e = studio().spec(&ws, &input("lofi", json!({"inference_steps": 999, "max_duration": 10}))).unwrap_err();
    assert!(e.contains("inference_steps must be between"), "{e}");
    assert!(e.contains("max_duration is not a per-take setting"), "{e}");
    assert!(studio().spec(&ws, &input("no-such-style", json!({}))).unwrap_err().starts_with("unknown style"));
}

#[test]
fn workspace_overrides_show_in_the_settings_table_and_can_be_removed() {
    let ws = workspace("set");
    let s = studio();
    let problems = s.set_config(&ws, "quality.presets.standard.inference_steps", json!(16).into()).unwrap();
    assert!(problems.is_empty(), "{problems:?}");
    let t = s.knob_table(&ws, Some("standard"), "low").unwrap();
    let row = t["knobs"].as_array().unwrap().iter().find(|k| k["key"] == "inference_steps").unwrap().clone();
    assert_eq!(row["value"], 16);
    assert_eq!(row["from_user"], true);
    assert_eq!(row["path"], "quality.presets.standard.inference_steps");

    let problems = s.set_config(&ws, "generation.shift", json!(-1).into()).unwrap();
    assert!(problems.iter().any(|p| p.contains("generation.shift")), "{problems:?}");
    s.set_config(&ws, "generation.shift", None).unwrap();
    s.set_config(&ws, "quality.presets.standard.inference_steps", None).unwrap();
    let t = s.knob_table(&ws, Some("standard"), "low").unwrap();
    let row = t["knobs"].as_array().unwrap().iter().find(|k| k["key"] == "inference_steps").unwrap().clone();
    assert_eq!((row["value"].clone(), row["from_user"].clone()), (json!(8), json!(false)));
    assert!(s.set_config(&ws, "_doc", json!(1).into()).is_err());
}

#[test]
fn overview_lists_everything_the_create_screen_needs() {
    let ws = workspace("overview");
    let o = studio().overview(&ws, "low").unwrap();
    assert_eq!(o["styles"].as_array().unwrap().len(), 18);
    assert!(o["moods"].as_array().unwrap().iter().any(|m| m["id"] == "calm"));
    assert!(o["instruments"].as_array().unwrap().iter().any(|i| i["id"] == "erhu"));
    assert_eq!(o["default_quality"], "standard");
    assert_eq!(o["tier_caps"]["max_duration"], 120);
    assert_eq!(o["problems"], json!([]));
}
