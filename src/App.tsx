import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { t } from "./i18n";
import "./App.css";

type AppInfo = { version: string; target: string };

export default function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState(false);

  useEffect(() => {
    invoke<AppInfo>("get_app_info").then(setInfo, () => setError(true));
  }, []);

  return (
    <main className="shell">
      <h1>{t.appName}</h1>
      <p className="tagline">{t.tagline}</p>
      <p className="notice">{t.skeletonNotice}</p>
      <dl className="info">
        <dt>{t.version}</dt>
        <dd>{info ? info.version : error ? t.backendUnavailable : t.loading}</dd>
        <dt>{t.platform}</dt>
        <dd>{info ? info.target : "—"}</dd>
      </dl>
    </main>
  );
}
