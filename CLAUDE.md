# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 專案

Laysi One 是台灣中小企業的財務＋人資正式產品，不預載任何特定公司的營運資料。
Rust（axum + sqlx）＋ React（Vite + TS）＋ PostgreSQL 17 ＋ RustFS ＋ Docker Compose。
授權為 **BSL 1.1**（見 `LICENSE-BSL.txt`），不是開源授權。

產品規格的權威來源是 Claude Design 專案「台灣中小企業財務系統原型」的
`財務系統原型.dc.html`（30 個畫面、3 種身分視角）。跨層契約寫在
**`docs/architecture.md`**：動到型別、schema、端點或操作清單之前先讀它，
改動要同步更新那份文件。

## 常用指令

```bash
# 後端（在 backend/ 下）
cargo test --workspace                 # 全部測試
cargo test -p laysi-regulation         # 單一 crate
cargo test -p laysi-ops registry_holds # 單一測試（比對名稱前綴）
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p laysi-server              # 需要 DATABASE_URL
cargo run -p laysi-server --bin laysi-admin -- list-users

# 從操作註冊表重新產生前端型別（改過 laysi-ops 就要跑）
cargo run -p laysi-server --bin laysi-schema -- ../web/src/api/generated.ts

# 品牌完整性（改過品牌檔就要跑，否則伺服器拒絕啟動）
cargo run -p laysi-brand --bin laysi-brandhash -- --verify
cargo run -p laysi-brand --bin laysi-brandhash -- --update

# 前端（在 web/ 下）
npm run dev        # 5173，proxy /v1 與 /mcp 到 localhost:8080
npm run build      # tsc --noEmit && vite build
npm run typecheck

# 整套部署
cp .env.example .env && docker compose up -d --build   # 前端在 :8088
```

本機開發不需要 `sqlx-cli`：migrations 由 `sqlx::migrate!()` 在啟動時套用，
查詢一律用**執行期** `sqlx::query`（不是 `query!` 巨集），所以編譯不需要資料庫。

## 架構：四個不明顯但關鍵的設計

### 1. 操作註冊表：「所有操作支援 MCP」是結構保證，不是紀律

`laysi-ops` 的 `Registry` 是唯一的操作定義處。REST（`POST /v1/ops/{name}`）、
MCP（`tools/list` / `tools/call`）、TypeScript 型別三個介面都由它生成。

**新增功能時不要另外手寫 axum 路由**：實作 `Operation` trait 並註冊進
`Registry::new()`，三個介面自動都有。`crates/laysi-server/tests/parity.rs`
會確認三者名稱集合相等。

所有呼叫都必須經過 `Registry::dispatch`，它的順序不可調換：
工具開關 → 風險檢查 → 角色權限 → 執行 → 寫稽核。**被擋下的呼叫也要寫稽核**。

### 2. Channel：工具開關管的是 AI，不是人

`OpCtx.channel` 分 `Ui` 與 `Mcp`。`mcp_tool_grant` 只對 `Mcp` 生效。

會計在畫面上按「過帳」，和 AI 自己決定呼叫 `post_voucher`，是兩件事。把
`post_voucher` 對 AI 關掉，不應該讓會計也不能過帳。加新操作時想清楚它在
兩個 channel 下各自該是什麼行為。

### 3. 法規數值只能存在於 TOML

**任何法規數字（費率、級距、上限、天數）都不得出現在 Rust 原始碼中。**
它們住在 `backend/regulation/<年度>/<domain>.toml`，由 `laysi-regulation`
以 `include_str!` 編譯期嵌入。

- 計算子一律接收 `&Ruleset`，而 ruleset 是用 `as_of` 日期解析出來的。
  沒有「當前費率」，只有「某日適用的費率」。
- `resolve()` 找不到適用規則時**回錯誤，絕不回退到預設值**。
- 法規更新 = 新增一份 `effective_from` 較晚的 TOML，不改程式。

`laysi-regulation` 的測試把原型畫面上的實際數字（3527／2647／822／1008／3180）
當基準；對不上就是計算子錯了。

### 4. 品牌完整性採硬性策略：不通過就拒絕啟動

`laysi-brand` 在 `main()` 的第一步驗證品牌關鍵檔案的 BLAKE3 與 Ed25519 簽章，
失敗即 `exit(78)`，連資料庫都不連。

**動到 `LICENSE-BSL.txt` 或 `laysi-brand/src/{glyphs,mark,integrity}.rs` 之後
一定要跑 `laysi-brandhash --update`**，否則伺服器起不來。錯誤訊息會把指令印
出來。私鑰在 `backend/brand/vendor.key`（已 gitignore）。

字標由 Rust 的參數化幾何即時算出，經 `GET /v1/brand/mark.svg` 提供。
**前端不得自帶 logo 檔案，也不得用文字重繪品牌名稱。**

## 資料層的三個不變量（由 SQL 強制，不只靠 Rust）

`migrations/0001_initial.sql`：

1. `voucher_line`：金額非負、一行不可同時有借有貸（CHECK）
2. 每張傳票借貸相等且至少兩行（**deferred** constraint trigger，交易結束才檢查）
3. 已過帳傳票不可 UPDATE／DELETE，分錄也不可異動；只允許 `posted → void`

因為 MCP 與未來的匯入程式都可能繞過應用層，資料庫必須是最後一道防線。

**後果**：建立傳票時要先寫 `pending`、分錄齊備後再更新為 `posted`。
`post_voucher` 走這條路徑；直接插入 `status='posted'` 的傳票會被 trigger 擋下。

## 慣例

- **程式碼註解與文件字串一律英文**（Rust `//` `///` `//!`、SQL `--`、TS `//`
  `/** */`），識別字本來就是英文。註解寫事實與不明顯的約束，不寫論說文：不用
  `##` 修辭小標、不用 `**粗體**`、不用 `── ─────` 橫幅、不寫開發過程與第一人稱。
- **UI 文案一律繁體中文，語氣為清晰簡潔的商務風格**。不使用原型的口語腔
  （「今天的帳，還好嗎？」「別人還沒付我」已全數改為「財務總覽」「應收帳款」）。
  畫面文字包含後端 ops 產出的 `kicker`／`label`／`note`，兩邊同一套語氣。
- **金額一律 `rust_decimal::Decimal`，絕不用 f64**；JSON 上是字串
  （`#[serde(with = "rust_decimal::serde::str")]`），前端也不轉成 number。
- 前端只能用 `web/src/styles/classical.css` 的 CSS 變數，**不得寫死 hex／字型／px**。
  設計語彙是「以線條與框線構圖，不用色塊填滿」，按鈕外框式，數字 tabular。
- 單租戶：資料表**不帶** `company_id`（已確認的產品決策）。

## 目前進度

已完成並驗證：資料庫不變量、法規引擎、操作註冊表、REST + MCP、品牌完整性、
RustFS 物件儲存，以及 **28 個畫面、87 個操作**，分成三段導覽（`FINANCE_NAV`／
`HR_NAV`／`ME_NAV`）：

- **財務 17 畫面**：總覽／傳票總帳／記帳抽屜、應收／應付／銀行、電子發票／稅務／
  固定資產、費用報支／預算／財務報表、文件管理／匯入整合、基礎設定／角色核准／
  MCP／安全營運。
- **人資 7 畫面**：人資總覽／員工名冊／到離職異動、出勤與請假、薪資作業／
  勞健保勞退、扣繳與法遵。
- **員工自助 4 畫面**：我的首頁／我的請款／我的薪資單／我的請假。

原型是 30 個畫面，人資段原先估為 9 個。目前的 15 個人資操作攤在 7 個畫面上，
差的 2 個沒有對應操作，**不確定原型上那兩個是什麼**。要補之前先回去讀
`財務系統原型.dc.html`，不要自行補兩個畫面。

**財務報表由總帳現算，系統裡沒有一張報表數字表**。新增科目時要在
`account.report_group` 與 `cash_flow_section` 指定它出現在報表哪一段，
否則 CHECK 會擋下來。三條不變量（資產＝負債＋權益、兩張表的本期損益一致、
期末現金＝現金科目餘額）都有測試，見 `docs/architecture.md`。

**預算與報支同一條原則**：`budget` 只存預算數，實際數一律現查 `voucher_line`；
簽核關卡由 `approval_rule` 的金額門檻算出來，`ops/expenses.rs` 裡沒有任何金額。

**文件內容在 RustFS，資料庫只留物件鍵與 BLAKE3**。先配編號、檔案進桶子成功
才寫資料庫；取回時一定重算雜湊比對，對不上就不交出內容。**沒有 OCR 引擎**，
`ocr_status` 的 CHECK 只收 `pending` 與 `not_applicable`。

**角色權限由 `dispatch` 即時強制 deny-by-default**：每次都查
`role_permission → permission.op_names`，不快取；`Risk::BossOnly` 仍是額外的風險限制。
REST 身分只由 HttpOnly session cookie 查回 `user_account`，請求標頭不能指定
actor 或 role。MCP 身分由 Bearer token 所對應的 `api_token` 資料列決定，
請求標頭同樣無法覆寫 actor 或 role；撤銷後下一次呼叫立刻失效。
OIDC 四項設定齊全時採 discovery + Authorization Code + PKCE，且只接受管理員
預先連結的 issuer／subject，不依 email 比對也不自動建立帳號。

**人資模組不含任何費率**：勞健保勞退由 `laysi-regulation` 以**期末日**解析、
扣繳以**給付日**解析（扣繳義務發生在給付當下）。薪資過帳產生待過帳傳票
（借 6111 ／貸 2117、1102）；公司負擔的保費**不在那張傳票裡**，那條銜接還沒接上，
畫面照實標成未接上，不逕行併進 6111。加班費金額由差勤系統帶入，**系統不自己乘
倍數**，因為規則庫還沒收錄勞基法第 24 條。`get_hr_compliance` 的 `checked` 與
`passed` 是兩件事，沒收錄的規則一律 `checked = false` 並附 `not_checked_reason`。

**員工自助的授權靠輸入形狀，不靠 `Risk`**：五個 `*_my_*` 操作**都不接受任何指定
對象的參數**，對象由可信 session 的 `ctx.employee_id` 決定。`selfservice.rs` 的
`no_self_service_input_can_select_another_employee` 把每個操作的 `Input` 欄位集合
逐字釘死：**加任何一個欄位都會讓測試紅**。

**新增操作時 migration 的 `mcp_tool_grant` 要同步**：`laysi-ops` 有測試以
`include_str!` 直接讀 0008／0009／0012／0014，比對 Registry 與 grant 的名稱集合。
這條測試是因為 0008 種了 15 筆 grant 卻只實作 10 個操作才補上的。
