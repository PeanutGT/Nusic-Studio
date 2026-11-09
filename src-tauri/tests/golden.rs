//! Golden baseline from the music-studio skill v3.7 (see golden/README.md).
//! A0 only checks the data is complete and readable from Rust; the A2 / A3
//! ports compare their output against these files.

use serde_json::Value;
use std::path::PathBuf;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../golden/v3.7")
}

fn load(rel: &str) -> Value {
    let path = golden_dir().join(rel);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn cases(doc: &Value) -> &Vec<Value> {
    doc["cases"].as_array().expect("`cases` array")
}

#[test]
fn manifest_names_skill_v3_7() {
    let m = load("manifest.json");
    assert!(m["skill_version"].as_str().unwrap().starts_with("3.7."));
}

#[test]
fn plan_cases_cover_every_builtin_style() {
    let doc = load("plan/cases.json");
    let styles: Vec<String> = std::fs::read_dir(golden_dir().join("defaults/styles"))
        .unwrap()
        .map(|e| e.unwrap().path().file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(!styles.is_empty());
    for style in &styles {
        assert!(
            cases(&doc).iter().any(|c| c["input"]["style"] == style.as_str()),
            "no plan case for style {style}"
        );
    }
    for c in cases(&doc) {
        assert!(c["expected"]["caption"].is_string(), "{}", c["id"]);
    }
}

#[test]
fn params_and_key_picks_present() {
    assert!(!cases(&load("plan/params.json")).is_empty());
    for c in cases(&load("plan/keys.json")) {
        let options = c["options"].as_array().unwrap();
        assert!(options.contains(&c["expected"]), "{}", c["seed_string"]);
    }
}

#[test]
fn every_qc_case_has_its_audio_file() {
    let doc = load("qc/cases.json");
    assert_eq!(doc["sample_rate"], 48000);
    for c in cases(&doc) {
        let audio = golden_dir().join("qc").join("..").join(c["audio"].as_str().unwrap());
        assert!(audio.is_file(), "missing {}", audio.display());
        assert!(c["expected"]["inspect_audio"].is_array(), "{}", c["id"]);
    }
}
