# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 專案

Laysi One 是台灣中小企業的財務＋人資正式產品。
評估用的示範資料只能由 `laysi-admin seed-demo` 手動種入（見 `docs/architecture.md`）。
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
cargo run -p laysi-server --bin laysi-admin -- seed-demo   # 示範資料，僅限拋棄式安裝

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
`laysi-store/build.rs` 會 `rerun-if-changed=../../migrations`——**沒有它，新增一個
migration 檔不會觸發重編**，伺服器會帶著舊的 migration 集合啟動且不報錯。

## 架構：四個關鍵設計

### 1. 操作註冊表：三個介面由同一份定義生成

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

### 4. 品牌完整性：不通過就拒絕啟動

`laysi-brand` 在 `main()` 的第一步驗證品牌關鍵檔案的 BLAKE3 與 Ed25519 簽章，
失敗即 `exit(78)`，連資料庫都不連。

**動到 `LICENSE-BSL.txt` 或 `laysi-brand/src/{glyphs,mark,integrity}.rs` 之後
一定要跑 `laysi-brandhash --update`**，否則伺服器起不來。錯誤訊息會把指令印
出來。私鑰在 `backend/brand/vendor.key`（已 gitignore）。

字標由 Rust 的參數化幾何即時算出，經 `GET /v1/brand/mark.svg` 提供。
**前端不得自帶 logo 檔案，也不得用文字重繪品牌名稱。**

## 資料層的三個不變量（由 SQL 強制）

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
  畫面文字包含後端 ops 產出的 `kicker`／`label`／`note`、`OpError` 訊息，
  以及 migration 種下的 `role.name`／`security_policy.detail` 這類會被畫面讀出
  的欄位——**只要看得到就是 UI 文案**，四處同一套語氣。六條判準：
  1. 講使用者要做什麼，不講系統處在什麼狀態；
  2. 只留下做決定需要、而畫面上看不到的資訊；
  3. 不寫理由（理由寫在 `docs/architecture.md`）；
  4. 不可逆的事要講，可逆的不必；
  5. 錯誤訊息寫「哪裡不對、怎麼修」，不寫「系統拒絕了，因為……」；
  6. 一句話講得完就不寫兩句。
  推論：內部識別字（操作名、資料表欄位、環境變數、`dispatch`／`Registry`
  這類函式與型別名）不出現在畫面上；`boss` 角色一律稱**負責人**，不稱「老闆」
  （0022 把種子資料一起改了），`admin` 稱**系統管理員**。
- **`page-lede` 只寫使用者需要知道、而畫面上看不到的事**，不寫「這頁有什麼」——
  標題已經說過了。純複述內容或解釋設計決策的一律刪掉。
- **數字用 `.num`，它有自己的 `--font-numeric`**（Source Serif 4，lining +
  tabular）。該規則排在所有元件規則之後，因為元件也設 `font-family`，同權重下由
  檔案順序決定勝負。
- **`web/src` 底下不寫註解**（`.ts`／`.tsx`／`.css` 皆然，`api/generated.ts` 由
  產生器自帶的兩行標頭除外）。理由與設計說明寫在 `docs/architecture.md`，不要
  複製一份到元件旁邊。後端 Rust 仍照上一條寫英文註解。
- **金額一律 `rust_decimal::Decimal`，絕不用 f64**；JSON 上是字串
  （`#[serde(with = "rust_decimal::serde::str")]`），前端也不轉成 number。
- 前端只能用 `web/src/styles/classical.css` 的 CSS 變數，**不得寫死 hex／字型／px**。
  設計語彙是「以線條與框線構圖，不用色塊填滿」，按鈕外框式，數字 tabular。
- 單租戶：資料表**不帶** `company_id`（已確認的產品決策）。

## 目前進度

已完成：資料庫不變量、法規引擎、操作註冊表、REST + MCP、品牌完整性、
RustFS 物件儲存，以及 **32 個畫面、97 個操作**，分成三個分頁
（`FINANCE_NAV`／`HR_NAV`／`ME_NAV`）加上左下角帳號列的系統按鈕（`SYSTEM_NAV`）：

- **財務 13 畫面**：總覽／傳票總帳／記帳抽屜、應收／應付／銀行、電子發票／稅務／
  固定資產、費用報支／預算／財務報表、文件管理／匯入整合。
- **系統 4 畫面**：基礎設定／角色核准／MCP／安全營運。原本掛在財務樹底下，
  但那是管理這套系統，不是財務工作。**它不是第四個分頁**：前三個是每天待著的
  地方，這個是偶爾去一次的地方，所以放在左下角帳號列旁邊，角色開不起任何一個
  畫面時連按鈕都不出現。
- **人資 7 畫面**：人資總覽／員工名冊／到離職異動、出勤與休假（含打卡與補登）、
  薪資作業／勞健保勞退、扣繳與法遵。
- **員工自助 8 畫面**：打卡／個人專區／休假申請／請款申請／我的休假／我的請款／
  我的薪資單／我的 MCP。分成「提出申請」與「查詢紀錄」兩組，因為填表與查進度是
  兩件事。

原型是 30 個畫面，但兩邊的數字不是同一組：人資段原先估為 9 個，目前的人資操作
攤在 7 個畫面上，差的 2 個沒有對應操作，**不確定原型上那兩個是什麼**；自助段
則多出休假申請、請款申請與打卡三個畫面。要補人資那兩個之前先回去讀
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
`role_permission → permission.op_names`，不快取。矩陣之外還有**兩道風險底線**
（0023）：`Risk::BossOnly` 綁 `boss`，管業務機密與權威（薪資明細、扣繳憑單、預算、
簽核門檻）；`Risk::AdminOnly` 綁 `admin`，管系統本身（權限矩陣、API Token、公司設定、
建立帳號）。**裝機的人不該因此看得到全公司薪資**，所以這兩件事是兩個角色。分離之所以
成立：底線在矩陣之外，`set_role_permission` 拉不動它；而且**沒有任何操作會改
`user_account.role_code`**，角色只在建帳號時決定，系統管理員要變負責人只能拿到 shell。
系統管理員另持 `manage_masters`（首次設定要建銀行帳戶與部門），該組裡的
`set_approval_rule` 仍擋在負責人底線後。
REST 身分只由 HttpOnly session cookie 查回 `user_account`，請求標頭不能指定
actor 或 role。MCP 身分由 Bearer token 所對應的 `api_token` 資料列決定，
請求標頭同樣無法覆寫 actor 或 role；撤銷後下一次呼叫立刻失效。
OIDC 四項設定齊全時採 discovery + Authorization Code + PKCE，且只接受管理員
預先連結的 issuer／subject，不依 email 比對也不自動建立帳號。

**第一個帳號可以在網頁上建立（`POST /v1/auth/bootstrap`，畫面 `screens/Init.tsx`）**，
它是 `/v1/auth/*` 例外的下一層：連要被檢查的身分都還不存在，所以守門條件是安裝
本身的狀態。保證來自 SQL 不是 Rust——0021 的 `install_bootstrap` 是單列表，
`bootstrap_first_user` 在同一個交易裡先 `INSERT ... WHERE NOT EXISTS (帳號)
ON CONFLICT (id) DO NOTHING` 再建帳號，所以併發只會成功一次，事後清空
`user_account` 也不會重新打開。角色寫死 `admin`（`laysi_ops::ROLE_ADMIN`）**不是
`boss`**、密碼由本人設定（沒有 `must_change_password`），呼叫端不能指定任何其他欄位。
負責人與其他成員的帳號之後由系統管理員用 `create_user_account` 在「角色與核准」建立；
那個操作不收密碼，密碼由系統產生、只回一次、強制首次登入變更。`SETUP_ALLOW_WEB=false`
關掉整條路，`SETUP_TOKEN` 非空時要求輸入代碼（行程內只留 BLAKE3 雜湊）。
未登入的判斷資料來自 `/v1/auth/me` 的 `setup` 欄位。**這不是 Registry 操作，
也不是導覽畫面**，所以不計入 97 個操作與 32 個畫面。

**人資模組不含任何費率**：勞健保勞退由 `laysi-regulation` 以**期末日**解析、
扣繳以**給付日**解析（扣繳義務發生在給付當下）。薪資過帳產生待過帳傳票
（借 6111 ／貸 2117、1102）；公司負擔的保費**不在那張傳票裡**，那條銜接還沒接上，
畫面照實標成未接上，不逕行併進 6111。加班費金額由差勤系統帶入，**系統不自己乘
倍數**，因為規則庫還沒收錄勞基法第 24 條；打卡推導出的加班同樣只有時數。

**換畫面只有一條路：`App.tsx` 的 `go()`**。它同時 `setScreen` 與 `pushState`；
直接 `setScreen` 的地方網址不會動，症狀是「只有落地那一頁有網址」。
`popstate` 的 handler 掛在 `[]` effect 上，判斷寫在每次 render 更新的 ref 裡，
不要直接閉包住 `permissions`／`landing`。

**網址就是頁面狀態**：`nav.ts` 的 `SCREEN_PATH` 對應 `ScreenId` ↔ 路徑，
`App.tsx` 用 History API，沒有 router 套件；`section` 由 `screen` 推導而不是自己
一份 state。落地畫面是**我的 → 打卡**（`SECTION_ORDER` 以 `me` 開頭），打卡另有
一顆常駐在我的分段 header 上的按鈕。

**打卡卡片是直向置中限寬的**：橫向 flex 會把按鈕推到右上——最常用的操作落在
最不容易看到的位置。順序是時鐘 → 按鈕 → 今日狀態，且只有一顆按鈕（上下班順序由
資料庫強制，兩顆裡永遠有一顆是死的）。備註在按鈕正上方展開，不另闢區塊。
補登的時間欄用純文字不用 `<input type="time">`：後者的 12／24 小時制跟著瀏覽器
語系走，也不收秒，而 `correct_punch` 收秒。

**彈性工時（0019）與遲到寬限是兩件事**：寬限原諒遲到但下班時間不動，彈性把整天
往後移、長度不變。`expected_end()` 因此吃「幾點到」，`overtime_seconds()` 收
`(arrived, left)`。早到不縮短當天。留空＝不採彈性工時。

**打卡（0016）記錄到秒**，時數與加班以秒計算、遲到在秒的精度上比較；
`correct_punch` 收 `HH:MM:SS`，只給分鐘時記為秒零。勞基法第 30 條第 7 項要求的是
「至分鐘為止」，記到秒是超過而非符合，所以程式與畫面都不宣稱這是法規要求。
**打卡與 `attendance` 月統計互不覆蓋**：打卡是原始事件，`attendance` 是
來源系統主張的彙總，操作把兩邊都算出來並回報差額。`punch_clock` **不收時間欄位**，
記的就是呼叫當下那一刻，連 `LAYSI_TODAY` 都不移動它；漏打只能由人資
`correct_punch` 補登，`source` 記成 `manual` 並在畫面上標示。上下班時間是公司政策，
放在 `company_setting` 的 `work_start`／`work_end`／`late_grace_minutes`，**空值時
不判定遲到**（`late()` 回 `Option<bool>`，`None` 表示沒問，不是「沒遲到」）。
資料庫用 `UNIQUE (employee_id, punched_on, kind)` 與一條比對 `+08:00` 本地日期的
CHECK 把不變量釘住；同一個時區常數是 `laysi_domain::BUSINESS_UTC_OFFSET_HOURS`，
`today()` 也已改用它（原本讀 UTC，本地 00:00–08:00 建的單據會記成前一天）。

`get_hr_compliance` 的 `checked` 與 `passed` 是兩件事，沒收錄的規則一律
`checked = false` 並附 `not_checked_reason`。

**員工自助的授權靠輸入形狀，不靠 `Risk`**：十二個自助操作**都不接受任何指定
對象的參數**，對象由可信 session 的 `ctx.employee_id` 決定。`selfservice.rs` 的
`no_self_service_input_can_select_another_employee` 把每個操作的 `Input` 欄位集合
逐字釘死：**加任何一個欄位都會讓測試紅**。`submit_my_expense_claim` 之所以不共用
`submit_expense_claim`，就是因為後者帶 `claimant`；自助版本另外不收費用科目
（留給會計那一關）也不收文件編號（職員沒有文件權限，填得進來的必然是別人的）。

**收據是送位元組，不是指既有文件（0020）**：`attach_my_expense_receipt` 收
`file_name` ＋ `content_base64`，輸入指不到任何既有文件。`claim_no` 指的是單、
單屬於人，所以 0020 給 `expense_claim` 加了 `employee_id` 並寫進 `WHERE`——
`claimant` 是顯示名稱、會同名，不能當寫入側的鍵；會計代建的單那一欄是 NULL，
自助這側看不到。只有 `submitted`／`approving` 收件。先建單再附件（`link_ref`
要單號），所以上傳失敗時單已存在，畫面要說哪幾個沒附上並請去「我的請款」補，
不要重送整張單。

**檔案內容不進稽核紀錄**：`dispatch` 寫 `ai_audit_log` 前先過 `redact_for_audit`，
超過 512 位元組的字串改記長度。判準是值的長度不是欄位名，新的帶位元組操作自動
涵蓋；是哪個檔案由 `document_version` 的 BLAKE3 回答（那是落地位元組的雜湊）。

**用 MCP 不是主管特權（0017、0018）**：`issue_my_api_token` 不收 `actor`／
`actor_role`，一律取自 session，所以那把 Token 能做的事與本人完全相同。所有權的鍵
是 `api_token.owner_employee_id`（`actor` 是顯示名稱、會同名，不能當鍵），只有自助
核發會寫入；`revoke_my_api_token` 把它寫進 `WHERE`，別人的 id 是「查無此列」。
同一欄也是 `mcp::token_identity` 取得 `employee_id` 的唯一來源——不從姓名猜，
老闆代發的 Token 沒有 owner，自助操作對它一律回「尚未連結員工資料」。0018 因此
才把五個唯讀自助工具對 AI 打開；寫入類與薪資明細維持關閉。

**新增操作時 migration 的 `mcp_tool_grant` 要同步**：`laysi-ops` 有測試以
`include_str!` 直接讀 0008／0009／0012／0014／0015／0016／0017，比對 Registry 與
grant 的名稱集合（0020 也在裡面）。0018 只 UPDATE 既有 grant、不新增，所以不在那份清單裡。這條測試是因為 0008 種了 15 筆 grant 卻只實作 10 個操作才補上的。
改寫既有 permission 的 migration（0015、0016、0017）在測試的重播清單裡要按順序排在最後，
理由與 PostgreSQL 最後套用它們相同。
