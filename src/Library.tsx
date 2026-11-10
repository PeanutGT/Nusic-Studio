import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import { t } from "./i18n";
import { errorText, type Issue, type LoudnessPreset, type Take } from "./types";

function issueText(i: Issue): string {
  const label = t.issue[i.code]?.(i) ?? i.code;
  return i.at > 0 ? `${label} @ ${t.atTime(i.at)}` : label;
}

function Issues({ issues }: { issues: Issue[] | null }) {
  if (issues === null) return <span className="muted">{t.checkNone}</span>;
  if (issues.length === 0) return <span className="muted">{t.checkOk}</span>;
  return (
    <span className="issues">
      {issues.map((i, n) => (
        <span key={n} className="chip warn">
          {issueText(i)}
        </span>
      ))}
    </span>
  );
}

/** Pick a variant and a loudness target, run the take's post-processing workflow, export. */
function Export({ take, presets, onDone }: { take: Take; presets: LoudnessPreset[]; onDone: () => void }) {
  const [variant, setVariant] = useState<number>(take.picked ?? take.checks[0]?.index ?? 1);
  const [loudness, setLoudness] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const run = async () => {
    setBusy(true);
    setNote("");
    try {
      const rec = await invoke<{ files: string[] }>("finalize_take", {
        id: take.id,
        input: { variant, loudness: loudness || null },
      });
      setNote(t.exportDone(rec.files.join("、")));
      onDone();
    } catch (e) {
      setNote(errorText(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="row export">
      {take.checks.length > 1 && (
        <select value={variant} onChange={(e) => setVariant(Number(e.target.value))}>
          {take.checks.map((c) => (
            <option key={c.index} value={c.index}>
              {t.variant(c.index)}
            </option>
          ))}
        </select>
      )}
      <label className="small">
        {t.loudness}{" "}
        <select value={loudness} onChange={(e) => setLoudness(e.target.value)}>
          <option value="">{t.loudnessWorkflow}</option>
          {presets.map((p) => (
            <option key={p.id} value={p.id}>
              {`${p.label ?? p.id}（${p.lufs} LUFS）`}
            </option>
          ))}
        </select>
      </label>
      <button onClick={run} disabled={busy}>
        {busy ? t.exporting : t.exportButton}
      </button>
      {note && <span className="small-text">{note}</span>}
    </div>
  );
}

/** Finished takes, newest first, with the automatic check and exports. Listening comes with A4. */
export default function Library({
  takes,
  presets,
  onTrash,
  onChanged,
}: {
  takes: Take[];
  presets: LoudnessPreset[];
  onTrash: (id: string) => void;
  onChanged: () => void;
}) {
  return (
    <section className="card">
      <h2>{t.libraryTitle}</h2>
      {takes.length === 0 && <p className="muted">{t.libraryEmpty}</p>}
      <ul className="takes">
        {takes.map((k) => (
          <li key={k.id}>
            <div>
              <strong>{k.title}</strong>{" "}
              <span className="muted small-text">
                {[k.created?.replace("T", " "), k.quality && (t.qualityNames[k.quality] ?? k.quality), k.bpm && `${k.bpm} BPM`, k.key]
                  .filter(Boolean)
                  .join(" · ")}
              </span>
            </div>
            <code className="small-text">{k.dir}</code>
            <ul className="variants small-text">
              {k.checks.map((c) => (
                <li key={c.index}>
                  {c.file} <Issues issues={c.qc} />
                </li>
              ))}
            </ul>
            {k.exports.length > 0 && (
              <ul className="variants small-text">
                {k.exports.map((x) => (
                  <li key={x.base}>
                    {x.files.join("、")} <span className="muted">{t.exportLine(x.lufs, x.true_peak_db)}</span>{" "}
                    <Issues issues={x.issues} />
                    {[...x.warnings, ...x.qc].map((w, n) => (
                      <div key={n} className="muted">
                        {w}
                      </div>
                    ))}
                  </li>
                ))}
              </ul>
            )}
            <Export take={k} presets={presets} onDone={onChanged} />
            <button className="link" onClick={() => onTrash(k.id)}>
              {t.trash}
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
