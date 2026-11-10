# Nusic Studio

本機 AI 音樂工作室桌面 app（Windows + NVIDIA）。由 [music-studio skill](https://github.com/PeanutGT/NutsSkills/tree/main/skills/music-studio) 重寫而來：Tauri 2（Rust）+ React，Python 只負責執行 ACE-Step。

> 目前是 **A3 後製與品檢**：每次生成後自動檢查每個版本（爆音、咔噠聲、中間靜音、斷尾、幾乎無聲、音量太小、發悶），作品庫可以挑一個版本照工作流程後製並匯出 wav／flac／mp3／ogg；響度是一次線性增益（EBU R128 量測，真峰值上限保留 0.5 dB 餘裕），不壓縮動態。音訊全部在 Rust 處理，不需要 ffmpeg。介面仍是精簡版，完整介面在 A4。

## 里程碑

| | 內容 | 狀態 |
|---|---|---|
| A0 | 骨架：Tauri + React、Windows CI、安裝檔 | ✅ |
| A1 | 引擎橋接：啟動／監控／取消 ACE-Step worker | ✅ |
| A2 | 核心搬家：設定、參數目錄、風格／情緒／詞庫、規劃、佇列、作品庫 | 🛠 已合併，待實測 |
| A3 | 後製與品檢：線性增益響度、品檢、匯出 | 🚧 開發中 |
| A4 | 完整介面：建立頁進階、佇列、作品庫試聽、設定頁參數表 | ⬜ |
| A5 | 安裝與發佈：首次啟動精靈、安裝檔、Releases、自動更新 | ⬜ |

🚧 開發中、🛠 已合併待 Peanut 實測、✅ 驗收通過。驗收條件沿用 NutsSkills `skills/music-studio/progress.md` 的「App 路線」，之後進度只記在這裡。

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
| `src-tauri/` | Rust 後端（Tauri 2）；`src/engine/` 啟動／監控／取消 Python worker；`src/studio/` 是從 skill 移植的設定、參數目錄、風格庫、規劃、佇列、作品庫；`src/studio/post/` 是後製、響度與品檢 |
| `src-tauri/resources/defaults/` | 內建資料：設定、參數目錄、風格、情緒、流程、詞庫（取自 skill v3.7）。使用者的覆寫放在作品資料夾的 `config.json`、`styles/` 等，和 skill 相同 |
| `src-tauri/resources/ace_worker.py` | 跑 ACE-Step 的常駐 worker（取自 skill v3.7，JSON lines 走 stdin／stdout） |
| `golden/` | v3.7 skill 的對照資料，給 Rust 版對照測試用（見 `golden/README.md`） |
| `tools/export_golden.py` | 從 skill 重新匯出對照資料 |

CI（`.github/workflows/ci.yml`）在 Windows 上跑型別檢查、`cargo test`，並建置安裝檔，安裝檔可在該次 Actions 執行的 Artifacts 下載。

## 授權

Apache-2.0。匯出 mp3 用的 LAME 以原始碼形式靜態連結（LGPL-2.0；本專案原始碼公開，可自行換掉 LAME 重新建置）；其他音訊套件為 MIT／Apache-2.0／BSD／MPL-2.0。
