import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { t } from "./i18n";
import { errorText, type Overview, type Plan, type PlanInput } from "./types";

/** Style + moods + description + quality preset -> live plan preview -> queue. */
export default function Create({ overview }: { overview: Overview }) {
  const [style, setStyle] = useState(overview.styles[0]?.id ?? "");
  const [description, setDescription] = useState("");
  const [moods, setMoods] = useState<string[]>([]);
  const [quality, setQuality] = useState(overview.default_quality);
  const [duration, setDuration] = useState("");
  const [seed, setSeed] = useState("-1");
  const [plan, setPlan] = useState<Plan | null>(null);
  const [error, setError] = useState("");
  const [queued, setQueued] = useState(false);

  const input = (): PlanInput => ({
    style,
    description,
    moods,
    quality,
    duration: duration.trim() ? Number(duration) : undefined,
    seed: Number(seed) >= 0 ? Number(seed) : undefined,
  });

  useEffect(() => {
    const timer = window.setTimeout(() => {
      invoke<Plan>("plan_track", { input: input() }).then(
        (p) => {
          setPlan(p);
          setError("");
        },
        (e) => setError(errorText(e)),
      );
    }, 300);
    return () => window.clearTimeout(timer);
  }, [style, description, moods, quality, duration, seed]);

  const enqueue = async () => {
    setError("");
    try {
      await invoke<string>("enqueue", { input: input() });
      setQueued(true);
      window.setTimeout(() => setQueued(false), 2000);
    } catch (e) {
      setError(errorText(e));
    }
  };

  const toggleMood = (id: string) => setMoods((m) => (m.includes(id) ? m.filter((x) => x !== id) : [...m, id]));
  const p = plan?.params;
  const warnings = plan ? [...plan.warnings, ...plan.spec.warnings] : [];

  return (
    <section className="card">
      <h2>{t.createTitle}</h2>
      <div className="row">
        <label className="grow">
          {t.style}
          <select value={style} onChange={(e) => setStyle(e.target.value)}>
            {overview.styles.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name_zh ?? s.name}
              </option>
            ))}
          </select>
        </label>
        <label className="small">
          {t.quality}
          <select value={quality} onChange={(e) => setQuality(e.target.value)}>
            {Object.keys(overview.presets).map((q) => (
              <option key={q} value={q}>
                {t.qualityNames[q] ?? q}
              </option>
            ))}
          </select>
        </label>
      </div>
      <p className="muted small-text">{overview.styles.find((s) => s.id === style)?.description_zh}</p>
      <label>
        {t.description}
        <textarea
          rows={2}
          value={description}
          placeholder={t.descriptionPlaceholder}
          onChange={(e) => setDescription(e.target.value)}
        />
      </label>
      <div className="chips" role="group" aria-label={t.moods}>
        <span className="muted">{t.moods}</span>
        {overview.moods.map((m) => (
          <button
            key={m.id}
            className={moods.includes(m.id) ? "chip on" : "chip"}
            aria-pressed={moods.includes(m.id)}
            onClick={() => toggleMood(m.id)}
          >
            {m.label_zh ?? m.id}
          </button>
        ))}
      </div>
      <div className="row">
        <label className="small">
          {t.duration}
          <input
            type="number"
            min={5}
            max={600}
            value={duration}
            placeholder={t.durationAuto}
            onChange={(e) => setDuration(e.target.value)}
          />
        </label>
        <label className="small">
          {t.seed}
          <input type="number" value={seed} onChange={(e) => setSeed(e.target.value)} />
        </label>
      </div>

      {plan && (
        <div className="plan">
          <p className="caption">{plan.spec.caption}</p>
          <p className="muted small-text">
            {[
              plan.spec.bpm ? `${plan.spec.bpm} BPM` : null,
              plan.spec.key,
              t.durationSec(Number(p?.duration ?? plan.spec.duration)),
              `${t.tier} ${plan.tier}`,
            ]
              .filter(Boolean)
              .join(" · ")}
          </p>
          <p className="muted small-text">
            {t.paramsLine(
              String(p?.dit_model ?? ""),
              Number(p?.inference_steps ?? 0),
              Number(p?.batch_size ?? 1),
              p?.lm_model ? String(p.lm_model) : null,
              Boolean(p?.thinking),
            )}
          </p>
          {warnings.length > 0 && (
            <ul className="warnings">
              {warnings.map((w) => (
                <li key={w}>{w}</li>
              ))}
            </ul>
          )}
        </div>
      )}
      <div className="row">
        <button className="primary" onClick={enqueue} disabled={!plan}>
          {t.enqueue}
        </button>
        {queued && <span className="muted">{t.queued}</span>}
      </div>
      {error && <p className="error">{error}</p>}
    </section>
  );
}
