"""Export golden baseline data from the music-studio skill (v3.7) for Rust parity tests.

    python tools/export_golden.py --skill PATH/TO/NutsSkills/skills/music-studio [--out golden/v3.7]

Needs Python 3.10+, numpy and ffmpeg on PATH (the same things the skill needs).
Everything is computed with the skill's own functions on an empty workspace, so
user overrides never leak in. Re-running it on the same skill version gives the
same files: the diff is the review.

What it writes (see golden/README.md for the file formats):
    defaults/            the skill's built-in data, copied as-is (Rust reads the same input)
    plan/cases.json      planning inputs -> expected spec (caption, parts, bpm, key, warnings...)
    plan/params.json     engine parameters resolved per hardware tier x quality preset
    plan/keys.json       Python's random.Random(str).choice key picks, for many seeds
    audio/*.flac         short synthetic test signals (16-bit, 48 kHz, stereo)
    qc/cases.json        per signal: inspect_audio issues, analyze_array report, qc notes,
                         seam metrics, brightness, loudnorm gain
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np

SR = 48000


def write(path: Path, obj) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(obj, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def git_rev(path: Path) -> str | None:
    try:
        return subprocess.run(["git", "-C", str(path), "rev-parse", "HEAD"], capture_output=True,
                              text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None


# --------------------------------------------------------------------------- planning
PLAN_CASES = [
    # style only, every built-in style (filled in below)
    {"style": "lofi", "description": "下雨的夜晚，適合讀書", "seed": 1},
    {"style": "lofi", "description": "rainy night, no drums", "seed": 2},
    {"style": "war-requiem", "description": "戰敗之後的悲壯與犧牲", "seed": 3},
    {"style": "nature-piano", "description": "清晨的森林，鋼琴和長笛", "moods": ["calm", "hopeful"], "seed": 4},
    {"style": "rpg-town", "description": "熱鬧的村莊", "moods": ["cozy"], "instruments": ["erhu", "guzheng"], "seed": 5},
    {"style": "rpg-battle", "description": "緊張的追逐戰", "avoid_instruments": ["choir"], "seed": 6},
    {"style": "synthwave-drive", "description": "夜晚的城市公路", "moods": ["nostalgic", "energetic"], "seed": 7},
    {"style": "jazz-cafe", "description": "咖啡店，薩克斯風，不要鼓", "seed": 8},
    {"style": "ambient-focus", "description": "冥想", "moods": ["focused"], "seed": 9},
    {"style": "epic-trailer", "description": "壯麗的城堡，合唱與太鼓", "moods": ["epic", "triumphant"], "seed": 10},
    {"style": "dungeon-ambient", "description": "洞穴裡的恐怖氣氛", "moods": ["eerie", "dark"], "seed": 11},
    {"style": "chiptune-adventure", "description": "快樂的冒險", "moods": ["playful"], "bpm": 150, "seed": 12},
    {"style": "cinematic-emotional", "description": "婚禮", "duration": 200, "seed": 13},
    {"style": "corporate-upbeat", "description": "", "caption_extra": "ukulele, claps", "seed": 14},
    {"style": "puzzle-calm", "description": "解謎", "caption_override": "marimba, soft pads, 90 bpm, playful", "seed": 15},
    {"style": "from-description", "description": "一段完全聽不懂的描述 xyzzy", "seed": 16},
    {"style": "lofi", "description": "鋼琴 吉他 小提琴 大提琴 長笛 豎琴 薩克斯風 小號 馬林巴 鐘聲 合唱 二胡 古箏 尺八",
     "moods": ["calm", "melancholic", "nostalgic"], "seed": 17},
    {"style": "acoustic-folk", "description": "夏天的海邊", "key": "D major", "time_signature": "3",
     "workflow": "game-bgm", "quality": "high", "variants": 3, "seed": 18},
    {"style": "cinematic-tension", "description": "tense chase", "moods": ["tense", "unknown-mood"],
     "instruments": ["not-an-instrument"], "seed": 19},
]


def plan_golden(ms, out: Path) -> None:
    cfg, _ = ms.load_config()
    styles, moods, workflows, lexicon = ms.load_styles(), ms.load_moods(), ms.load_workflows(), ms.load_lexicon()
    cases = [{"style": s} for s in sorted(styles)] + PLAN_CASES
    rows = []
    for i, inp in enumerate(cases):
        kw = dict(inp)
        style_id = kw.pop("style")
        kw["mood_ids"] = kw.pop("moods", [])
        spec = ms.build_spec(cfg, styles, moods, style_id=style_id, workflows=workflows, lexicon=lexicon, **kw)
        rows.append({"id": f"plan-{i:03d}", "input": inp, "expected": spec})
    write(out / "plan" / "cases.json", {"function": "msconfig.build_spec", "cases": rows})

    params = []
    for tier in cfg["hardware"]["tiers"]:
        for quality in cfg["quality"]["presets"]:
            if quality.startswith("_"):
                continue
            for overrides in (None, {"duration": 400, "batch_size": 8, "thinking": True}):
                r = ms.resolve_params(cfg, quality=quality, overrides=overrides, tier=tier)
                params.append({"input": {"tier": tier, "quality": quality, "overrides": overrides}, "expected": r})
    write(out / "plan" / "params.json", {"function": "msconfig.resolve_params", "cases": params})

    import random
    keys = []
    for style_id in sorted(styles):
        options = styles[style_id].get("keys")
        if not options:
            continue
        for desc in ("", "下雨的夜晚", "battle"):
            for seed in (None, 0, 1, 42, 12345, -1):
                seed_str = f"{style_id}|{desc}|{seed}"
                keys.append({"seed_string": seed_str, "options": options,
                             "expected": random.Random(seed_str).choice(options)})
    write(out / "plan" / "keys.json", {
        "function": "random.Random(seed_string).choice(options)",
        "note": "build_spec picks the key this way; seed_string is f'{style_id}|{description}|{seed}' with Python's "
                "str(None) == 'None'. Python seeds MT19937 from SHA-512 of the string (random.seed version 2).",
        "cases": keys})


# --------------------------------------------------------------------------- audio / QC
def signals() -> dict[str, tuple[np.ndarray, dict]]:
    """Synthetic signals mirroring the skill's own QC unit tests. Returns name -> (stereo float, meta)."""
    sr = SR
    t = np.arange(sr * 20) / sr
    lows = 0.3 * np.sin(2 * np.pi * 220 * t) * (0.6 + 0.4 * np.sin(2 * np.pi * 0.5 * t))
    tone = lows + 0.06 * np.sin(2 * np.pi * 6000 * t)
    x = np.stack([tone, tone], 1)
    x[-sr * 2:] *= np.linspace(1, 0, sr * 2)[:, None]
    out: dict[str, tuple[np.ndarray, dict]] = {"clean": (x, {})}

    rng = np.random.default_rng(0)
    drums = x * 0.5
    for k in range(36):
        s0, n = int(k * sr * 0.5), int(sr * 0.15)
        drums[s0:s0 + n] += (rng.standard_normal(n) * 0.4 * np.exp(-np.arange(n) / (sr * 0.03)))[:, None]
    out["drums"] = (drums, {})
    click = x.copy(); click[sr * 5] += 0.5
    out["click"] = (click, {})
    gap = x.copy(); gap[sr * 6:sr * 9] = 0
    out["gap"] = (gap, {})
    out["cut_off"] = (x[: sr * 10].copy(), {})
    out["cut_off_loopable"] = (x[: sr * 10].copy(), {"loopable": True})
    out["clipped"] = (np.clip(x * 5, -1, 1), {})
    out["near_silent"] = (x * 1e-4, {})
    out["low_volume"] = (x * 0.1, {})
    dull = np.stack([lows, lows], 1)
    dull[-sr * 2:] *= np.linspace(1, 0, sr * 2)[:, None]
    out["dull"] = (dull, {})
    rng = np.random.default_rng(1)
    tail = x.copy(); tail[sr * 16:] = 0
    tail[-sr // 3:] += (rng.standard_normal(sr // 3) * 3e-4)[:, None]
    tail[-sr // 5:] += (rng.standard_normal(sr // 5) * 1e-3)[:, None]
    out["tail_hiss"] = (tail, {})
    tail_gap = tail.copy(); tail_gap[sr * 18:sr * 19] = x[sr * 2:sr * 3]
    out["tail_gap"] = (tail_gap, {})
    t16 = np.arange(sr * 16) / sr
    s = 0.3 * np.sin(2 * np.pi * 220 * t16)
    out["periodic_loop"] = (np.stack([s, s], 1), {"loopable": True, "looped": True, "bpm": 110})
    t30 = np.arange(sr * 30) / sr
    env = np.select([t30 < 10, t30 < 20], [0.06, 0.18], 0.5)
    w = env * np.sin(2 * np.pi * 440 * t30)
    out["wide_dynamics"] = (np.stack([w, w], 1), {"loudnorm": {"target_lufs": -16, "true_peak_db": -1.5}})
    return out


def write_flac(x: np.ndarray, path: Path, pp) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    cfg = {"ffmpeg": "ffmpeg"}
    pp.encode(np.asarray(x, dtype=np.float32), SR, path, cfg, ["-c:a", "flac", "-sample_fmt", "s16"])


def qc_golden(pp, cfg: dict, out: Path) -> None:
    rows = []
    for name, (x, meta) in signals().items():
        flac = out / "audio" / f"{name}.flac"
        write_flac(x, flac, pp)
        y = pp.decode(flac, cfg, SR)  # expectations are computed on what a decoder reads back
        loopable = bool(meta.get("loopable"))
        bar = int(round(SR * 60 / meta["bpm"] * 4)) if meta.get("bpm") else None
        lufs_t, tp_t = cfg["postprocess"]["loudness_lufs"], cfg["postprocess"]["true_peak_db"]
        report = pp.analyze_array(y, SR, flac, cfg, lufs_t, tp_t, bar)
        row = {
            "id": name,
            "audio": f"audio/{name}.flac",
            "input": {"loopable": loopable, "looped": bool(meta.get("looped")), "bar_samples": bar,
                      "lufs_target": lufs_t, "true_peak_target": tp_t},
            "expected": {
                "inspect_audio": pp.inspect_audio(y, SR, loopable),
                "analyze_array": report,
                "qc_notes": pp.qc(report, cfg, looped=bool(meta.get("looped"))),
                "brightness": pp.brightness(y.mean(axis=1), SR),
                "seam_metrics": pp.seam_metrics(y, SR, bar),
            },
        }
        ln = meta.get("loudnorm") or {"target_lufs": lufs_t, "true_peak_db": tp_t}
        ctx = {"cfg": cfg, "warnings": [], "notes": []}
        z = pp.op_loudnorm(y, SR, ln, ctx)
        nz = np.abs(y) > 1e-4
        gain_db = float(20 * np.log10(np.median(np.abs(z[nz]) / np.abs(y[nz])))) if nz.any() else 0.0
        row["input"]["loudnorm"] = ln
        row["expected"]["loudnorm"] = {"gain_db": round(gain_db, 3), "notes": ctx["notes"], "warnings": ctx["warnings"]}
        rows.append(row)
    write(out / "qc" / "cases.json", {
        "sample_rate": SR,
        "functions": ["postprocess.inspect_audio", "postprocess.analyze_array", "postprocess.qc",
                      "postprocess.brightness", "postprocess.seam_metrics", "postprocess.op_loudnorm"],
        "note": "lufs / true_peak_db / lra come from ffmpeg loudnorm (EBU R128); compare with a tolerance.",
        "cases": rows})


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--skill", required=True, type=Path, help="path to NutsSkills/skills/music-studio")
    ap.add_argument("--out", type=Path, default=Path(__file__).resolve().parent.parent / "golden" / "v3.7")
    a = ap.parse_args(argv)
    skill = a.skill.resolve()
    sys.path.insert(0, str(skill / "scripts"))
    ws = Path(tempfile.mkdtemp(prefix="ms_golden_"))
    os.environ["MUSIC_STUDIO_HOME"] = str(ws)
    os.environ["MUSIC_STUDIO_FAKE_VRAM_GB"] = "4"  # never probe a real GPU; tiers are passed explicitly anyway
    try:
        import msconfig as ms
        import postprocess as pp
        version = next((ln.split(":", 1)[1].strip().strip('"') for ln in
                        (skill / "SKILL.md").read_text(encoding="utf-8").splitlines()
                        if ln.strip().startswith("version:")), None)
        if a.out.exists():
            shutil.rmtree(a.out)
        shutil.copytree(skill / "defaults", a.out / "defaults")
        plan_golden(ms, a.out)
        cfg, _ = ms.load_config()
        qc_golden(pp, cfg, a.out)
        write(a.out / "manifest.json", {
            "source": "PeanutGT/NutsSkills skills/music-studio",
            "skill_version": version,
            "source_commit": git_rev(skill),
            "numpy": np.__version__,
            "ffmpeg": subprocess.run(["ffmpeg", "-version"], capture_output=True, text=True).stdout.split("\n")[0],
        })
    finally:
        shutil.rmtree(ws, ignore_errors=True)
    print(f"wrote {a.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
