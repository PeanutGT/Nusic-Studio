import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { t } from "./i18n";
import "./App.css";

type AppInfo = { version: string; target: string };
type Settings = { ace_step_dir: string; workspace_dir: string };
type EngineError = { code: string; message: string; hint: string };
type Variant = { index: number; file: string; seed: number; sample_rate?: number };
type Job = {
  id: string;
  caption: string;
  status: "running" | "done" | "failed" | "cancelled";
  phase: string;
  pct: number;
  detail: string;
  warnings: string[];
  error: EngineError | null;
  out_dir: string;
  variants: Variant[];
  effective: { device?: string } | null;
  elapsed_s: number | null;
};
type EngineStatus = {
  state: "stopped" | "starting" | "idle" | "busy" | "crashed";
  pid: number | null;
  models_loaded: boolean;
  model: string | null;
  job: Job | null;
};
type GpuInfo = { name: string; total_gb: number; used_gb: number };

function errorText(e: unknown): string {
  const err = e as Partial<EngineError>;
  if (err && typeof err === "object" && err.code) {
    return [t.errors[err.code] ?? "", err.message].filter(Boolean).join(" ");
  }
  return String(e);
}

export default function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [backendDown, setBackendDown] = useState(false);
  const [settings, setSettings] = useState<Settings>({ ace_step_dir: "", workspace_dir: "" });
  const [savedNote, setSavedNote] = useState(false);
  const [status, setStatus] = useState<EngineStatus | null>(null);
  const [gpu, setGpu] = useState<GpuInfo | null | undefined>(undefined);
  const [caption, setCaption] = useState("");
  const [duration, setDuration] = useState(30);
  const [seed, setSeed] = useState(-1);
  const [actionError, setActionError] = useState("");
  const [cancelling, setCancelling] = useState(false);
  const [log, setLog] = useState<string[] | null>(null);

  useEffect(() => {
    invoke<AppInfo>("get_app_info").then(setInfo, () => setBackendDown(true));
    invoke<Settings>("get_settings").then(setSettings, () => {});
    invoke<EngineStatus>("engine_status").then(setStatus, () => {});
    const unlisten = listen<EngineStatus>("engine-status", (e) => setStatus(e.payload));
    const pollGpu = () => invoke<GpuInfo | null>("gpu_info").then(setGpu, () => setGpu(null));
    pollGpu();
    const timer = window.setInterval(pollGpu, 2000);
    return () => {
      unlisten.then((f) => f());
      window.clearInterval(timer);
    };
  }, []);

  const saveSettings = async () => {
    setActionError("");
    try {
      await invoke("save_settings", { settings });
      setSavedNote(true);
      window.setTimeout(() => setSavedNote(false), 2000);
    } catch (e) {
      setActionError(errorText(e));
    }
  };

  const generate = async () => {
    setActionError("");
    try {
      await invoke("generate", { input: { caption, duration, seed } });
    } catch (e) {
      setActionError(errorText(e));
    }
  };

  const cancel = async () => {
    setCancelling(true);
    try {
      await invoke("cancel");
    } finally {
      setCancelling(false);
    }
  };

  const restart = async () => {
    setActionError("");
    try {
      await invoke("restart_engine");
    } catch (e) {
      setActionError(errorText(e));
    }
  };

  const loadLog = useCallback(() => {
    invoke<string[]>("engine_log").then(setLog, () => setLog([]));
  }, []);

  const job = status?.job ?? null;
  const running = job?.status === "running";
  const stateLabel = status
    ? status.state === "idle" && !status.models_loaded
      ? t.engineState.idleEmpty
      : t.engineState[status.state]
    : t.loading;

  return (
    <main className="shell">
      <header>
        <h1>{t.appName}</h1>
        <p className="tagline">
          {t.tagline} · {t.version} {info ? info.version : backendDown ? t.backendUnavailable : t.loading}
        </p>
      </header>

      <section className="card">
        <h2>{t.settingsTitle}</h2>
        <label>
          {t.aceDir}
          <input
            value={settings.ace_step_dir}
            placeholder={t.aceDirPlaceholder}
            onChange={(e) => setSettings({ ...settings, ace_step_dir: e.target.value })}
          />
        </label>
        <label>
          {t.workspaceDir}
          <input
            value={settings.workspace_dir}
            onChange={(e) => setSettings({ ...settings, workspace_dir: e.target.value })}
          />
        </label>
        <div className="row">
          <button onClick={saveSettings}>{t.save}</button>
          {savedNote && <span className="muted">{t.saved}</span>}
        </div>
      </section>

      <section className="card">
        <h2>{t.generateTitle}</h2>
        <label>
          {t.caption}
          <textarea
            rows={3}
            value={caption}
            placeholder={t.captionPlaceholder}
            onChange={(e) => setCaption(e.target.value)}
          />
        </label>
        <div className="row">
          <label className="small">
            {t.duration}
            <input
              type="number"
              min={10}
              max={240}
              value={duration}
              onChange={(e) => setDuration(Number(e.target.value))}
            />
          </label>
          <label className="small">
            {t.seed}
            <input type="number" value={seed} onChange={(e) => setSeed(Number(e.target.value))} />
          </label>
        </div>
        <div className="row">
          <button className="primary" onClick={generate} disabled={running}>
            {t.generate}
          </button>
          <button onClick={cancel} disabled={!running || cancelling}>
            {cancelling ? t.cancelling : t.cancel}
          </button>
        </div>
        <p className="muted small-text">{t.a1Note}</p>
        {actionError && <p className="error">{actionError}</p>}
      </section>

      <section className="card">
        <h2>{t.statusTitle}</h2>
        <dl className="info">
          <dt>{t.engine}</dt>
          <dd>
            {stateLabel}
            {status?.model ? ` · ${status.model}` : ""}
          </dd>
          <dt>{t.vram}</dt>
          <dd>
            {gpu === undefined
              ? t.loading
              : gpu === null
                ? t.vramNone
                : `${gpu.name} · ${t.vramUsed(gpu.used_gb, gpu.total_gb)}`}
          </dd>
        </dl>
        {gpu && (
          <div className="bar" title={t.vramUsed(gpu.used_gb, gpu.total_gb)}>
            <div style={{ width: `${Math.min(100, (gpu.used_gb / gpu.total_gb) * 100)}%` }} />
          </div>
        )}

        {job && (
          <div className="job">
            <p>
              <strong>{t.jobStatus[job.status]}</strong> · {t.phase[job.phase] ?? job.phase}
              {job.detail ? ` (${job.detail})` : ""}
              {job.elapsed_s != null ? ` · ${t.seconds(job.elapsed_s)}` : ""}
              {job.effective?.device ? ` · ${t.device} ${job.effective.device}` : ""}
            </p>
            {running && (
              <div className="bar progress">
                <div style={{ width: `${Math.round(job.pct * 100)}%` }} />
              </div>
            )}
            {job.warnings.length > 0 && (
              <div className="warnings">
                <strong>{t.warnings}</strong>
                <ul>
                  {job.warnings.map((w) => (
                    <li key={w}>{w}</li>
                  ))}
                </ul>
              </div>
            )}
            {job.error && (
              <div className="error">
                <strong>{t.error}</strong> {errorText(job.error)}
                {job.error.hint && (
                  <pre>
                    {t.hint}: {job.error.hint}
                  </pre>
                )}
              </div>
            )}
            {job.status === "done" && (
              <div>
                <strong>{t.output}</strong> <code>{job.out_dir}</code>
                <ul>
                  {job.variants.map((v) => (
                    <li key={v.index}>
                      <code>{v.file}</code> · seed {v.seed}
                    </li>
                  ))}
                </ul>
              </div>
            )}
          </div>
        )}

        <div className="row">
          <button onClick={restart}>{t.restartEngine}</button>
        </div>
        <details onToggle={(e) => (e.currentTarget.open ? loadLog() : undefined)}>
          <summary>{t.showLog}</summary>
          <button onClick={loadLog}>{t.refreshLog}</button>
          <pre className="log">{log && log.length ? log.join("\n") : t.logEmpty}</pre>
        </details>
      </section>

      <footer className="muted small-text">
        {t.platform} {info ? info.target : "—"}
      </footer>
    </main>
  );
}
