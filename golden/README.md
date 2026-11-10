# Golden baseline

Reference outputs of the music-studio skill, used to check that the Rust
port behaves the same. One folder per skill version; `v3.7/` is the baseline
for milestones A2 (planning, captions) and A3 (loudness, QC).

Regenerate (needs Python 3.10+, numpy, ffmpeg):

```bash
python tools/export_golden.py --skill path/to/NutsSkills/skills/music-studio
```

The export is deterministic: re-running on the same skill version produces
identical files. `manifest.json` records the skill version and source commit.

## Files

| File | Source function | Contents |
|---|---|---|
| `defaults/` | – | The skill's built-in data (config, knobs, styles, moods, workflows, lexicon), copied as-is so Rust reads the same input |
| `plan/cases.json` | `msconfig.build_spec` | Every built-in style plus mixed cases (Chinese/English descriptions, moods, picked/avoided instruments, overrides, tag cap, unknown ids). `input` → full spec in `expected` |
| `plan/params.json` | `msconfig.resolve_params` | Engine parameters for every tier × quality preset, with and without over-limit request overrides |
| `plan/keys.json` | `random.Random(s).choice` | Key picks for many seed strings. Python seeds MT19937 from SHA-512 of the string, so Rust must reproduce that to match |
| `audio/*.flac` | – | Synthetic test signals, 16-bit / 48 kHz / stereo |
| `qc/cases.json` | `postprocess.*` | Per signal: `inspect_audio` issues, `analyze_array` report, `qc` notes, `brightness`, `seam_metrics`, and the `op_loudnorm` gain |
| `qc/ops.json` | `postprocess.op_*` | `trim_silence`, `fit`, `fade`, `loop` on the signals: length, checksums, sample probes, notes and warnings |
| `qc/r128.json` | ffmpeg `ebur128` filter | The signals through a standard EBU R128 meter: integrated loudness, loudness range, true peak (0.1 precision) |

Expectations are computed on the decoded FLAC, not the original floats, so a
decoder in Rust sees exactly the same samples.

## Tolerances

- Planning (`plan/*`): exact match, including warning text.
- Sample-domain numbers (peak, DC, silence, seam, brightness): within rounding of the stored precision.
- Post-processing ops (`qc/ops.json`): exact length, samples within 2e-6, notes and warnings exact.
- QC issue codes and their order: exact. Times (`at`): ±0.01 s.
- Loudness: v3.7 read it from ffmpeg's `loudnorm` filter, whose analysis runs on its own
  192 kHz, look-ahead-buffered copy and reads 0.1-0.4 LU off a standard meter on these signals
  (its LRA much more). The app meters to the standard instead: `lufs` and `true_peak_db` within
  ±0.11 of `qc/r128.json`, `lra` within ±0.6 (reported only, no check uses it), and every value
  within 0.5 of v3.7's reading. The loudness gain equals target − standard reading (capped by
  the true peak) within ±0.11 dB.
