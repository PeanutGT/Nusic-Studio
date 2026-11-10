import { t } from "./i18n";

export type EngineError = { code: string; message: string; hint: string };

export type Style = {
  id: string;
  name: string;
  name_zh: string | null;
  description_zh: string | null;
  loopable: boolean | null;
  duration: { default?: number; max?: number } | null;
};
export type Mood = { id: string; label_zh: string | null };
export type Instrument = { id: string; label: string };
export type Overview = {
  styles: Style[];
  moods: Mood[];
  workflows: { id: string; name_zh: string | null; name: string | null }[];
  instruments: Instrument[];
  presets: Record<string, Record<string, unknown>>;
  loudness_presets: LoudnessPreset[];
  default_quality: string;
  tier: string;
  tier_caps: { max_duration?: number; max_batch?: number; lm_model?: string | null } | null;
  problems: string[];
};

export type PlanInput = {
  style: string;
  description: string;
  moods: string[];
  quality?: string;
  duration?: number;
  seed?: number;
  caption_extra?: string;
};
export type Params = Record<string, string | number | boolean | null>;
export type Spec = {
  title: string;
  caption: string;
  bpm: number | null;
  key: string | null;
  duration: number;
  quality: string | null;
  warnings: string[];
  instruments: { use: string[]; avoid: string[] };
};
export type Plan = { spec: Spec; params: Params; tier: string; quality: string; warnings: string[] };

export type QueueJob = {
  id: string;
  title: string;
  status: "queued" | "running" | "done" | "failed" | "cancelled";
  spec: { caption?: string; style?: string; duration?: number };
  phase: string;
  pct: number;
  warnings: string[];
  error: EngineError | null;
  take_id: string | null;
  quality: string | null;
  params: Params | null;
  elapsed_s: number | null;
  cancelling: boolean;
  queue_position: number | null;
};
export type QueueView = { jobs: QueueJob[]; current: string | null };

export type LoudnessPreset = { id: string; label: string | null; lufs: number; true_peak_db: number | null };
/** One automatic-check finding; `at` is seconds into the track. */
export type Issue = { code: string; at: number; end?: number; count?: number; rms_db?: number; hf_pct?: number };
export type TakeExport = {
  variant: number;
  base: string;
  created: string;
  files: string[];
  lufs: number | null;
  true_peak_db: number | null;
  qc: string[];
  issues: Issue[];
  warnings: string[];
};

export type Take = {
  id: string;
  title: string;
  created: string;
  style: string | null;
  quality: string | null;
  duration: number | null;
  bpm: number | null;
  key: string | null;
  variants: number;
  dir: string;
  files: string[];
  checks: { index: number; file: string; qc: Issue[] | null }[];
  exports: TakeExport[];
  picked: number | null;
};

export function errorText(e: unknown): string {
  const err = e as Partial<EngineError>;
  if (err && typeof err === "object" && err.code) {
    return [t.errors[err.code] ?? "", err.message].filter(Boolean).join(" ");
  }
  return String(e);
}
