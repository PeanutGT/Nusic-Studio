import { t } from "./i18n";
import type { Take } from "./types";

/** Finished takes, newest first. Listening and annotations come with the full UI (A4). */
export default function Library({ takes, onTrash }: { takes: Take[]; onTrash: (id: string) => void }) {
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
            <div className="small-text">{k.files.join("、")}</div>
            <button className="link" onClick={() => onTrash(k.id)}>
              {t.trash}
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
