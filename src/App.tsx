import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { t } from "./i18n";
import Create from "./Create";
import Library from "./Library";
import { errorText, type Overview, type QueueView, type Take } from "./types";
import "./App.css";

type AppInfo = { version: string; target: string };
type Settings = { ace_step_dir: string; workspace_dir: string };
type EngineStatus = {
  state: "stopped" | "starting" | "idle" | "busy" | "crashed";
  pid: number | null;
  models_loaded: boolean;
  model: string | null;
  job: { id: string; detail: string; effective: { device?: string } | null } | null;
};
type GpuInfo = { name: string; total_gb: number; used_gb: number };

export default function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [backendDown, setBackendDown] = useState(false);
  const [settings, setSettings] = useState<Settings>({ ace_step_dir: "", workspace_dir: "" });
  const [savedNote, setSavedNote] = useState(false);
  const [status, setStatus] = useState<EngineStatus | null>(null);
  const [gpu, setGpu] = useState<GpuInfo | null | undefined>(undefined);
  const [overview, setOverview] = useState<Overview | null>(null);
  const [queue, setQueue] = useState<QueueView>({ jobs: [], current: null });
  const [takes, setTakes] = useState<Take[]>([]);
  const [actionError, setActionError] = useState("");
  const [log, setLog] = useState<string[] | null>(null);

  const loadOverview = useCallback(() => {
    invoke<Overview>("studio_overview").then(setOverview, (e) => setActionError(errorText(e)));
  }, []);
  const loadTakes = useCallback(() => {
    invoke<Take[]>("list_takes").then(setTakes, () => {});
  }, []);

  useEffect(() => {
    invoke<AppInfo>("get_app_info").then(setInfo, () => setBackendDown(true));
    invoke<Settings>("get_settings").then(setSettings, () => {});
    invoke<EngineStatus>("engine_status").then(setStatus, () => {});
    invoke<QueueView>("queue_status").then(setQueue, () => {});
    loadOverview();
    loadTakes();
    const unlisten = listen<EngineStatus>("engine-status", (e) => setStatus(e.payload));
    const unlistenQueue = listen<QueueView>("queue-status", (e) => setQueue(e.payload));
    const pollGpu = () => invoke<GpuInfo | null>("gpu_info").then(setGpu, () => setGpu(null));
    pollGpu();
    const timer = window.setInterval(pollGpu, 2000);
    return () => {
      unlisten.then((f) => f());
      unlistenQueue.then((f) => f());
      window.clearInterval(timer);
    };
  }, [loadOverview, loadTakes]);

  // A finished job means a new take in the library.
  const doneCount = queue.jobs.filter((j) => j.status === "done").length;
  useEffect(loadTakes, [doneCount, loadTakes]);

  const saveSettings = async () => {
    setActionError("");
    try {
      await invoke("save_settings", { settings });
      loadOverview();
      loadTakes();
      setSavedNote(true);
      window.setTimeout(() => setSavedNote(false), 2000);
    } catch (e) {
      setActionError(errorText(e));
    }
  };

  const cancelJob = async (id: string) => {
    await invoke("cancel_job", { id }).catch(() => {});
  };

  const trashTake = async (id: string) => {
    if (!window.confirm(t.trashConfirm)) return;
    await invoke("trash_take", { id }).catch((e) => setActionError(errorText(e)));
    loadTakes();
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

  const device = status?.job?.effective?.device;
  const visibleJobs = [...queue.jobs].reverse().slice(0, 20);
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

      {overview ? (
        <Create overview={overview} />
      ) : (
        <section className="card">
          <p className="muted">{t.loading}</p>
        </section>
      )}
      {overview && overview.problems.length > 0 && (
        <section className="card">
          <h2>{t.configProblems}</h2>
          <ul className="warnings">
            {overview.problems.map((p) => (
              <li key={p}>{p}</li>
            ))}
          </ul>
        </section>
      )}
      {actionError && <p className="error">{actionError}</p>}

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

        <h3>{t.queueTitle}</h3>
        {visibleJobs.length === 0 && <p className="muted">{t.queueEmpty}</p>}
        {visibleJobs.map((job) => {
          const active = job.status === "running" || job.status === "queued";
          return (
            <div className="job" key={job.id}>
              <p>
                <strong>{job.title}</strong> · {t.jobStatus[job.status]}
                {job.status === "queued" && job.queue_position ? ` · ${t.position(job.queue_position)}` : ""}
                {job.status === "running" ? ` · ${t.phase[job.phase] ?? job.phase}` : ""}
                {job.status === "running" && status?.job?.id === job.id && status.job.detail ? ` (${status.job.detail})` : ""}
                {job.quality ? ` · ${t.qualityNames[job.quality] ?? job.quality}` : ""}
                {job.elapsed_s != null ? ` · ${t.seconds(job.elapsed_s)}` : ""}
                {job.status === "running" && device ? ` · ${t.device} ${device}` : ""}
              </p>
              {job.params && job.status !== "queued" && (
                <p className="muted small-text">
                  {t.paramsLine(
                    String(job.params.dit_model ?? ""),
                    Number(job.params.inference_steps ?? 0),
                    Number(job.params.batch_size ?? 1),
                    job.params.lm_model ? String(job.params.lm_model) : null,
                    Boolean(job.params.thinking),
                  )}
                </p>
              )}
              {job.status === "running" && (
                <div className="bar progress">
                  <div style={{ width: `${Math.round(job.pct * 100)}%` }} />
                </div>
              )}
              {job.warnings.length > 0 && (
                <ul className="warnings">
                  {job.warnings.map((w) => (
                    <li key={w}>{w}</li>
                  ))}
                </ul>
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
              {active && (
                <button onClick={() => cancelJob(job.id)} disabled={job.cancelling}>
                  {job.cancelling ? t.cancelling : t.cancel}
                </button>
              )}
            </div>
          );
        })}

        <div className="row">
          <button onClick={restart}>{t.restartEngine}</button>
        </div>
        <details onToggle={(e) => (e.currentTarget.open ? loadLog() : undefined)}>
          <summary>{t.showLog}</summary>
          <button onClick={loadLog}>{t.refreshLog}</button>
          <pre className="log">{log && log.length ? log.join("\n") : t.logEmpty}</pre>
        </details>
      </section>

      <Library takes={takes} onTrash={trashTake} />

      <footer className="muted small-text">
        {t.platform} {info ? info.target : "—"}
      </footer>
    </main>
  );
}
