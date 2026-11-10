//! Request planning: style + moods + description -> a portable track spec with its ACE-Step caption
//! (port of v3.7 `msconfig.build_spec` and the lexicon helpers). Output must match v3.7 exactly;
//! `tests/golden.rs` checks it against `golden/v3.7/plan`.

use super::catalog::{Catalog, Collection};
use super::json::{Obj, num, opt_truthy, py_str, strs, truthy};
use super::pyrandom::PyRandom;
use serde::Deserialize;
use serde_json::{Value, json};

/// What a person asks for. Numbers stay JSON values so `120` and `120.0` survive as v3.7 wrote them.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlanInput {
    pub style: String,
    pub description: String,
    pub moods: Vec<String>,
    pub title: Option<String>,
    pub duration: Option<Value>,
    pub bpm: Option<Value>,
    pub key: Option<String>,
    pub time_signature: Option<String>,
    pub variants: Option<Value>,
    pub quality: Option<String>,
    pub workflow: Option<String>,
    pub seed: Option<i64>,
    pub instrumental: Option<bool>,
    pub caption_extra: String,
    pub caption_override: Option<String>,
    pub instruments: Option<Vec<String>>,
    pub avoid_instruments: Vec<String>,
    /// Per-take engine settings (checked against knobs.json by the caller).
    pub engine_params: Option<Obj>,
}

const FILLER: &[&str] = &[
    "音樂", "背景", "一首", "一段", "幫我", "我要", "我想", "我們", "做", "的", "要", "給", "用", "bgm", "歌", "曲", "適合", "可以",
    "有點", "一點", "很", "非常", "感覺", "比較", "那種", "一個", "一些", "需要", "想要", "希望", "整體", "風格", "類型", "請", "畫面",
    "場景", "時候", "用於", "用在",
];

const MAX_TAGS: usize = 14;

fn is_cjk(c: char) -> bool {
    ('\u{3400}'..='\u{9fff}').contains(&c)
}

fn is_ascii_term(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == ' ' || c == '-')
}

fn clen(s: &str) -> usize {
    s.chars().count()
}

/// Comma-separated caption text -> trimmed tags.
pub fn tags(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).collect()
}

fn dedupe(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for i in items {
        if !out.contains(&i) {
            out.push(i);
        }
    }
    out
}

/// Replace every occurrence of `term`; ASCII terms only where not glued to other letters/digits
/// (`(?<![a-z0-9])term(?![a-z0-9])`). Returns the new text and the number of replacements.
fn replace_term(text: &str, term: &str) -> (String, usize) {
    let bounded = is_ascii_term(term);
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let (mut out, mut n, mut last, mut from) = (String::new(), 0, 0, 0);
    while let Some(off) = text[from..].find(term) {
        let p = from + off;
        let end = p + term.len();
        if !bounded || (!word(text[..p].chars().next_back()) && !word(text[end..].chars().next())) {
            out.push_str(&text[last..p]);
            out.push(' ');
            n += 1;
            last = end;
            from = end;
        } else {
            from = p + text[p..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out.push_str(&text[last..]);
    (out, n)
}

/// `(?<![a-z])keyword` anywhere in the lower-cased tag.
fn mentions(tag: &str, keywords: &[String]) -> bool {
    let t = tag.to_lowercase();
    keywords.iter().any(|k| {
        t.match_indices(k.as_str()).any(|(p, _)| !t[..p].chars().next_back().is_some_and(|c| c.is_ascii_lowercase()))
    })
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extracted {
    pub tags: Vec<String>,
    pub passthrough: Vec<String>,
    pub unmatched: Vec<String>,
    pub matched: Vec<String>,
    pub remove: Vec<String>,
    pub bpm_delta: i64,
}

/// Plain-language description -> English caption tags using the lexicon. Longest terms win and are
/// blanked out so 電吉他 is not also read as 吉他. Left-over English passes through; left-over Chinese
/// is reported, never guessed.
pub fn extract_tags(description: &str, lexicon: &Collection) -> Extracted {
    let mut text = description.to_lowercase();
    let mut terms: Vec<(String, &String)> = lexicon
        .iter()
        .filter(|(_, v)| opt_truthy(v.get("tags")))
        .flat_map(|(k, v)| strs(v.get("match")).into_iter().map(move |t| (t.to_lowercase(), k)))
        .collect();
    terms.sort_by_key(|(t, _)| std::cmp::Reverse(clen(t)));
    let mut hit: Vec<String> = Vec::new();
    for (term, key) in &terms {
        if term.is_empty() {
            continue;
        }
        let (new, n) = replace_term(&text, term);
        if n > 0 {
            text = new;
            if !hit.contains(key) {
                hit.push((*key).clone());
            }
        }
    }
    let mut out = Extracted::default();
    for key in &hit {
        let e = &lexicon[key];
        let take = if e.get("group").and_then(Value::as_str) == Some("genre") { 3 } else { 2 };
        out.tags.extend(tags(e["tags"].as_str().unwrap_or("")).into_iter().take(take));
        out.remove.extend(strs(e.get("remove")).iter().map(|r| r.to_lowercase()));
        out.bpm_delta += num(e.get("bpm_delta")).unwrap_or(0.0) as i64;
    }
    for frag in text.split([',', ';', '，', '；', '。', '.', '!', '?', '！', '？', '\n', '、', '/']) {
        let frag = frag.trim();
        if frag.is_empty() {
            continue;
        }
        if frag.chars().any(is_cjk) {
            let mut rest = frag.to_string();
            for f in FILLER {
                rest = rest.replace(f, " ");
            }
            let rest: String = rest.chars().filter(|c| is_cjk(*c) || c.is_ascii_alphanumeric()).collect();
            if rest.chars().filter(|c| is_cjk(*c)).count() >= 2 {
                out.unmatched.push(rest);
            }
        } else if has_ascii_word(frag, 3) {
            out.passthrough.push(frag.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
    out.matched = hit;
    out
}

fn has_ascii_word(s: &str, len: usize) -> bool {
    let mut run = 0;
    for c in s.chars() {
        run = if c.is_ascii_lowercase() { run + 1 } else { 0 };
        if run >= len {
            return true;
        }
    }
    false
}

/// Lexicon entries with group `instrument`: the ids styles, moods and requests refer to.
pub fn instrument_catalog(lexicon: &Collection) -> Collection {
    lexicon
        .iter()
        .filter(|(_, v)| v.get("group").and_then(Value::as_str) == Some("instrument") && opt_truthy(v.get("tags")))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

pub fn instrument_label(key: &str, entry: &Value) -> String {
    if let Some(l) = entry.get("label_zh").filter(|v| truthy(v)) {
        return py_str(l);
    }
    strs(entry.get("match")).into_iter().find(|m| m.chars().any(is_cjk)).unwrap_or_else(|| key.to_string())
}

/// English match words double as detectors: 'strings' finds 'driving strings ostinato'.
fn instr_keywords(entry: &Value) -> Vec<String> {
    strs(entry.get("match"))
        .into_iter()
        .filter(|m| !m.is_empty() && m.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-'))
        .map(|m| m.to_lowercase())
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstrumentPlan {
    pub custom: bool,
    pub use_: Vec<String>,
    pub avoid: Vec<String>,
    pub drop: Vec<String>,
    pub unknown: Vec<String>,
    pub overrode: Vec<String>,
}

/// Which instruments to use, which to keep out, and which style tags that implies dropping.
/// `instruments = None` follows the style (suggest + avoid); a list replaces both. Anything named in
/// the description (`said`) always wins over an avoid list.
pub fn plan_instruments(
    style: &Value,
    moods: &Collection,
    mood_ids: &[String],
    catalog: &Collection,
    said: &[String],
    instruments: Option<&[String]>,
    avoid_instruments: &[String],
) -> InstrumentPlan {
    let si = style.get("instruments").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({}));
    let known = |v: Vec<String>| -> Vec<String> { v.into_iter().filter(|i| catalog.contains_key(i)).collect() };
    let suggest = known(strs(si.get("suggest")));
    let style_avoid = known(strs(si.get("avoid")));
    let custom = instruments.is_some();
    let mut avoid = if custom { vec![] } else { style_avoid.clone() };
    for mid in mood_ids {
        let mood_avoid = moods.get(mid).and_then(|m| m.get("instruments")).and_then(|i| i.get("avoid"));
        avoid.extend(known(strs(mood_avoid)));
    }
    let user_avoid = known(avoid_instruments.to_vec());
    let unknown: Vec<String> = instruments
        .unwrap_or_default()
        .iter()
        .chain(avoid_instruments)
        .filter(|i| !catalog.contains_key(*i))
        .cloned()
        .collect();
    let use_: Vec<String> = match instruments {
        Some(list) => known(list.to_vec()),
        None => suggest.iter().filter(|i| !user_avoid.contains(i)).cloned().collect(),
    };
    let keep = |i: &String| use_.contains(i) || said.contains(i);
    let shown_avoid: Vec<String> = dedupe(avoid.into_iter().chain(user_avoid)).into_iter().filter(|i| !keep(i)).collect();
    let extra = if custom { suggest.clone() } else { vec![] };
    let drop: Vec<String> = dedupe(shown_avoid.iter().cloned().chain(extra)).into_iter().filter(|i| !keep(i)).collect();
    InstrumentPlan {
        custom,
        use_: dedupe(use_.iter().cloned().chain(said.iter().cloned())),
        avoid: shown_avoid,
        drop,
        unknown,
        overrode: said.iter().filter(|i| style_avoid.contains(i)).cloned().collect(),
    }
}

/// Case-insensitive de-duplication, first spelling wins.
fn join(parts: &[&[String]]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    parts.iter().flat_map(|p| p.iter()).filter(|t| seen.insert(t.to_lowercase())).cloned().collect()
}

/// Python's `round()` for .5 ties: to even.
fn round_half_even(x: f64) -> i64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 && (r as i64) % 2 != 0 { (r - x.signum()) as i64 } else { r as i64 }
}

fn caption_len(tags: &[String]) -> usize {
    clen(&tags.join(", "))
}

/// Turn a style + moods + overrides into a track spec. Precedence for quality / variants / duration:
/// explicit input > workflow `defaults` > style (duration only) > generation config.
pub fn build_spec(cfg: &Value, cat: &Catalog, input: &PlanInput) -> Result<Value, String> {
    let mut warnings: Vec<String> = Vec::new();
    let style_id = input.style.as_str();
    let Some(style) = cat.styles.get(style_id) else {
        let mut known: Vec<&String> = cat.styles.keys().collect();
        known.sort();
        let known: Vec<&str> = known.iter().map(|s| s.as_str()).collect();
        return Err(format!("unknown style '{style_id}'. Known: {}", known.join(", ")));
    };
    let description = input.description.as_str();
    let mut style_tags = tags(style.get("caption").and_then(Value::as_str).unwrap_or(""));
    let mut bpm_delta: i64 = 0;
    let mut mood_tags: Vec<String> = Vec::new();
    for mid in &input.moods {
        let Some(mood) = cat.moods.get(mid) else {
            warnings.push(format!("unknown mood '{mid}' ignored"));
            continue;
        };
        mood_tags.extend(tags(mood.get("caption").and_then(Value::as_str).unwrap_or("")));
        bpm_delta += num(mood.get("bpm_delta")).unwrap_or(0.0) as i64;
    }
    let ext = extract_tags(description, &cat.lexicon);
    bpm_delta += ext.bpm_delta;
    let desc_tags: Vec<String> = ext.tags.iter().chain(&ext.passthrough).cloned().collect();
    if !ext.remove.is_empty() {
        let drop = |t: &String| ext.remove.iter().any(|r| t.to_lowercase().contains(r.as_str()));
        style_tags.retain(|t| !drop(t));
        mood_tags.retain(|t| !drop(t));
    }
    let catalog = instrument_catalog(&cat.lexicon);
    let said: Vec<String> = ext.matched.iter().filter(|k| catalog.contains_key(*k)).cloned().collect();
    let inst = plan_instruments(
        style,
        &cat.moods,
        &input.moods,
        &catalog,
        &said,
        input.instruments.as_deref(),
        &input.avoid_instruments,
    );
    if !inst.unknown.is_empty() {
        warnings.push(format!("unknown instrument id(s) ignored: {} (see `msconfig.py instruments`)", inst.unknown.join(", ")));
    }
    if !inst.overrode.is_empty() {
        let labels: Vec<String> = inst.overrode.iter().map(|i| instrument_label(i, &catalog[i])).collect();
        warnings.push(format!("this style usually avoids {} - kept because your description asks for it", labels.join(", ")));
    }
    let drop_kws: Vec<String> = inst.drop.iter().flat_map(|i| instr_keywords(&catalog[i])).collect();
    if !drop_kws.is_empty() {
        style_tags.retain(|t| !mentions(t, &drop_kws));
        mood_tags.retain(|t| !mentions(t, &drop_kws));
    }
    if !ext.unmatched.is_empty() {
        warnings.push(format!(
            "not understood from your description: {} - add English tags in 'extra tags', or teach it in <workspace>/lexicon.json",
            ext.unmatched.join(" / ")
        ));
    }
    let extra_tags = tags(&input.caption_extra);
    let mut instr_tags: Vec<String> = Vec::new();
    for i in &inst.use_ {
        if ext.matched.contains(i) {
            continue; // already in desc_tags
        }
        let kws = instr_keywords(&catalog[i]);
        let present = style_tags.iter().chain(&desc_tags).chain(&extra_tags).chain(&instr_tags).any(|t| mentions(t, &kws));
        if !present {
            instr_tags.extend(tags(catalog[i]["tags"].as_str().unwrap_or("")).into_iter().take(2));
        }
    }

    let override_text = input.caption_override.as_deref().filter(|c| !c.trim().is_empty());
    let (uniq, parts) = if let Some(ov) = override_text {
        let uniq = join(&[&tags(ov)]);
        let parts = json!({"style": [], "instruments": [], "description": [], "moods": [], "extra": [], "override": uniq});
        (uniq, parts)
    } else {
        let mut uniq = join(&[&style_tags, &instr_tags, &desc_tags, &mood_tags, &extra_tags]);
        let parts = json!({"style": style_tags, "instruments": instr_tags, "description": desc_tags, "moods": mood_tags,
                           "extra": extra_tags, "override": []});
        // Too many tags blur the result and ACE-Step caps the caption at 512 chars. Keep, in order: the
        // style's core tags, everything the person said (incl. picked instruments), their moods, the rest.
        let picked: &[String] = if inst.custom { &instr_tags } else { &[] };
        let head = &style_tags[..style_tags.len().min(4)];
        let tail = &style_tags[style_tags.len().min(4)..];
        let prio = join(&[head, &desc_tags, picked, &extra_tags, &mood_tags, &instr_tags, tail]);
        if prio.len() > MAX_TAGS || caption_len(&uniq) > 480 {
            let mut kept: Vec<String> = Vec::new();
            for t in &prio {
                let mut trial = kept.clone();
                trial.push(t.clone());
                if kept.len() < MAX_TAGS && caption_len(&trial) <= 480 {
                    kept = trial;
                }
            }
            let dropped: Vec<String> = prio.iter().filter(|t| !kept.contains(t)).cloned().collect();
            let order: std::collections::HashMap<String, usize> =
                uniq.iter().enumerate().map(|(i, t)| (t.to_lowercase(), i)).collect();
            kept.sort_by_key(|t| order[&t.to_lowercase()]);
            uniq = kept;
            if !dropped.is_empty() {
                warnings.push(format!("kept the {} most important tags; dropped: {}", uniq.len(), dropped.join(", ")));
            }
        }
        (uniq, parts)
    };
    let mut caption = uniq.join(", ");
    if clen(&caption) > 512 {
        let cut: String = caption.chars().take(512).collect();
        caption = cut.rsplit_once(',').map_or(cut.clone(), |(a, _)| a.to_string());
        warnings.push("caption cut at 512 characters (ACE-Step limit)".into());
    } else if clen(&caption) > 400 {
        warnings.push(format!("caption is {} chars; very long captions tend to blur the style", clen(&caption)));
    }
    if override_text.is_some() && uniq.len() > 16 {
        warnings.push(format!("{} tags - more than ~14 tends to blur the result", uniq.len()));
    }

    let seed_str = input.seed.map_or("None".to_string(), |s| s.to_string());
    let mut rng = PyRandom::from_str_seed(&format!("{style_id}|{description}|{seed_str}"));
    let style_bpm = style.get("bpm").filter(|v| truthy(v));
    let mut bpm = input.bpm.clone().filter(|v| !v.is_null());
    if bpm.is_none() {
        if let Some(sb) = style_bpm {
            let range = match sb {
                Value::Array(a) if a.len() == 2 => Some((num(a.first()).unwrap_or(0.0), num(a.get(1)).unwrap_or(0.0))),
                Value::Number(n) => n.as_f64().map(|f| (f, f)),
                _ => None,
            };
            if let Some((lo, hi)) = range {
                let mut b = if sb.is_array() { round_half_even((lo + hi) / 2.0) } else { lo as i64 };
                if bpm_delta != 0 {
                    // moods nudge the tempo, but never far outside what the style is meant to sound like
                    b = ((b + bpm_delta) as f64).min(hi + 10.0).max(lo - 10.0) as i64;
                }
                bpm = Some(json!(b));
            }
        }
    }
    let mut key = input.key.clone().map(Value::String);
    if key.is_none() {
        if let Some(keys) = style.get("keys").and_then(Value::as_array).filter(|k| !k.is_empty()) {
            key = rng.choice(keys).cloned();
        }
    }
    let default_wf = py_str(&cfg["workflow"]["default"]);
    let workflow = input.workflow.clone().filter(|w| !w.is_empty()).unwrap_or(default_wf);
    let wf_defaults = cat.workflows.get(&workflow).and_then(|w| w.get("defaults")).cloned().unwrap_or_else(|| json!({}));
    let pick = |own: Option<Value>, key: &str| -> Value {
        own.filter(|v| truthy(v)).or_else(|| wf_defaults.get(key).cloned()).unwrap_or(Value::Null)
    };
    let quality = pick(input.quality.clone().map(Value::String), "quality");
    let variants = pick(input.variants.clone(), "variants");
    let dur_cfg = style.get("duration").filter(|v| truthy(v)).cloned().unwrap_or_else(|| json!({}));
    let duration = match input.duration.clone().filter(|v| !v.is_null()) {
        Some(d) => d,
        None => wf_defaults
            .get("duration")
            .filter(|v| truthy(v))
            .cloned()
            .or_else(|| dur_cfg.get("default").cloned())
            .unwrap_or_else(|| cfg["generation"].get("duration").cloned().unwrap_or(json!(60))),
    };
    if let Some(max) = dur_cfg.get("max").filter(|v| truthy(v)) {
        if num(Some(&duration)).unwrap_or(0.0) > num(Some(max)).unwrap_or(f64::MAX) {
            warnings.push(format!("duration {}s is above this style's usual max ({}s)", py_str(&duration), py_str(max)));
        }
    }
    let title = match input.title.clone().filter(|t| !t.is_empty()) {
        Some(t) => json!(t),
        None if !description.is_empty() => json!(description.chars().take(40).collect::<String>()),
        None => style.get("name").cloned().unwrap_or_else(|| json!(style_id)),
    };
    let time_signature = match input.time_signature.clone().filter(|t| !t.is_empty()) {
        Some(t) => json!(t),
        None => style.get("time_signature").cloned().unwrap_or_else(|| json!("4")),
    };
    let instrumental = input.instrumental.map_or_else(|| cfg["generation"]["instrumental"].clone(), Value::Bool);
    if truthy(&duration) && num(Some(&duration)).unwrap_or(0.0) > 180.0 {
        warnings.push(format!(
            "{}s is long for ACE-Step: instrumentals are most stable at 30-180 s. For longer pieces generate ~2 min and loop it.",
            py_str(&duration)
        ));
    }
    Ok(json!({
        "title": title,
        "style": style_id,
        "moods": input.moods,
        "description": description,
        "caption": caption,
        "caption_parts": parts,
        "instruments": {"mode": if inst.custom { "custom" } else { "style" }, "use": inst.use_, "avoid": inst.avoid},
        "bpm": bpm,
        "key": key,
        "time_signature": time_signature,
        "duration": duration,
        "variants": variants,
        "quality": quality,
        "workflow": workflow,
        "seed": input.seed.unwrap_or(-1),
        "instrumental": instrumental,
        "loopable": style.get("loopable").is_some_and(truthy),
        "warnings": warnings,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_terms_need_word_boundaries() {
        assert_eq!(replace_term("pianos and piano", "piano"), ("pianos and  ".to_string(), 1));
        assert_eq!(replace_term("電吉他和吉他", "吉他"), ("電 和 ".to_string(), 2));
    }

    #[test]
    fn mentions_needs_a_word_start() {
        let kws = vec!["strings".to_string()];
        assert!(mentions("driving strings ostinato", &kws));
        assert!(!mentions("bowstrings", &kws));
    }

    #[test]
    fn rounds_ties_to_even() {
        assert_eq!(round_half_even(79.0), 79);
        assert_eq!(round_half_even(97.5), 98);
        assert_eq!(round_half_even(98.5), 98);
        assert_eq!(round_half_even(98.6), 99);
    }
}
