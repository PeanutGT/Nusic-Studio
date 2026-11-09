// All user-facing strings live here so an English table can be added later
// without touching components.
export const zhTW = {
  appName: "Nusic Studio",
  tagline: "本機 AI 音樂工作室",
  skeletonNotice: "這是 A0 骨架版本：還不能生成音樂。",
  version: "版本",
  platform: "平台",
  loading: "讀取中…",
  backendUnavailable: "無法連到後端",
} as const;

export type Strings = typeof zhTW;
