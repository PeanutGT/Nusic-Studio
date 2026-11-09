# Nusic Studio

本機 AI 音樂工作室桌面 app（Windows + NVIDIA）。由 [music-studio skill](https://github.com/PeanutGT/NutsSkills/tree/main/skills/music-studio) 重寫而來：Tauri 2（Rust）+ React，Python 只負責執行 ACE-Step。

> 目前是 **A0 骨架**：能開出視窗，還不能生成音樂。路線圖見 NutsSkills `skills/music-studio/progress.md` 的「App 路線」。

## 開發

需要 Node.js 22、Rust stable，以及 [Tauri 的系統需求](https://tauri.app/start/prerequisites/)。

```bash
npm install
npm run tauri dev        # 開發模式
npm run tauri build      # 建置 Windows 安裝檔（NSIS）
cd src-tauri && cargo test
```

## 結構

| 路徑 | 內容 |
|---|---|
| `src/` | React + TypeScript 前端；介面字串集中在 `src/i18n/` |
| `src-tauri/` | Rust 後端（Tauri 2） |
| `golden/` | v3.7 skill 的對照資料，給 Rust 版對照測試用（見 `golden/README.md`） |
| `tools/export_golden.py` | 從 skill 重新匯出對照資料 |

CI（`.github/workflows/ci.yml`）在 Windows 上跑型別檢查、`cargo test`，並建置安裝檔，安裝檔可在該次 Actions 執行的 Artifacts 下載。

## 授權

Apache-2.0
