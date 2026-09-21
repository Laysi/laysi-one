# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 規範
用戶並不喜歡文字敘述，設計時以ui設計取代文字敘述，這可以大幅提升使用者體驗。

## 專案

Laysi One 是台灣中小企業的財務＋人資正式產品。
評估用的示範資料只能由 `laysi-admin seed-demo` 手動種入（見 `docs/architecture.md`）。
Rust（axum + sqlx）＋ React（Vite + TS）＋ PostgreSQL 18 ＋ RustFS ＋ Docker Compose。
授權為**專有商業授權**（見 `LICENSE.txt`），由代理商與經銷商販售，不是開源授權。

產品規格的權威來源是 Claude Design 專案「台灣中小企業財務系統原型」的
`財務系統原型.dc.html`（30 個畫面、3 種身分視角）。跨層契約寫在
**`docs/architecture.md`**：動到型別、schema、端點或操作清單之前先讀它，
改動要同步更新那份文件。

## 常用指令

```bash
# 後端（在 backend/ 下）
cargo test --workspace                 # 全部測試
cargo fmt --check                      # CI 會擋，格式漂移要先修
cargo test -p laysi-regulation         # 單一 crate
cargo test -p laysi-ops registry_holds # 單一測試（比對名稱前綴）
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p laysi-server --bin laysi-server   # 需要 DATABASE_URL
cargo run -p laysi-server --bin laysi-admin -- list-users
cargo run -p laysi-server --bin laysi-admin -- seed-demo   # 示範資料，僅限拋棄式安裝

# 從操作註冊表重新產生前端型別（改過 laysi-ops 就要跑）
cargo run -p laysi-server --bin laysi-schema -- ../web/src/api/generated.ts

# 端對端（需要容器起著；第二支沿用第一支留下的資料庫）
bun scripts/function-test.ts     # 算出來的數字，以及每一次回應對不對得上前端 schema
bun scripts/schema-live-test.ts  # 逐一核對唯讀操作的回應

# 前端（在 web/ 下）
npm run dev        # 5173，proxy /v1 與 /mcp 到 localhost:8080
npm run build      # tsc --noEmit && vite build
npm run typecheck

# 整套部署（不需要 .env，沒有任何值要人自己想）
docker compose up -d --build     # 前端在 :8088
                                 # 初始化代碼寫在 setup/初始化代碼.txt

# 開發時只起資料庫與物件儲存，伺服器與前端跑在本機（不必重建映像）
docker compose -f docker-compose.yml -f docker-compose.dev.yml up -d postgres rustfs

# 同一件事的腳本版：容器＋伺服器＋Vite，Ctrl-C 只停腳本起的那兩個
scripts/dev            # 伺服器改動後由 cargo-watch 自動重建重啟（WATCH=0 關掉）
scripts/dev api        # 只起容器與伺服器
scripts/dev stop       # 停掉兩個容器
```

`scripts/dev` 需要 `cargo install cargo-watch`；沒裝就只跑一次，改完要自己重啟。

改容器裡的程式要 `--force-recreate`：`--build` 只重建映像，不會換掉已存在的容器。

本機開發不需要 `sqlx-cli`：migrations 由 `sqlx::migrate!()` 在啟動時套用，
查詢一律用**執行期** `sqlx::query`（不是 `query!` 巨集），所以編譯不需要資料庫。
`laysi-store/build.rs` 會 `rerun-if-changed=../../migrations`——**沒有它，新增一個
migration 檔不會觸發重編**，伺服器會帶著舊的 migration 集合啟動且不報錯。

**上線前的每一份 migration 都合併進 `0001_initial.sql` 了**，前後三輪，來龍去脈在
git 歷史裡（`git log --diff-filter=D -- backend/migrations`）。
合併只有在還沒有安裝要沿用時做得了；**0001 一旦被套用過就不再更動**——改一個位元組，
現有的資料庫就會拒絕啟動；schema 要變就新增下一個編號的檔案，那也是 0001 之後每一份
migration 的寫法。

**後續 migration 也種 `permission` 與 `mcp_tool_grant` 時要整列重寫**（`ON CONFLICT
DO UPDATE`），並把檔案接到 `laysi-ops` 測試的 `SCHEMA` 常數後面：那三個測試把種子資料
當字串讀，後面的列蓋掉前面的，與資料庫裡後一道敘述蓋掉前一道是同一件事。

**文件不寫 migration 編號**：合併過三輪之後同一個四位數指過三份不同的 migration，
而指出去的檔案一份都不存在。要追某個決定的來由，用 `git log -S` 找那段程式或那個
欄位名。

## 架構：五個關鍵設計

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

### 3. 法規數值只能存在於 `reg_ruleset` 資料表

**任何法規數字（費率、級距、上限、天數）都不得出現在 Rust 原始碼中。**
它們住在 `reg_ruleset`，畫面在「法規版本」，操作是 `save_regulation`／
`delete_regulation`（`AdminOnly`）。

- 計算子一律接收 `&Ruleset`，而 ruleset 是用 `as_of` 日期解析出來的。
  沒有「當前費率」，只有「某日適用的費率」。
- `resolve()` 找不到適用規則時**回錯誤，絕不回退到預設值**。所以
  `delete_regulation` 拒絕刪掉一個領域的最後一版。
- 法規更新 = 新增一版 `effective_from` 較晚的規則，不改程式、不發版、不重啟。
  `save_regulation` 寫完就重讀重裝。

**廠商送新版走 `refresh_regulations`，不走 `migrations/` 也不換映像**：費率是資料不是
schema，而 0001 已經合併過三輪 migration，下一次合併時一則費率修正會擋在路上；換一版
映像只為了改一個數字，等於把上線時程綁在發版時程上。那個操作（`AdminOnly`，畫面在
「法規版本」）向 `https://raw.githubusercontent.com/Laysi/laysi-one-regulation-feed/main/manifest.json` 取，套用過的
識別字記在 `reg_update`，**一個資料庫只套用一次**——所以管理員刪掉的版本不會在下一次
更新回來，而這正是 `seed_regulations()` 那條「只在表空時寫」要守的性質；缺口只有出廠
那一批（`seed_regulations()` 不留記錄），在第一次更新之前刪掉會回來一次。
**送的是 payload 不是 SQL**：發布端供應值，敘述由伺服器自己組，所以那個網址碰不到
`reg_ruleset` 以外的表；每一份 payload 寫入前都走 `probe()`，與手打進去的同一道檢查。
一律 `ON CONFLICT (domain, version) DO NOTHING`。**`file` 是 `reg_update` 的鍵，發布
之後不能改**——換名字等於要每個安裝再套用一次，包括刻意刪掉那一版的那些。
`manifest.json` 由 `scripts/regulation-manifest.ts` 從 `backend/regulation/**/*.toml`
產出（`.github/workflows/regulation-feed.yml` 推到發布用的公開儲存庫），不要手抄
payload：同一份 TOML 對新安裝是出廠預設值，對既有安裝是更新。`reg_ruleset` 沒有
`checksum` 欄位：寫入時算的 FNV-1a，讀取端從來不取它。

**`laysi-regulation` 仍然不碰 SQL**：伺服器啟動時把整張表交給 `install()`，
`resolve()` 回 `&'static Ruleset`，計算子維持同步純函式，`cargo test` 不需要資料庫。
`backend/regulation/<年度>/<domain>.toml` 只是**出廠預設值**，`seed_regulations()`
**只在表空的時候寫入**——逐列補寫會讓刪掉的舊版在下一次重新載入時自己回來。

`laysi-regulation` 的測試把原型畫面上的實際數字（3527／2647／822／1008／3180）
當基準；對不上就是計算子錯了。

**存進去之前先算一次**：`save_regulation` 用該領域自己的計算子跑 `probe()`，
少一個欄位的 payload 在存檔當下就被擋下，而不是等到發薪那天。畫面上只有字串葉節點
編得動（欄位名與排序共用 `ops/regulations.rs` 的 `FIELDS`，識別字不進畫面）；
改結構走 `save_regulation` 本身，它收整份 `payload`。

### 4. 產品字標是文字，公司標誌是設定

「Laysi One」由 `components/BrandMark` 以 Inter 排出來（`--font-sans` 已經是
Inter），三個尺寸 `sm`／`md`／`lg` 對應 header、側邊欄與登入面板。原本的
`mark.svg`（字標）與 `monogram.svg`（方形的 L）連同 `BrandMark` 的載入失敗分支一起
刪掉；`favicon.svg` 留著，它是圖示不是字標，用文字排不出來。

字標原本由 Rust 即時組出來，並在啟動時驗 BLAKE3 與 Ed25519 簽章、不過就
`exit(78)`。那套機制連同 `laysi-brand` crate、`backend/brand/` 的金鑰、
`/v1/brand/*.svg` 兩條路由、`/healthz` 的 `brand`、`/v1/meta` 的 `brand_build`
與安全營運頁的「品牌完整性」卡片一起移除。

**公司自己的標誌是兩列 `company_setting`**：`brand_logo` 存
`data:image/…;base64` 的圖檔內容，`brand_display` 是「名稱」／「標誌」／
「標誌與名稱」。畫在兩個地方——登入面板與側邊欄左上角，共用
`components/CompanyBrand`；`sidebar__company` 那一段換成它。
**圖檔進 `company_setting` 而不是進桶子**：登入頁沒有 session，而
`company_setting` 已經是那個瀏覽器唯二讀得到的兩個端點讀的東西，放進桶子就要為它
再開一條公開路由。代價是它跟著 `/v1/auth/me` 與 `/v1/meta` 每次載入走一遍，所以
`MAX_BRAND_LOGO_BYTES` 是 512 KB，而顯示方式不用標誌時後端回空字串。
`validate_setting` 只收 PNG、JPEG、WebP 與 SVG 的 data URI——那個字串會直接進
簽入頁的 `<img src>`。沒有標誌時後端把顯示方式降回「名稱」，畫面也不畫那個下拉。
`update_company_setting` 的稽核摘要對 `brand_logo` 寫「已更換」不寫值：
`redact_for_audit` 只碰輸入，摘要是自己寫的。

`favicon.svg` 畫的是兩道等長的粗弧繞成一個環，互為 180 度旋轉，中間留兩道缺口：一筆分錄的借方與
貸方等量，而兩邊不相接。終端切平（butt）但不沿半徑切，
內緣比外緣退後 16 度，所以兩道缺口是同向的斜槽而不是兩條輻線。**沒有底板**，
`manifest.json` 指同一個檔。原本畫的是一個圓被一道 S 分成兩塊等面積的深藍與金，
但藍配黃再加一道 S 就是 Python 的標記；缺口取代 S，兩塊之間不再有那條交扣的曲線。
沒有底板就有一條硬約束：**底色不再幫忙撐任何一塊**。這裡的解法是讓兩塊在兩種分頁列上
互相接手：白底由深藍 `#234b7d` 撐（11.5:1，金只有 1.9:1），深色分頁列由金 `#f0b429`
撐（8.6:1，深藍只有 1.8:1），兩塊之間 4.8:1，所以缺口在哪一邊都切得開。
**金取 `#f0b429` 而不是畫面上的 accent `#b68235`**：色相幾乎沒動（36 度到 42 度），
彩度由 55% 拉到 87%。`#b68235` 彩度太低，貼著藍會被同時對比推成褐色——兩塊濁色相鄰
就是一團泥；它躺在紙色底上沒有這個問題，所以兩者是同一個色相的兩階，不是兩個顏色。
同一條約束也是墨色配金色行不通的原因（墨色那半在深色分頁列上消失），以及藍配綠要先把
兩者亮度拉開才行的原因（同亮度會糊成一塊）。
**不要改成隨主題切換**——分頁列的深淺跟著作業系統走而不是網站，而 Chrome 不讀 SVG
favicon 裡的 `prefers-color-scheme`（只有 Firefox 讀，症狀是換主題圖示不動）。
選旋轉對稱是因為 16px 讀得出來的幾何形狀本來就少，而正交的那些早被工具列圖示佔走：
橫線＝減號／選單、完整的環＝零、兩個位移方塊＝複製、翻角紙＝檔案、圓加斜線＝禁止。
實心的四葉飾與六瓣花在 16px 會塌成一團黑，要有對比色透出來的縫才讀得出結構。

### 5. 授權是簽章，不是資料庫裡的一列

授權伺服器在同一個 repo 的 `license-server/`，但**是另一個部署也是另一個 cargo
workspace**：它多租戶、經銷商與代理商在那裡開立啟動金鑰，本產品單租戶、資料表不帶
`company_id`，資料庫與程序都不合併。同一個 repo 的理由只有一個——授權檔的 payload
（`laysi-license-token`）兩邊共用一份定義，欄位漂掉是編譯錯誤而不是客戶端解不出來。
它跑得動 `cargo test` 也是因此才進得了 CI。

`license_state` 單列表存三樣東西：本安裝抽出的 `install_id`、客戶輸入的啟動金鑰，以及
授權伺服器用 Ed25519 簽回來的授權檔。**畫面上每一個授權欄位都從簽章 payload 讀出**
（`ops/license.rs` 的 `evaluate`），不從欄位讀——改資料庫改不動任何人看得到的東西。公鑰
編在 `PUBLIC_KEY_HEX`（`ed25519-dalek` 本來就在依賴樹裡，由 `openidconnect` 帶進來），
私鑰只在授權伺服器上。

**租期只有一個日期**：`not_after`。每次檢查成功由伺服器續 45 天，整套狀態機只看它
（`> 30 天`＝使用中、`0～30`＝即將到期、`-30～0`＝寬限、更早＝唯讀）。`expires_on` 是
合約到期日，只給人看，不做任何判斷。**合約到期時伺服器仍然發授權檔**（`not_after` 是
過去），畫面才說得出到期日與該找誰；403 帶不了這兩件事。

**到期是唯讀，不是鎖死**。`dispatch` 的第 0 步問 `op.writes()`，不是 `risk()`——
`list_payslips` 帶著負責人底線卻不寫入，那是「誰可以叫」不是「改了什麼」，兩者混在一起
會讓失效的安裝連自己的薪資單都印不出來。少數帶身分底線的讀取要自己覆寫
`writes()`，`reads_are_not_writes_whatever_floor_they_carry` 依命名把關。`activate_license`
是唯一豁免的寫入：它是離開這個狀態的路。

**主機識別碼綁的是主機，不是資料庫**：`fingerprint()` 取 `/etc/machine-id`，取不到才退回
`install_id`。**不把兩者混在一起**——那會讓同一台伺服器重裝變成第二台主機，每次重建都要
請經銷商解除綁定。compose 把 `/etc/machine-id` 掛進 `api` 與 `mcp` 兩個服務，**兩個都要
掛**，不然它們對同一個資料庫算出兩個識別碼，其中一個會認為自己未授權。

**啟用在建立系統管理員帳號之前**（`POST /v1/auth/bootstrap` 收 `activation_key`，
`screens/Init.tsx` 是第一個欄位）：驗不過就什麼都不建，安裝維持原狀。
**導入前就存在的安裝有 90 天寬限**（0007 看得到 `install_bootstrap` 才寫 `grace_until`），
不然升級當天所有既有客戶一起變唯讀。

**每天回報三個欄位**：授權編號、主機識別碼、系統版本。主機識別碼是 `/etc/machine-id`
的 BLAKE3，認得出是不是同一台機器，認不出那是誰；**在職人數曾經是第四項，拿掉了**——
那是客戶的營運數字，而授權判斷從來不看它。帳務與人事資料一個都不外傳。

**沒有離線授權檔**：一份撤銷不了也看不見的授權檔，加上一個收不到 `refresh_regulations` 與 `refresh_work_calendar`、費率永遠停在
出廠映像那一版的安裝，兩件事都不該賣出去。理由寫在 `docs/architecture.md`。

## 資料層的四個不變量（由 SQL 強制）

`migrations/0001_initial.sql`：

1. `voucher_line`：金額非負、一行不可同時有借有貸（CHECK）
2. 每張傳票借貸相等且至少兩行（**deferred** constraint trigger，交易結束才檢查）
3. 已過帳傳票不可 UPDATE／DELETE，分錄也不可異動；只允許 `posted → void`
4. 同一人的休假單彼此不得在時間上重疊，加班申請亦然（`EXCLUDE USING gist`）

因為 MCP 與未來的匯入程式都可能繞過應用層，資料庫必須是最後一道防線。

**後果**：建立傳票時要先寫 `pending`、分錄齊備後再更新為 `posted`。
`post_voucher` 走這條路徑；直接插入 `status='posted'` 的傳票會被 trigger 擋下。

**第四條的三個細節**：時段是半開區間，所以前一張到 18:00、後一張從 18:00 開始不算
重疊；**被駁回的單不在約束範圍內**（`WHERE status <> 'rejected'`），同一段日期被駁回
之後可以重送；相等欄位與範圍要放進同一個 GiST 索引，所以 0001 開頭啟用 `btree_gist`。
PostgreSQL 18 的 `WITHOUT OVERLAPS` 寫起來較短但**不收 `WHERE`**，那會讓駁回過的單也
擋住重送，所以這裡用排除約束。`create_leave_request`／`create_overtime_request` 在寫入
前先查一次，那一步只決定錯誤訊息說什麼，保證來自約束本身。

## 慣例

- **程式碼註解與文件字串一律英文**（Rust `//` `///` `//!`、SQL `--`、TS `//`
  `/** */`），識別字本來就是英文。註解寫事實與不明顯的約束，不寫論說文：不用
  `##` 修辭小標、不用 `**粗體**`、不用 `── ─────` 橫幅、不寫開發過程與第一人稱。
- **UI 文案一律繁體中文，語氣為清晰簡潔的商務風格**。不使用原型的口語腔
  （「今天的帳，還好嗎？」「別人還沒付我」已全數改為「財務總覽」「應收帳款」）。
  畫面文字包含後端 ops 產出的 `kicker`／`label`／`note`、`OpError` 訊息，
  以及 migration 種下的 `role.name`／`permission.detail` 這類會被畫面讀出
  的欄位，以及 `Operation::summary()`（MCP 畫面會把它列出來）——**只要看得到就是
  UI 文案**，四處同一套語氣。八條判準：
  1. 講使用者要做什麼，不講系統處在什麼狀態；
  2. 不寫實作進度。「已實作／尚未實作／排在下一階段」是開發者視角，畫面上沒有
     它的位置；做不到的功能就不要在畫面上留一個講它做不到的位置；
  3. 只留下做決定需要、而畫面上看不到的資訊；
  4. 不寫理由（理由寫在 `docs/architecture.md`）；
  5. 不用否定對比句。讀者沒提過那個對照組，是文字自己立一個再打倒它——
     「公司政策，不是法規」「開關只作用於 AI；畫面上的操作不受影響」整句刪掉，
     不是改寫；
  6. 不可逆的事要講，可逆的不必；
  7. 錯誤訊息寫「哪裡不對、怎麼修」，不寫「系統拒絕了，因為……」；
  8. 一句話講得完就不寫兩句，同一個約束在一頁只講一次。
  口語與公文腔都不要，寫平常說得出口、又是寫下來的那種商務中文：「這張表」寫
  「本表格」、「這裡」寫「本頁」、「對不上」寫「不相符」，但「就是」寫「等於」不寫「即」，
  「才會減少」不寫「隨之減少」，「還沒填」不寫「尚未填寫」——「即」「須」「亦」「均」「屬」
  「始」這些字一個都不要。不用第二人稱（「你的帳號」寫成「本帳號」），Token 的量詞是
  「組」不是「把」。
  推論：內部識別字（操作名、資料表欄位、環境變數、`dispatch`／`Registry`
  這類函式與型別名）不出現在畫面上；`boss` 角色一律稱**負責人**，不稱「老闆」
  （種子資料一起改了），`admin` 稱**系統管理員**。這條同樣適用於
  `permission.note`／`integration.detail`／`mcp_tool_grant.note` 這些會被畫面讀出的
  欄位，以及權限錯誤訊息——「另由 `adjust_entry` 管理」「限系統管理員執行，
  目前身分為「boss」」都是同一個毛病。
  **一個未決的例外**：`ops/selfservice.rs` 的 `scope_note` 與 `identity_note` 帶著
  `ctx.employee_id`、`created_by`、`claimant`、`BLAKE3`。畫面不讀這兩個欄位，它們是
  回應裡寫給 AI 看的授權契約說明，所以不算違規。要嘛換成中文說法、要嘛整組刪掉，
  兩件事都還沒決定；在決定之前**不要照著它們的寫法擴散到會被畫面讀出的欄位**。
- **看得到就要按得動**。畫不畫入口一律看 `/v1/auth/me` 的 `permissions`；叫不動的
  操作不要叫（會留下被擋下的稽核，而畫面把錯誤吃掉就成了整段無聲消失），也不要在
  它原地留白——寫一句誰維護。**分頁與畫面之外的入口也算**：header 的「＋ 建立傳票」
  要看 `create_voucher` 與 `list_accounts`（出納與主管兩者皆無，原本按得開抽屜、
  選科目時才跳「無法讀取會計科目表」），側邊的「繼續設定」要看
  `update_company_setting`（首次設定整條路是 `AdminOnly`，負責人點下去第一步就被擋，
  改成請系統管理員設定，不畫按鈕）。
- **時間欄（`HH:MM`）一律用 `components/TimeField`**：時、分兩個 `<select>`，
  格式由元件保證，永遠是 24 小時制。用在首次設定精靈與基礎設定的上下班時間、以及
  員工自己送出的補登申請。人資的 `correct_punch` 例外，它收秒，仍用純文字欄。
- **日期欄一律用 `components/DateField`**，不直接寫 `<input type="date">`：原生欄位
  的年份段收得下六位數，貼上或連續輸入「2026/08/21」會留下一個改不掉的年份，而畫面
  沒有任何提示。該元件把超出 2000–2099 的值退回上一個好值，不往 state 送。
- **一個東西一個名字**：正職（不是全職）、併入本期損益（不是不適用）、
  按曆日比例計薪（不是按日計算）、借方／貸方（不是借／貸）。同義詞在兩頁之間換來
  換去，讀的人會以為是兩件事。費率一律寫 `5%`，不寫 `0.05`。
- **解釋句掛在它解釋的那件事上**。損益表那句「開票傳票尚未過帳」原本掛在「收入為 0」，
  於是全新安裝第一天必定印出來，而系統裡根本沒有那張傳票。條件寫得比結論寬，就是在
  編前提。
- **用詞以台灣中小企業的會計與人資聽得懂為準**。「維度」是 BI 與對岸 ERP 的講法，
  畫面上寫「分類」「類別」「項目」；「矩陣」是數學與 BI 的講法，角色乘權限的那張表
  畫面上寫「權限對照表」；「回滾」是對岸的軟體用語，畫面上寫「復原」。資料表與操作名
  維持 `dimension`／`role_permission`／`rollback_import` 不動。能直接列舉就列舉
  （「部門、專案、成本中心、付款條件」），不為了省字造一個統稱。
- **`page-lede` 只寫使用者需要知道、而畫面上看不到的事**，不寫「這頁有什麼」——
  標題已經說過了。純複述內容或解釋設計決策的一律刪掉。
- **數字用 `.num`，它有自己的 `--font-numeric`**（Source Serif 4，lining +
  tabular）。該規則排在所有元件規則之後，因為元件也設 `font-family`，同權重下由
  檔案順序決定勝負。
- **回應形狀用 zod 驗在邊界**：`callOp(name, input, prompt, schema)` 的第四個參數
  傳了就驗、沒傳就照舊。宣告寫成 `export const employeeRow = z.object({...})` 加一行
  `export type EmployeeRow = z.infer<typeof employeeRow>`，型別名不變、畫面不用動；
  `z.object` 丟掉不認得的欄位，所以後端新增欄位不會弄壞舊畫面。這不會讓手寫的形狀
  自己跟上後端，它換掉的是失敗的方式：欄位改名或金額由字串變成數字，原本要等表格
  印出 `undefined`，現在停在 `callOp`。**手寫的 schema 只有一條路證得出它與後端相符**：
  `bun scripts/function-test.ts && bun scripts/schema-live-test.ts`，後者對著真的伺服器
  逐一呼叫再解析，並且從呼叫端本身讀出操作與 schema 的對應，不另存一份清單。
  純資料的邊界檢查在 `scripts/schema-test.ts`；螢幕上的金額與日期格式在
  `scripts/format-test.ts`、導覽與網址在 `scripts/nav-test.ts`，四支都在 CI 裡。
  **`/v1/auth/me`／`/v1/meta`／`/v1/ops` 不是操作但同樣要驗**（`authEnvelope`／`meta`／
  `opList`，都在 `client.ts`），live 檢查以 `mismatchAgainst` 核對。`SessionUser` 這類
  形狀住 `client.ts` 不住 `auth.ts`。風險分級的執行期清單由產生器發成 `RISKS`，不手抄。
- **`web/src` 底下不寫註解**（`.ts`／`.tsx`／`.css` 皆然，`api/generated.ts` 由
  產生器自帶的兩行標頭除外）。理由與設計說明寫在 `docs/architecture.md`，不要
  複製一份到元件旁邊。後端 Rust 仍照上一條寫英文註解。
- **金額一律 `rust_decimal::Decimal`，絕不用 f64**；JSON 上是字串
  （`#[serde(with = "rust_decimal::serde::str")]`），前端也不轉成 number。
- 前端只能用 `web/src/styles/classical.css` 的 CSS 變數，**不得寫死 hex／字型／px**。
  設計語彙是「以線條與框線構圖，不用色塊填滿」，按鈕外框式，數字 tabular。
- 單租戶：資料表**不帶** `company_id`（已確認的產品決策）。

## Commit 前的 Codex 審查

**每一個 commit 都要先過 Codex 審查**：`.githooks/pre-commit` 把暫存區的 diff 交給
`codex`，第一行不是 `ALLOW:` 就擋下，理由印在 stderr；`codex` 沒裝、沒登入或出錯一樣
擋下。merge、cherry-pick 與 revert 不審。git 不會自己採用 `core.hooksPath`，**每個
clone 要跑一次** `git config core.hooksPath .githooks`。

它掛在 git 而不是 Claude Code 的 `PreToolUse` 上：審的是實際的暫存區，不是去猜指令字串，
所以終端機、其他 agent 與 CI 的 commit 都會經過它。`codex` 在暫存目錄裡跑並清掉每一個
`GIT_*` 環境變數——在 codex 信任過的專案裡直接叫它，它會把 repo 的 HEAD 重設掉。

commit 之前先跑 `codex-review` skill，問題在 commit 前就解掉。**被擋下而判斷是誤判時停下來
交給使用者**，不要用 `--no-verify` 或 `CODEX_GATE_BYPASS=1` 繞過，那兩條是留給人的出口。
本機的 hook 擋不住刻意繞過的人，擋得住的是伺服器端的 branch protection。

## 專案管理（GitHub 看板）

**看板同步是完成定義的一部分**：開始一項工作、被卡住、PR 或 commit 落地、發現新的待辦或
技術債，四個時刻都要同步 `Laysi` org 的看板。**工作項一律用 issue**，不用 draft 卡——
只有 issue 掛得上 sub-issue、label、type 與 PR 連結，尚未成形的純想法才用 draft 暫存。
操作細節（GitHub MCP payload、欄位現查、gh fallback）在
`.claude/skills/project-board/SKILL.md`，本節只定政策。

- **Triage 與 Todo 的分界**：agent 自行發現或建立的工作項一律停在 **Triage**（AI 產生、
  尚未經人工審核）；**Todo 只放使用者核准過、真正可啟動的工作**。把卡移出 Triage 是
  使用者的決定，agent 不得自行升級；使用者明示指派的工作才可直接進 Todo 或 In Progress。
- **建 issue 時 type 與 label 必齊**：type 三選一（Feature 新能力／Bug 行為錯誤與資安
  漏洞／Task 其餘），label 只用 repo 現有的八個——分類五個（`bug`／`enhancement`／
  `documentation`／`ui`／`ci/cd`）與分級三個（`priority:P0`／`priority:P1`／
  `priority:P2`）。**新增 label 分類要先問過**——同一個 repo 長出兩套命名之後，讀的人會
  以為是兩件事。分級設兩處且要一致：看板的 Priority 欄位與 `priority:*` label（P0 生產
  事故或阻斷／P1 當前重點／P2 backlog）——前者只在看板上看得到，後者才出現在 issue
  頁面。**org 的 issue 欄位 Priority 不用**：它的選項是 Urgent／High／Medium／Low，
  改名只有 org owner 做得到，而同一件事有兩種寫法就會被當成兩件事；trustiot 用的也是
  label 這條路。
- **內文寫成冷啟動讀得懂**：七段結構（成果／背景／範圍／驗收條件／驗證／附記／預估），
  背景要指到檔案與行號。驗收條件每一條獨立可驗證並對應 CI 跑得到的 gate，**全部勾完才
  允許 close**，而勾的依據是那一條的驗證跑過了，不是程式碼寫完了。
- **寫入之後讀回驗證**：GitHub 回 200 只證明它收下請求，不證明看板欄位、auto-add 與
  close 連動產生了預期的結果。close 之後仍要讀回看板確認 Status 是 Done。

## 目前進度

已完成：資料庫不變量、法規引擎、操作註冊表、REST + MCP、
RustFS 物件儲存，以及 **37 個畫面、135 個操作**，分成三個分頁
（`FINANCE_NAV`／`HR_NAV`／`ME_NAV`）加上左下角帳號列的系統按鈕（`SYSTEM_NAV`）：

- **財務 12 畫面**：總覽／傳票總帳／記帳抽屜、應收／應付／銀行、稅務／固定資產、
  費用報支／預算／財務報表、文件管理／資料匯入。**不做電子發票**：開立統一發票是
  加值中心的事，銷項稅額改由應收單自己拆到 2211。
- **系統 6 畫面**：基礎設定／帳號管理／角色核准／法規版本／MCP／安全營運。原本掛在財務樹底下，
  但那是管理這套系統，不是財務工作。**它不是第四個分頁**：前三個是每天待著的
  地方，這個是偶爾去一次的地方，所以放在左下角帳號列旁邊，角色開不起任何一個
  畫面時連按鈕都不出現。
- **人資 8 畫面**：人資總覽／員工名冊、出勤與休假（含打卡與補登）／工作日曆、
  薪資作業／勞健保勞退、扣繳與法遵、公告與規章。
- **員工自助 11 畫面**：打卡／個人專區／公告與規章／休假申請／加班申請／請款申請／
  我的休假／我的加班／我的請款／我的薪資單／我的 MCP。分成「提出申請」與「查詢紀錄」兩組，
  因為填表與查進度是兩件事。

原型是 30 個畫面，但兩邊的數字不是同一組：人資段原先估為 9 個，目前的人資操作
攤在 6 個畫面上，差的 3 個沒有對應操作，**不確定原型上那三個是什麼**；自助段
則多出休假申請、請款申請與打卡三個畫面。要補人資那三個之前先回去讀
`財務系統原型.dc.html`，不要自行補兩個畫面。

**公告與規章是同一張表，每一版都留著**：`notice` 只存身分
（`kind` 分 `announcement` 與 `policy`）、廢止日與指向現行版本的 `version_no`；標題、
內文與生效日住在 `notice_version`，**只增不改**。改動這三者任一項就是新的一版，設定
下架／廢止日不算。舊版任何看得到這一頁的人都讀得到——`list_notices` 與
`list_my_notices` 帶 `id` 就回該則的每一個版本，兩邊回的是同一份。
原本的做法是改寫規章就刪掉閱讀確認，那條路把「誰在哪一天讀過哪一段文字」銷毀，
而那是勞資雙方事後唯一拿得出來的東西；確認因此整套移除，改成保存版本。

**未讀是「看到哪一刻為止」，不是簽核**：`notice_read` 一人一列只記時刻，
沒有版本、人資那一面也讀不到它。`mark_my_notices_read` 由本人按下，對 AI 一律關閉。
沒有紀錄＝全部未讀（新人要看得到他到職前就貼上的規章）。**不要用「最近 N 天」推估
未讀**——那是每個人都一樣的數字，消不掉，而畫面上那個數字消不掉就等於沒有用。

**公告與規章的內文是 Markdown**：由 `components/Markdown.tsx` 直接產生 React 元素，
**不經 `dangerouslySetInnerHTML`**，所以結構上沒有 HTML 注入的路。支援的子集是標題、
清單、引述、粗體斜體、行內程式碼、分隔線與 http(s) 連結；子集寫在
`MARKDOWN_HINT`，編輯畫面把那一句印給作者看，不另外寫一段說明。表格不支援。

**內容一律折疊**：卡片預設只顯示標題與日期，點標題展開。員工端把**還沒看過的**那幾則
預設展開——提醒要看的東西就是要看得到的東西。

**佈告欄推送到聊天室是 `company_setting` 的一列**：`notice_webhook_url` 填了才推，
`create_notice` 與改動內容的 `update_notice` 會送，設定下架日不送（那不是新的一版）。
**訊息形狀由網址的主機決定，不另設「平台」欄位**：Slack、Google Chat 與 Teams
connector 讀 `text`，Discord 讀 `content`，Lark／飛書讀 `msg_type` 底下的
`content.text`，Teams 走 Power Automate 收 Adaptive Card——Discord 的 `content` 是字串
而 Lark 的是物件，一份塞滿每個欄位的 payload 不存在；認不得的主機送 `text`。同步送、
結果回報給發布的人，不排隊不重試，超過 1800 字截斷（Discord 上限 2000）。網址只收
`https://` 且**本身是憑證**，稽核摘要寫「已更換」不寫值；存檔當下送一則測試訊息，成敗
回在畫面上。畫面在「基礎設定」自己一張卡（輸入框＋儲存／測試推送／清除），「送往 Slack」
那一行比對後端 `get_company_settings` 回的 `push_targets`，**前端不得自帶那張對照表**；
重測走 `test_notice_push`（`AdminOnly`），不是重存一次設定——重存沒有變更的值會留下一筆
說「網址已更換」的稽核紀錄。

**固定資產登記得進去**：`create_fixed_asset` 同一交易寫登記簿與取得傳票
（借資產科目與 1284 進項稅額／貸付款科目）。成本填**未稅**金額——它同時是 1601 的
借方與折舊基礎；營業稅走 `split_vat`，稅率不進 Rust。**每筆資產一定要指向一張傳票**
（付款科目二選一產生新傳票，或填既有傳票號），否則登記簿會宣稱一筆總帳沒見過的
成本，而那一頁的第一句話就是拿兩邊對帳。賒購不由這裡貸 2101：應付帳款頁把未付合計
與總帳 2101 對帳，正解是在那裡以 1601 為借方科目開單再回填傳票號。耐用年數與稅率
一律由 `list_fixed_assets` 回的 `categories`／`vat` 帶給畫面，**前端不得自帶**。

**單據編號的計數器鍵是（前綴, 期間）**。原本一個前綴一列、換期間就歸 1，
往前走對、往回走會發回已經用過的號碼：提列 9 月折舊後再算 8 月薪資，`V2608-001`
撞上 UNIQUE，薪資作業直接以資料庫錯誤收場而畫面上繞不過去。

**報表要說出它漏掉了什麼**：`unposted` 有兩個數字——還沒過帳的，以及**已過帳但
日期在截止日之後**的。整月折舊的傳票是月底日期，報表結到今天，帳是對的而那筆錢
不在表上；只數未過帳的傳票會讓它在每一張能回答它的畫面上無人認領。

**資產科目可選，所以不能寫死 1601／1602**：`create_fixed_asset` 驗兩個科目是可入帳
的 `non_current_asset` 且借貸方向正確；`list_fixed_assets` 拿登記簿實際用到的科目
去對總帳並回科目名稱給畫面；`record_depreciation_run` 的貸方依累計折舊科目分組，
一個科目一行。合併成一行貸 1602，第二組科目的對帳就永遠差一個數。

**內部代碼不進畫面文字**：人資總覽的銜接說明原本印 `pending`，其他每一頁都寫
「待過帳」。`voucher_status` 當成資料欄位交給前端是可以的，寫進句子就要先換成
顯示名。

**改不動的欄位不要畫成可以改的樣子**：帳號管理的編輯抽屜裡角色是唯讀文字加一顆
「變更角色」（`update_user_account` 本來就不收 `role_code`），不是 `disabled` 的
下拉——一顆看起來能按的控制項加上「存檔沒有訊息」就是「按下去看起來成功、其實什麼
都沒發生」。沒有欄位變更時要說「沒有變更」，不要靜靜關掉抽屜。擋的是**選得動的樣子**
（`disabled` 的下拉還列著其他選項），不是欄位本身：固定值仍然畫成 `readonly` 的輸入框
（初始化頁的「登入名稱 admin」、帳號管理的「登入信箱」），因為那是憑證的一半，寫成
散句瀏覽器與密碼管理程式都收不到。`readonly` 不用 `disabled`——`disabled` 的欄位不會
隨表單送出，密碼管理程式就只存得到密碼、下次登入頁自動填不出帳號。

**員工附件不走 `manage_documents`**：`list_employee_documents`／
`upload_employee_document`／`get_employee_document` 用同兩張表、同一個桶子與同一次
BLAKE3 比對，但**一律收員工編號、不收自由的文件編號**，並比對 `link_type = 'employee'`
與 `link_ref`——別人的編號回**查無此附件**（拒絕會確認編號存在並說出屬於誰），同一條
檢查也擋住用 `doc_no` 往別人的附件加一版。`manage_documents` 一併打開全公司的收據與
銀行文件，而一個操作只能屬於一條 permission，所以「同樣三個操作換一組權限」不存在。

**人資核發得了帳號，但只發一般職員**：`create_employee_login` 角色寫死 `staff`、
只認名冊上既有的員工編號、已有帳號的人一律拒絕；其餘檢查與 `create_user_account`
共用 `check_new_login`／`write_login`，兩條路不會長出兩套規則。**檢查與寫入分兩個
函式**，因為 `create_user_account` 在兩者之間要建員工主檔，先寫再被拒會留下一筆沒人要
的名冊資料。要不要密碼由伺服器判斷（外部登入開著＋自動建帳號＋地址在網域內三者齊備就
免密碼），人資不必知道這條規則。`list_employees` 因此回 `has_login`——畫面靠它決定畫不畫
按鈕，而 `list_user_accounts` 是 `AdminOnly`。放寬的方式不是把 `create_user_account`
交出去：它收 `role_code`。

**投保級距是建議值，不是分級表**：抽屜填本薪時帶入三個投保欄位，帶的是
`minimum_wage.monthly` 與 `labor_insurance.max_insured_salary` 夾出來的本薪，由
`list_employees` 的 `insured_suggestion` 給畫面，**前端不得自帶**。**不搬整份分級表
進來**：那是三份逐年更動的表格，抄錯要到申報那天才出現，所以畫面照實說尚未對照分級表。
時薪制不帶入（換算月投保薪資要約定工時，系統沒有記錄它）；帶入只覆寫空的、或還停在
上一次建議值的欄位。

**變更角色是獨立操作**：`change_user_role` 拒絕呼叫者自己的帳號（那是唯一
會多出權限的方向；建帳號時本來就指定得了任何角色）、拒絕清空最後一個啟用中的系統
管理員，並在同一個交易撤銷該帳號的登入 session 與本人核發的 MCP Token——`dispatch`
每次現查角色，但 `api_token.actor_role` 是核發當下的快照，不撤等於舊角色永遠叫得動。
呼叫者的鍵是 `OpCtx.username`（`actor` 是顯示名稱、會同名），MCP Token 沒有登入
名稱，所以那條路一律拒絕。

**手工傳票也要能指定部門**：`LineInput` 有 `dept_code`，`insert_voucher` 逐行解析。
少了它，薪資會分部門、CSV 匯入會分部門，只有畫面上手開的傳票分不了，於是不匯入
CSV 的小公司永遠只看得到「未指定」那一行部門損益。

**月底彙總匯款的報支單同時是一張應付帳單**：最後一關產生的傳票貸 2101，
而應付帳款頁比對 `ap_bill` 明細合計與 2101 總帳餘額，所以只動總帳的那一筆會讓兩邊
永遠差一個報支金額，而畫面上修不掉——「新增應付帳單」一定會自己再開一張傳票，補一張
只會把差額變成兩倍。核准因此在同一個交易裡寫 `ap_bill`，帳單號碼即報支單號、供應商記
申請人、到期日為該月月底；這也是這筆錢唯一的匯出路徑，付款單只讀 `ap_bill`。零用金
支付的單據貸 1101，錢已經付掉，不建帳單。

**離職與關掉存取是同一件事**：`update_employee` 把狀態改成 `left` 時，同一個交易接著
停用他的登入帳號、撤銷未過期的 session，並撤銷 `owner_employee_id` 指向他的 MCP
Token。三件事要一起做——session 查詢帶 `u.status = 'active'`，但 MCP Token 不讀那一欄，
生效日之後也沒有任何排程回頭檢查，留著的登入就是永久有效的登入。唯一的例外是最後一個
啟用中的系統管理員：沒有任何操作能把角色給出去，停用它等於誰也打不開帳號管理，那次
更新直接拒絕並要求先建另一個管理員帳號。`update_employee` 以 `closed_access` 回報
關掉了什麼，畫面照實說；`list_user_accounts` 另回連結員工的在職狀態，帳號管理不必跳到
名冊才看得出這個人已經離職。

**四張沒有寫入路徑的表刪掉**：判準與異動單的 `hr_flow_step` 相同——
全 codebase 沒有任何東西寫得進去的表，是穿著資料表外衣的常數。
`attendance`（月統計）讓 `get_attendance` 每次回空清單、對帳每次回「沒有可對照的
資料」、月份選單每次是空的，而 `run_payroll` 的加班費退路恆為 0；操作與畫面上那
半頁一起移除，月份選單改讀 `punch_months()`。`dimension` 的專案、成本中心、付款
條件三列後端零命中，而基礎設定對四個分類全部畫「新增值」，建出來的項目永遠沒人
讀。`integration` 七列有四列在公告自己還沒實作（含已刪的電子發票），剩下三
列裡 REST 與 MCP 是同義反覆，而物件儲存那列把 `live` 寫死成 `true`，實際上伺服器
把它當成選配——那個問題「文件管理」本來就現查現答。`import_source` 六列有五列
`supported = false`；那一欄留著（它決定檔案交給哪個解析器），列刪掉。
`security_policy` 刪掉最後一列「只是宣稱」之後，`enforced` 全為 true，
分辨不了任何東西，兩列搬進 `ops/security.rs` 的 `FIXED_RULES`。

**「控制規則」整張卡與 `get_security_policy` 刪掉**：同一條判準往下一層。
四列裡兩列是編進 Rust 的常數，另外兩列（AI 禁止事項、金額門檻）由 `mcp_tool_grant`
與 `approval_rule` 現算，而改得動它們的開關在「MCP 與 AI」與「角色與核准」——那張卡
只是把別頁的設定再念一次，念完還得附一句叫人去別頁調整，違反「看得到就要按得動」。
`view_system` 少一個操作，側邊「安全與營運」改由 `list_api_tokens` 決定畫不畫。

**異動單整套刪掉**：`hr_flow_step`／`employee_movement`／
`employee_movement_step` 三張表與三個操作、「到離職與異動」畫面全數移除。四種異動
裡，調薪與調部門結案時只寫一個欄位，而名冊的編輯抽屜一步就寫得到，也沒有任何機制
要求走異動單；到職結案不寫任何東西；`hr_flow_step` 全 codebase 沒有寫入路徑，
十五列是穿著資料表外衣的常數。唯一沒有第二條路的是離職那段交易，搬進
`update_employee`——那正是原本繞得過去的那條路。

**首次設定精靈只送有變更的設定**：上下班時間那一步原本把三個欄位一律送出，欄位是空的
就送出三個清空指令，然後回報「已儲存」——全新安裝時那三項本來就是空的，於是精靈走完
一輪，畫面上寫著已儲存，實際上什麼也沒設定。現在只送與載入值不同的欄位，沒有變更就
說沒有變更，並把實際存下的值念回來。

**精靈沒有「部門」那一步**：它一次只收得下一個名稱，填完就前進，畫面也沒說其餘部門
要去哪裡建——而「基礎設定」的部門區塊本來就建得了，還連續建得下去。一步只能做半件事
又不講剩下半件在哪，比沒有這一步更糟。精靈剩五步：公司資料、銀行帳戶、上下班時間、
外部登入、負責人帳號。

**能不能簽由後端回答**：`list_expense_claims` 回 `can_approve`／
`approve_blocked_reason`，`list_leave_requests` 回 `can_decide`／
`decide_blocked_reason`；畫面在 `false` 的那一列**不畫按鈕，改印原因**。亮著一顆
按下去必定被拒的按鈕，等於把規則講兩次而其中一次是錯的。**申請人不得核准本人
送出的單據，休假、加班、補登與報支同一條規則**（比對 `employee_id`，報支另比名字）。
唯一的例外是公司設定 `owner_self_decide` 開著時的**負責人本人**——判斷只在
`ops/settings.rs` 的 `owner_decides_own`，不要在各流程另寫一份；其他角色被擋時拒絕訊息
說出出路是請系統管理員建第二個帳號（建帳號是 `AdminOnly`），負責人被擋時指向「基礎設定」。

**單據編號只有一種年份寫法**：`{前綴}{西元年末兩碼}{月份兩碼}-{三碼流水}`，
傳票 `V`、付款批次 `P`、報支 `EX`、文件 `D`、休假單 `LV` 共用。休假單原本用民國年
（`LV11507-001`），同一天的四張單就並排著兩種曆法。

**時間戳的日期一律用 `businessDate()`，不要 `slice(0, 10)`**：後端回 UTC，切字串在
本地 00:00–08:00 之間會少一天，而同一頁用 `businessTime()` 印的登入、稽核、打卡
時間全是對的。

**記帳時區是 `company_setting.business_utc_offset`，不是常數**：
`business_time`／`business_today` 收 offset，行程層那一份在 `laysi_ops::business_offset()`，
每次呼叫由 `OpCtx.offset` 帶著走，前端由 `/v1/meta` 的 `utc_offset` 拿。改時區由
`set_company_timezone` 在同一交易改寫 `attendance_punch` 與 `punch_correction` 的 CHECK，
**兩張表任一有資料就拒絕**——`punched_on` 是推導後存下來的日期又是 UNIQUE 鍵的一部分，
換偏移量會把晚間的打卡改記到另一天。設定列 `editable = false`，`update_company_setting`
碰不到它。時區修不了走錯的伺服器時鐘：錯的是瞬間本身，不是顯示。

**「現在」一律問伺服器，不要問瀏覽器**：會走動的時鐘用 `businessClock()`、日期預設用
`businessToday()`，兩者都由 `serverNow()`（`callOp`／`getMeta`／`/v1/auth/me` 回應的
HTTP `Date` 標頭）換算到 +08:00。`new Date().getHours()` 讀的是瀏覽器的時區，而記下來
的時間是伺服器時鐘換算 +08:00——瀏覽器時區不是 +08:00 時，打卡卡片會用一種時間往前數、
記錄另一種時間，差幾小時就整整差幾小時，而畫面上沒有任何跡象。

**角色只在帳號上，員工主檔沒有角色**：權限由 `user_account.role_code` 決定，
`employee.role_code` 誰也不讀，卻在名冊上跟職稱、僱用型態排成一串。整欄拿掉，不是
換個標籤。

**資金帳戶是分錄上的一欄**：`cash_position` 依 `voucher_line.bank_account_id` 分組，
而人工建立的傳票原本從不帶那一欄，於是借 1102 過帳後銀行卡片是 0、現金流量表宣告
對不上。`insert_voucher` 現在會在該科目底下只有一個 `bank_account` 時自動掛上；
掛不上的（零用金 1101、一個科目多個帳戶）由 `unattributed_cash` 另列一筆，
**卡片合計恆等於總帳現金餘額**，那不是差額，是還沒說錢放在哪個帳戶。

**一期可以計算不只一次**：`payroll_run.period` 沒有 UNIQUE，計薪之後才到職的
人由第二次計算補發，`run_payroll` 跳過已持有薪資單的人。**已過帳的期間補算是唯一的
路**，作廢在那裡被擋下。所以 `payroll_runs` 的排序要帶 `p.id DESC`，`record_payroll_run`
依 id 回傳它剛寫的那一列（依期別再查會拿到該期排最前面的那一列），而
`get_payroll_summary` 與 `list_payslips` 的 `run` 是**該期各次計算的合計**，各次的傳票與
規則版本走 `period_runs`，期別選單逐期只列一次。作廢的按鈕要該期每一張傳票都還沒過帳
才畫。

**薪資改得回來，但只到過帳為止**：`void_payroll_run` 作廢一整期，薪資單、
扣繳紀錄與傳票在同一交易刪掉，主檔改好後重新計算。改了到職日卻無法重算就是它要解的
問題——`run_payroll` 跳過已有薪資單的人，`discard_voucher` 又拒絕薪資產生的傳票。
**該期任何一張傳票不是待過帳就整個拒絕**：已過帳的金額報表算過了，那裡的更正是下一期
的差額。理由必填並寫進稽核摘要，畫面上的按鈕看 `permissions` 與傳票狀態兩個條件。

**`run_payroll` 同一交易寫 `withholding_record`**（薪資所得代號 50，`basis = rate`）。
稅額不存欄位——由給付日的 ruleset 現算，存一份等於同一個問題有兩個答案。
`get_hr_compliance` 的對帳因此看所得代號而不是 `basis`。少了這條，扣繳頁永遠寫著
「對不上」而畫面上沒有任何動作消得掉它。

**財務報表由總帳現算，系統裡沒有一張報表數字表**。新增科目時要在
`account.report_group` 與 `cash_flow_section` 指定它出現在報表哪一段，
否則 CHECK 會擋下來。三條不變量（資產＝負債＋權益、兩張表的本期損益一致、
期末現金＝現金科目餘額）都有測試，見 `docs/architecture.md`。

**預算與報支同一條原則**：`budget` 只存預算數，實際數一律現查 `voucher_line`；
簽核關卡由 `approval_rule` 的金額門檻算出來，`ops/expenses.rs` 裡沒有任何金額。
預算要先有版本才能編（`create_budget_version`，同一交易把前一版拉下 `is_current`）。
`budget_alert_percent` **留空＝沒有提醒**，不是格式錯誤：種下來就是空字串，當成錯誤
會讓全新安裝的預算頁只剩一行紅字，而那個設定只有系統管理員改得動。建了版本就要編得了
預算：「＋ 編列科目預算」與列上的 ✎ 走同一個 `set_budget`，科目只列可過帳的收益與費用。

**應收單與應付帳單一定要拆營業稅**：401 申報讀的是 2211 與 1284 的期間變動，
沒拆的單據會讓申報短少剛好那筆稅額。`create_ar_invoice`／`create_ap_bill` 的
`amount`（含稅）／`net_amount`（未稅）／`tax_amount` 給其中兩個，其餘由
`ops::tax::split_vat` 依單據日期解析出的 `vat` ruleset 補齊——**稅率不進 Rust**。
零稅率與免稅寫明確的 `tax_amount = 0`，不設旗標。

**更新類操作的金額欄一律三態**：不帶＝不動、明確給 `0`＝清為尚未設定、給數字＝更新
（`update_employee` 的底薪與三個投保級距、`update_user_account` 的 `employee_no`）。
少了這條，只改職稱就會把四個金額歸零，而且要到發薪那天才會發現。

**名冊的編輯抽屜把四個金額填進欄位，不當提示字**：原本欄位留白、原值印在 placeholder
（「目前 80,000」），於是調薪要把整個數字重打一次，而那個灰字讀起來像範例不像現值。
現在 `openEdit` 以 `amountOf`（`isZero` ＋ `trimDecimal`）填入現值，`amountFields`
只送與載入值不同的欄位——**沒動的欄位不帶、清空的欄位送 `'0'`**。三態沒有變，變的是
哪一個 UI 動作對應哪一態：畫面上清空一個欄位就是清為尚未設定，而不是靜靜地什麼都沒發生。
新增時沒有載入值，留白仍然是不帶。

**名冊回傳底薪與三個投保級距**：不回的話全系統沒有一頁看得到某人現在的本薪，調薪要
憑記憶，編輯抽屜也填不出原值。這不是放寬權限——同一組 `view_hr` 底下的
`get_insurance_summary` 本來就逐人列出三個級距，而持有它的只有負責人與會計。
逐月薪資單仍在 `list_payslips`，仍限負責人。

**員工的銀行帳戶一人多筆**：`employee.bank_account` 那一欄自由文字只裝得下
一個帳戶，而薪轉戶與收退款的帳戶常常不是同一個。`employee_bank_account` 把銀行名稱與
帳號分成兩欄——自助頁只印銀行與尾碼，欄位分開就不必猜一個字串在哪裡結束；`is_payroll`
以 `WHERE is_payroll` 的 partial UNIQUE 索引限制至多一筆，發薪當天只有一個去向。
`bank_accounts` 不帶＝不動、給清單＝整份取代，與四個金額同一族的三態；至多一筆可標為
薪轉，一筆都沒標時以第一筆為準。`account_no` 與 `id_no` 同列 `REDACTED_FIELDS`。

**`employee.id_no` 完整保存**，寫入時驗一個英文字母加九碼數字的檢查碼。
勞保申報與扣繳憑單都要完整字號，擋著只會讓人資另外開一份 Excel。**遮罩是顯示層
的事**（名冊預設 `A12*****89`，一顆鈕整批切換）；稽核紀錄由 `redact_for_audit`
的 `REDACTED_FIELDS` 依欄位名換成「[已遮蔽]」，長度規則擋不到十個字元的字號。

**文件內容在 RustFS，資料庫只留物件鍵與 BLAKE3**。先配編號、檔案進桶子成功
才寫資料庫；取回時一定重算雜湊比對，對不上就不交出內容。**沒有 OCR**——`ocr_status`
整欄已經刪掉，它是由 MIME 型別推出來、沒有任何判斷讀它的常數。

**人資是一個角色**：`hr` 持有 `view_hr`／`manage_hr`／`view_payroll`／`manage_payroll`／
`staff_self_service`，剛好開啟人資分頁的八個畫面，財務分頁與系統按鈕都不出現。原本
人資只能設成 `acct`，為了核一張休假單而拿到全套財務權限。**逐人薪資明細不在裡面也不是
漏掉的**：`list_payslips` 與 `list_withholding_statements` 掛在 `BossOnly` 底線上，
矩陣拉不動它。名冊的部門下拉因此改由 `list_employees` 回的 `departments` 帶給畫面
（與 `list_fixed_assets` 回 `categories` 同一個做法）——原本呼叫的 `list_dimensions`
屬於 `view_masters`，那組權限也開著「基礎設定」，而呼叫端把失敗吞掉，少了它的角色會
看到一個空的下拉。

**角色權限由 `dispatch` 即時強制 deny-by-default**：每次都查
`role_permission → permission.op_names`，不快取。矩陣之外還有**兩道風險底線**：
`Risk::BossOnly` 綁 `boss`，管業務機密與權威（薪資明細、扣繳憑單、預算、
簽核門檻）；`Risk::AdminOnly` 綁 `admin`，管系統本身（權限矩陣、API Token、公司設定、
建立帳號）。**裝機的人不該因此看得到全公司薪資**，所以這兩件事是兩個角色。分離之所以
成立：底線在矩陣之外，`set_role_permission` 拉不動它；而 `change_user_role`
**拒絕變更呼叫者自己的角色**，所以系統管理員仍然升不了自己。
系統管理員另持 `manage_masters`（首次設定要建銀行帳戶與部門），該組裡的
`set_approval_rule` 仍擋在負責人底線後。
**`/v1/auth/me` 的 `permissions` 要扣掉這兩道底線**（`auth::callable_by`）：矩陣把
每一項都給了負責人，只查矩陣的清單會讓基礎設定畫出按不動的編輯鈕。畫面一律靠這份
清單決定畫不畫入口，所以它的意思是「真的叫得動」，不是「矩陣上有」。
**權限錯誤與稽核摘要不出現操作名與角色代號**：`dispatch` 用 `op_label(op.summary())`
取中文操作名的第一段、用 `role.name` 取角色顯示名。名稱從 summary 推導而不另設欄位，
多一個欄位就是多一份會漂走的副本。
REST 身分只由 HttpOnly session cookie 查回 `user_account`，請求標頭不能指定
actor 或 role。MCP 身分由 Bearer token 所對應的 `api_token` 資料列決定，
請求標頭同樣無法覆寫 actor 或 role；撤銷後下一次呼叫立刻失效。
OIDC 四項設定齊全時採 discovery + Authorization Code + PKCE，登入的鑰匙永遠是
`(issuer, sub)`。

**不要新增需要人自己想一個值的環境變數**。密鑰由 compose 的 `secrets` 服務產生、寫進
資料卷，各服務讀檔取用（`laysi_store::env_or_file` 收任何設定的 `NAME_FILE`）；**產生過
就不再產生**。`RUSTFS_SECRET_KEY` 是唯一的例外。政策值進 `company_setting`，
`BIND_ADDR`／埠號是 compose 佈線、不是設定。

**新政策值進 `company_setting`，不另建鍵值表**（目前三項：閒置逾時、一次登入最長
時數、MCP 每分鐘呼叫上限）。沒有 `Store` 可用的路徑要照 `business_offset` 那條路：啟動
時讀進行程內的 atomic，`update_company_setting` 寫完重新發布，不必重啟。
**session cookie 的 `Secure` 由 `X-Forwarded-Proto` 推導，不是設定**；
`MCP_ALLOWED_ORIGINS` 留在環境變數，它不是設定。**伺服器不掛 CORS 層**：開發時
Vite 代理、compose 部署下 Caddy 代理，瀏覽器兩邊都看不到跨源請求。

**遠端連接器走 OAuth，取得的 access token 就是一列 `api_token`**（`oauth.rs`）：
Claude 這類雲端連接器只收得下一個網址，沒有貼 Token 的欄位，所以 `/mcp` 的 401 要帶
`WWW-Authenticate: Bearer resource_metadata="…"`，兩份 `/.well-known/oauth-*` 指出
註冊、授權與換發三個端點。**沒有第二套 Token 儲存**，因此沒有 refresh token 也不回
`expires_in`——生死由「安全與營運」那一頁回答，`mcp.rs` 的認證一行不用改。用戶端是
public client，改以 PKCE S256 證明持有請求；**`code_verifier` 的格式要先驗再用**，
`from_code_verifier_sha256` 對 43–128 字元是 `assert!` 不是回傳錯誤，沒檢查就 panic
掉工作執行緒。授權碼贖回時同一道敘述刪掉該列，`redirect_uri` 兩端逐字相符，身分只來自
瀏覽器 session，角色在鑄造當下重讀。**絕對網址由請求推導**（`public_base`），與 cookie
的 `Secure` 同一條理由。同意畫面是 SPA 的 `/oauth/authorize`，`App.tsx` 在 `Workspace`
之前攔下——`Workspace` 掛載時會 `replaceState`，查詢字串在那一步整段消失。
**`/.well-known/oauth-*` 在 Caddy 要自己一條 route**：落到 SPA fallback 就是 200＋
首頁 HTML，而連接器報的是「這個網址不是 MCP 伺服器」。
**還沒登入就按下連接器時，登入完要回得了同意畫面**：整份授權請求只活在那一頁的查詢
字串裡。兩條登入路各自會丟掉它——`landOnStart()` 把網址 `replaceState` 成 `/`（加一個
`consentRequested()` 條件），`oidc::callback` 302 回 `/`（`start` 收 `return_to`，與
PKCE 一起存在**伺服器端**的待決流程裡，放在 callback 的查詢參數上就換得掉）。
`return_target()` 只認 `/oauth/authorize?` 開頭，所以不是開放轉址。

**異地備份的目的地在畫面上**：`backup_setting` 單列表，`AdminOnly` 的三個操作
在「安全與營運」。金鑰非對稱，**私鑰由 `backup` 容器產生**（持有 `age` 的那一方），
顯示一次，`confirm_backup_key` 之前**備份不執行**——不然第一份備份會含有開啟它自己的
鑰匙。伺服器不引入 `age` crate。

**外部登入的設定在畫面上，不在環境變數**：`oidc_setting` 單列表，安全與營運頁
由系統管理員填；存檔推進 `updated_at`，伺服器以它當快取指紋重建 client，**不必重啟**。
`client_secret` 不經任何操作回傳，且列在 `REDACTED_FIELDS`。建立成敗寫回
`verified_at`／`verify_error`——discovery 是伺服器層的事，而按儲存的人要看得到能不能用；
寫回前比對舊值，所以不會碰 `updated_at`。使用者看得到的錯誤訊息裡不出現 issuer、
discovery 這類字眼。

**Google Cloud Identity 的網域邊界看 `hd`，不看 email 網域**：允許的網域
填了就要求 id_token 的 `hd` 相符，**claim 不存在一律拒絕**。個人 Google 帳號可以
持有一個已驗證的公司網域 email——看字串的檢查放它進來，而人離職時公司關不掉它。
`hd` 不是標準 claim，由 `oidc.rs` 的 `HostedDomainClaims` 以 `AdditionalClaims`
接住，仍然由 `openidconnect` 驗完簽章才交出來。

**「首次登入」選了角色就打開網域內自動建帳號**（選「不自動建立」維持原本的
「只認預先連結」）。角色代碼與網域在存檔時驗，缺一個就存不進去——沒有網域的自動
建帳號等於任何外部帳號都能在這裡開一個登入。解析順序：`user_identity` →
`user_account.email`（不分大小寫，**只在自動建帳號開啟時用一次**，用來接上管理員
已經建好的同一個人；不限啟用中的帳號，停用的要被找到並擋下）→ 都沒有才建。建的
時候同一個交易寫**員工主檔**、帳號與身分，員工編號在交易內用 advisory lock 配號；
少了員工主檔，自動建出來的帳號打不了卡、請不了假、報不了帳。密碼欄位是 NULL。
代價是 `user_account.email` 從此是安全相關欄位，改得動它的就決定得了誰連到哪個帳號
（`update_user_account` 是 `AdminOnly`，所以不是新權限，是新責任）。自動建帳號因此
沒有名稱可推導，第 3 步直接用那個 email 當登入名稱——見下一條，本站已經沒有獨立於
email 的登入名稱。

**登入名稱＝email，`admin` 是唯一的例外**：`create_user_account` 與 OIDC 自動建帳號只收
一個 email 欄位，`username` 建立當下就是那個 email（小寫化），兩處各自驗證格式、不共用
函式。建立後 `username` 仍不可變，但 `email` 之後可由 `update_user_account` 獨立編輯，
兩者從此可能分開——帳號管理的「登入信箱」（唯讀）與「電子郵件」（選填、可編輯）因此
是兩個欄位；OIDC 依 email 找既有帳號的第 2 步找的正是「聯絡信箱等於這個地址」，不是
「登入名稱」。`laysi-admin create-user --username` 不受影響，仍收任意字串——那是離線
逃生門，不是要簡化的網頁流程。

**網頁初始化建的是安裝本身的帳號，不是某個人的**：登入名稱固定 `admin`
（`laysi_ops::ADMIN_USERNAME`）、顯示名稱固定「系統管理員」、`email` 留空、沒有員工
主檔，呼叫端只送一個密碼。它是全站唯一不是 email 的登入名稱，而空 email 正是它成立的
條件：`resolve_account` 的第 2 步用 `users_by_email` 認人，填了地址就等於讓外部身分連
到系統管理員帳號並沿用它的角色。`update_user_account` 因此拒絕給 `admin` 設 email。
留空還有兩個後果：裝機的人離職不會帶走這個帳號，而 `password_login_disabled` 的例外
從此指著一個**本來就沒有 SSO 路徑**的帳號——那顆例外是它唯一的門，不是多開的第二道。
登入頁的欄位所以是 `type="text"`；`type="email"` 會讓瀏覽器擋下 `admin` 這個值。
第二個帳號起才是人：精靈第五步用 `create_user_account` 建負責人。

**`password_login_disabled`讓帳號密碼登入整個關掉，系統管理員例外**：
判斷點不在寫入端而在讀取端——`auth::login` 不直接讀這個欄位，而是問 `oidc::current()`
現在建不建得出可用 client，只有真的建得出來才擋。IdP 掛掉或設定有誤，`current()` 回
`None`，密碼登入立刻對所有人恢復，不必有人發現並手動把開關關回去；這與 `verified_at`
是同一份自我修復機制的另一個消費端。只在密碼**已驗證正確之後**才判斷，不會變成第二個
帳號列舉管道。登入頁對稱地收起密碼欄位，改畫一顆「系統管理員改用密碼登入」的展開連結。

**解除連結與刪除是兩件事**：`unlink_user_identity` 同一交易刪 `user_identity`
並撤銷未過期的 session（不撤等於沒解），拒絕 `password_hash IS NULL` 的帳號——解完
沒有人登得進去，而畫面上補不回密碼，那條路只剩主機上的 `laysi-admin reset-password`。
`delete_user_account` 拒絕還連著外部身分的帳號：自動建帳號讓刪除自己撤銷自己，下一次
登入又建一個新的。解除連結本身也不擋登入（同一個 email 會由第 2 步重新連上），要擋
就停用帳號。帳號管理在連著外部帳號的那一列不畫「刪除」，改畫「解除連結」。

**第一個帳號可以在網頁上建立（`POST /v1/auth/bootstrap`，畫面 `screens/Init.tsx`）**，
它是 `/v1/auth/*` 例外的下一層：連要被檢查的身分都還不存在，所以守門條件是安裝
本身的狀態。保證來自 SQL 不是 Rust——`install_bootstrap` 是單列表，
`bootstrap_first_user` 在同一個交易裡先 `INSERT ... WHERE NOT EXISTS (帳號)
ON CONFLICT (id) DO NOTHING` 再建帳號，所以併發只會成功一次，事後清空
`user_account` 也不會重新打開。角色寫死 `admin`（`laysi_ops::ROLE_ADMIN`）**不是
`boss`**、登入名稱與顯示名稱同樣寫死（見上一條）、密碼由本人設定（沒有
`must_change_password`），呼叫端只送一個密碼，不能指定任何其他欄位。
負責人與其他成員的帳號之後由系統管理員用 `create_user_account` 在「帳號管理」建立；
那個操作不收密碼，密碼由系統產生、只回一次、強制首次登入變更。`SETUP_ALLOW_WEB=false`
關掉整條路，`SETUP_TOKEN` 非空時要求輸入代碼（行程內只留 BLAKE3 雜湊）。
未登入的判斷資料來自 `/v1/auth/me` 的 `setup` 欄位。**這不是 Registry 操作，
也不是導覽畫面**，所以不計入操作與畫面的數量。

**忘記密碼由 `reset_user_password`重發一組臨時密碼**，它同時撤銷該帳號所有
登入工作階段——不撤等於沒換。兩個拒絕：`password_hash IS NULL` 的帳號只用外部身分
登入（發密碼等於繞過 IdP 的多因素），以及自己的帳號（畫面上不出現按鈕：重設會把自己
登出，密碼視窗會跟著消失）。真的忘了自己的密碼只能請另一個管理員重設，或用
`laysi-admin reset-password`。

**休假單收的是時段，時數與天數由後端算**：`request_leave` 只收 `from_at`／
`to_at` 兩個當地時刻，`hours` 是這段區間落在上下班時間內的部分（逐日相加，週六與
週日不計），`days` 等於 `hours` 除以一天的上班時數，兩者在送出當下定案並存下來——
上下班時間是單一現值、沒有生效日，事後改它不該回頭改寫已核准的單。`from_on`／`to_on`
改由 `from_at`／`to_at` 推導，是資料庫的 generated column。**上下班時間沒填就送不出
休假單**，畫面在那個狀態不畫表單，改請系統管理員去填；國定假日不在系統裡，跨到國定
假日的休假會多算那一天。

**計薪基準與僱用型態是兩個軸**：`employee.pay_basis` 是 `monthly` 或
`hourly`，決定 `base_salary` 是月底薪還是時薪；`employment_type` 只是僱用型態，兼職
也可以領月薪。時薪制以打卡工時計薪（`PunchDay::worked_seconds`，只採打了上下班的完整
日），**本期沒有完整打卡就不產生薪資單**而是列進 `skipped`——一張 0 元薪資單看起來像
算出來的金額。加班費走 `ordinary_hourly_wage`：月薪除以 `monthly_hours`，時薪直接是
它自己，少了這一步時薪員工的加班費會被除以 240。最低工資的下限跟著 basis 走
（`minimum_wage_shortfall` 收 basis 當 payload 鍵名，不認得就回錯誤）：`monthly` 與
`hourly` 是兩個並列的下限，拿錯一個那個檢核項目就永遠說同一句話。**特休不檢核兼職與
時薪員工**——部分工時的特休按約定工時比例給予，而系統沒有記錄約定工時，用全時額度比
等於全部通過。

**負責人的投保分擔比率在 `owner_insurance` domain**：`employee.insured_as`
是 `employee` 或 `owner`，`premiums_on` 據以換掉健保與勞退兩項——健保保費全額自付
（健保法第 27 條第 1 款第 1 目）、勞退不強制提繳（勞退條例第 7 條）。**費率不在那裡**：
健保 5.17% 對誰都一樣，仍取自 `health_insurance`，換掉的只有分擔比率。
**不把 `owner` 節點塞進 `health_insurance`**：那會逼出一個同日生效的新版本，而
`resolve()` 以 `effective_from` 取最大值，同一天的兩版誰勝出由資料庫回傳順序決定。
**勞保沒有負責人節點**：勞保條例第 15 條第 1 款對實際從事勞動之雇主與受僱者是同一組
70／20／10，抄一份只會多一個會漂走的副本。`missing_brackets` 對負責人只追健保——
勞保與勞退留白是投保事實，不是漏填。健保投保金額不得低於受僱員工最高者由
`get_hr_compliance` 說，不在寫入端擋：名冊存的是申報事實。

**人資模組不含任何費率**：勞健保勞退由 `laysi-regulation` 以**期末日**解析、
扣繳以**給付日**解析（扣繳義務發生在給付當下）、加班費以**加班當日**解析。薪資過帳
產生待過帳傳票（借 6111 ／貸 2117、1102）；公司負擔的保費**不在那張傳票裡**，那條
銜接還沒接上，畫面照實標成未接上，不逕行併進 6111。

**工作日曆只存例外**：`work_calendar` 一天一列，只放與星期不同的日期——
預設是週一至週五工作日、週六休息日、週日例假，而政府辦公日曆表多出來的資訊剛好就是
國定假日、調整放假與補行上班，一年約二十列。五種日別
（`workday`／`rest_day`／`regular_leave`／`holiday`／`natural_disaster`）由
`WorkSchedule` 帶著走，
`leave_seconds()`（非工作日不計時數）、`overtime_seconds()`（非工作日不扣一天的長度，
出勤第一秒就是加班）與 `overtime_day_type()`（決定用哪一種日別計價）三處讀它。
**不內建任何日期**（年年不同，內建一份等於印一份沒人看得出錯在哪的假日曆）：
`refresh_work_calendar` 向政府資料開放平臺（資料集 14718）取，依 `resourceDescription`
的民國年前綴挑檔案並**取最後一筆**（更正版是附加而不是取代），跳過 `_Google行事曆專用`
與非 UTF-8；欄位依欄名找不依位置，檔案帶 BOM 要先去掉；網址是常數不收參數。
**第一次啟動自動抓當年與次年**（`ops::calendar::seed`，表是空的才抓），整段 spawn 出去
**不擋啟動也不讓啟動失敗**，失敗就留著空表讓下一次啟動再試。只覆寫 `government` 列，
`company` 列保留不動並回報保留了幾天；`set_work_calendar_day` 的 `kind` 留空＝刪掉該列。

**天災停班（颱風假）不在政府檔案裡也永遠不會在**：辦公日曆表提前一年公布，停止上班是
前一晚由各縣市宣布的，人資在畫面上登記。**它不是國定假日**——勞基法對天災停班日的工資
與出勤加給都沒有明文，與國定假日的「工資照給、出勤加倍」是兩件事，所以是第五種日別而
不是換個名字的國定假日。**每一種休息日仍以休息日費率計價**：第 39、40 條是按日加倍而
不是時數級距，payload 沒有那個形狀。

**加班要先申請再核准，加班費由規則庫算**：`submit_my_overtime_request` 收起訖
時刻，時數等於區間長度；`decide_overtime_request` 核准後，`run_payroll` 依 `overtime.toml`
的倍數與「月薪 ÷ `monthly_hours`」算出金額，**倍數與時數上限都不在 Rust 裡**。加班費
**不存欄位**——它是時數乘一個有生效日的倍數，存下來等於凍結一個修法有權回頭改的數字，
與扣繳稅額同一條理由。平日申請的時段不得與上下班時間重疊（那段時間月薪已經給付），
一日與每月上限（勞基法第 32 條）由規則庫給，超過就擋下並說出還剩多少。**沒有核准
紀錄的加班不計入**（也沒有第二個來源可以沿用）。**國定假日與例假不做**：
那兩種日子按日加倍發給，而規則庫沒有國定假日日曆，判定不了某一天屬於哪一種；週六與
週日一律當休息日，與休假時數的假設同一套。打卡推導出的加班仍然只有時數。

**底薪 0 表示尚未設定，不是不給薪**（CHECK 是 `base_salary >= 0`）。人先存在，薪資
之後才談定；該擋的地方在計薪，`run_payroll` 遇到 0 底薪回「尚未設定底薪」而不是發出
0 元薪資單。**`create_user_account` 一定會有一筆員工主檔，
沒有開關**：`link_employee()` 既有編號就連結，指不到人（含留空）就建一筆只有姓名與
到職日的員工主檔，留空時由 `create_employee` 在寫入的交易內依 `E###` 配號（取
`EMPLOYEE_NO_LOCK_KEY`，三條配號的路共用；資產編號同理），部門與金額留給人資。
沒有這條，全新安裝的第一天發不出可用的帳號：建人要薪資、薪資是人資的資料、人資權限
不在系統管理員身上。原本它是預設關閉的 `new_employee` 旗標，但關著建出來的帳號打不了
卡、請不了假、報不了帳，等於預設把帳號建壞。`update_user_account` 的 `employee_no`
是三態：不帶不動、空字串解除、給編號就連結或建立。

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

**彈性工時與遲到寬限是兩件事**：寬限原諒遲到但下班時間不動，彈性把整天
往後移、長度不變。`expected_end()` 因此吃「幾點到」，`overtime_seconds()` 收
`(arrived, left)`。早到不縮短當天。留空＝不採彈性工時。

**`update_company_setting` 的空字串是清除，只對 `CLEARABLE_SETTINGS` 有效**
（`work_start`／`work_end`／`late_grace_minutes`／`flex_window_minutes`）：`late()` 回
`Option<bool>`，`None` 是「沒問」不是「沒遲到」，所以公司要說得出「還沒決定」，設過
之後也要收得回來。其餘設定仍必填。首次設定精靈第三步問上下班時間——跑精靈的就是唯一
改得動這一項的人。

**首次設定精靈第五步就建負責人帳號**：`create_user_account` 是 `AdminOnly`，跑精靈的
本來就叫得動它，所以沒有理由把它留到精靈結束後才用一句話指過去——那等於讓人填完四步
才發現剛剛登入的不是自己要用的帳號。角色寫死 `boss`，臨時密碼在同一步顯示一次並附
複製鈕，離開該步就看不到。已經有啟用中的負責人帳號時這一步只說已經有了。這一步可以
略過，所以「基礎設定」那張 `ownerMissing` 提示留著當退路，而完成頁只在真的沒有負責人
帳號時才提它。

**外部登入排在負責人帳號的前一步（第四步）**：順序就是相依關係——負責人要不要密碼，
由外部登入設好了沒有決定。這一步問 Google 需要的四件事（用戶端 ID、密鑰、允許的
網域、首次登入建不建帳號）加上要不要停用帳號密碼登入，issuer 與轉址網址由程式填；
轉址網址是 `readonly` 欄位加一顆複製鈕，不是一句叫人去複製的話。停用的開關放在這裡
而不是留給事後的安全營運頁，是因為它和下一步是同一個決定：關掉密碼登入卻給負責人一組
密碼，等於建一個永遠登不進去的帳號。

**`create_user_account` 的 `external_login_only` 建立沒有密碼的帳號**：`password_hash`
是 NULL、`must_change_password` 為 false、回應的 `temporary_password` 是 `null`。三個
前提缺一不可，寫入前就擋下——外部登入已啟用、設為自動建立帳號、地址在允許的網域內。
理由是 `resolve_account` 的第 2 步（依 email 認既有帳號）**只在自動建帳號開啟時才會
執行**，所以少了任一個前提，這個帳號沒有密碼也沒有任何外部身分連得上，而事後沒有
任何操作補得回密碼（`reset_user_password` 拒絕 `password_hash IS NULL`）。精靈的
「登入方式」欄位即時反映這三個前提：地址落在網域內就顯示供應商名稱，否則顯示「密碼」。

**反過來也要擋**：`password_login_disabled` 開著時，`create_user_account` 拒絕建立
沒有 `external_login_only` 的非 `admin` 帳號——那個帳號只有一組它永遠用不到的密碼。
`admin` 是例外，理由與登入端的例外相同。

**「登入方式」欄位在精靈與帳號管理是同一個**：唯讀、跟著 email 即時變，網域內顯示
供應商名稱、網域外顯示「密碼」，密碼登入停用時顯示「密碼（已停用）」並轉
`input--dead`、把建立鈕關掉。兩個畫面都不另外印一段話解釋——判斷看得見就不必再讀一遍。
免密碼建出來的帳號沒有密碼卡可顯示，所以帳號管理改發一則 toast，不靜靜關掉抽屜。

**外部登入成功後要清掉 `must_change_password`**：系統管理員代建的帳號帶著一組產生的
密碼與這個旗標，而從 IdP 進來的人從來沒拿到那組密碼——變更密碼表單會先跟他要「目前的
密碼」，於是一次成功的登入被擋在自己的畫面外。只清旗標不動雜湊：那組密碼還在，之後
真的想用密碼登入時 `reset_user_password` 仍叫得動。

**漏打卡由本人送出補登申請**：`submit_my_punch_correction` 不收任何指定
對象的欄位，對象由 `ctx.employee_id` 決定；申請寫在 `punch_correction`，**核准才
寫進 `attendance_punch`**，而且核准與寫入在同一個交易（`punch_in_tx` 是從
`record_punch` 拆出來的寫入半段）。要不要審核是公司設定
`punch_correction_policy`（`人資審核`／`自動通過`，存的就是畫面上印的字），
改得動它的是系統管理員。**只有幾個答案的設定由 `get_company_settings` 的 `options`
帶給畫面**，有值就畫下拉、沒值才是文字欄，`update_company_setting` 同時只收清單內的
值——把兩個答案交給人逐字打，打錯就靜靜落回預設那一個。
一格只能有一張未決的申請（部分 UNIQUE 索引），申請人不得決定本人送出的申請。
員工端的時間欄用 `components/TimeField`（時、分兩個下拉），不是純文字：那條「補登
用純文字」的規則是為了 `correct_punch` 收秒，而本人憑記憶填的時間沒有秒。

**每一次更正打卡都留下原時刻**：`punch_in_tx` 是 `ON CONFLICT DO UPDATE`，
被改掉的那個時刻只存在於 `punch_correction.replaced_at`（`NULL` ＝該時段原本是空的）。
人資的 `correct_punch` 因此也寫一列 `punch_correction`，寫入當下即為已核准——不寫等於
一次更正之後，原本的時間在系統裡哪裡都不存在，而出勤紀錄要保存五年。`record_punch`
的第二個參數是 `correcting: Option<&str>`（更正者的角色），會覆蓋的那一次就是要記錄的
那一次。人資直接補登與員工申請經核准的判準是 `decided_by = created_by` 且不為空，
不是比對顯示名稱。**一格可以更正很多次，所以整條鏈要掛在它改的那一天上**：逐日明細的
狀態欄是「補登紀錄 N 筆」，按下去展開 `components/PunchTrail`（由舊到新，原時刻 →
新時刻、寫的人、日期、事由），人資與員工兩邊同一個元件。畫面取 `list_punch_corrections`
一律 `include_done: true`，「含已決定」在前端過濾——分兩次取會讓其中一邊看到半條鏈。

**打卡記錄到秒**，時數與加班以秒計算、遲到在秒的精度上比較；
`correct_punch` 收 `HH:MM:SS`，只給分鐘時記為秒零。勞基法第 30 條第 7 項要求的是
「至分鐘為止」，記到秒是超過而非符合，所以程式與畫面都不宣稱這是法規要求。
`punch_clock` **不收時間欄位**，
記的就是呼叫當下那一刻，連 `LAYSI_TODAY` 都不移動它；漏打走補登（本人送申請或
人資直接補），`source` 記成 `manual` 並在畫面上標示。上下班時間是公司政策，
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

**收據是送位元組，不是指既有文件**：`attach_my_expense_receipt` 收
`file_name` ＋ `content_base64`，輸入指不到任何既有文件。`claim_no` 指的是單、
單屬於人，所以 `expense_claim` 有 `employee_id` 並寫進 `WHERE`——
`claimant` 是顯示名稱、會同名，不能當寫入側的鍵；會計代建的單那一欄是 NULL，
自助這側看不到。只有 `submitted`／`approving` 收件。先建單再附件（`link_ref`
要單號），所以上傳失敗時單已存在，畫面要說哪幾個沒附上並請去「我的請款」補，
不要重送整張單。

**檔案內容不進稽核紀錄**：`dispatch` 寫 `ai_audit_log` 前先過 `redact_for_audit`，
超過 512 位元組的字串改記長度。判準是值的長度不是欄位名，新的帶位元組操作自動
涵蓋；是哪個檔案由 `document_version` 的 BLAKE3 回答（那是落地位元組的雜湊）。

**用 MCP 不是主管特權**：`issue_my_api_token` 不收 `actor`／
`actor_role`，一律取自 session，所以那把 Token 能做的事與本人完全相同。所有權的鍵
是 `api_token.owner_employee_id`（`actor` 是顯示名稱、會同名，不能當鍵），只有自助
核發會寫入；`revoke_my_api_token` 把它寫進 `WHERE`，別人的 id 是「查無此列」。
同一欄也是 `mcp::token_identity` 取得 `employee_id` 的唯一來源——不從姓名猜，
老闆代發的 Token 沒有 owner，自助操作對它一律回「尚未連結員工編號」。五個唯讀自助
工具因此才對 AI 打開；寫入類與薪資明細維持關閉。

**AI 工具開關改得動了**：`set_mcp_tool_grant` 是 `AdminOnly`，「MCP 與 AI」那一頁
的「AI 可用」欄對有權限的人畫成開關。它自己那一列種成關閉且**拒絕把自己打開**——開了等於
讓管理員的 MCP Token 自行開啟其餘每一項，所以那一列不畫開關，改印「無法對 AI 開啟」。

**新增操作時 `mcp_tool_grant` 與 `permission` 要同步**：`laysi-ops` 有三個測試以
`include_str!` 讀 `0001_initial.sql` 的種子資料，比對 Registry 與 grant 的名稱集合、
每個操作恰好屬於一條 permission，以及每一列 grant 的風險分級與操作本身相同。少一筆
grant，AI 看得到一個叫不動的工具；少一條 permission，deny-by-default 讓那個操作永遠
打不開；分級寫低了，`scripts/mcp-role-matrix.ts` 的 INV1 會放過它本來要抓的那個工具。

## 示範站

`demo.laysi.one` 與 `one.laysi.io` 同一台主機、同一份映像、同一份
`docker-compose.prod.yml`，只多一層 `docker-compose.demo.yml`。**推 `v*` 標籤就部署
示範站**（`release.yml` 呼叫 `deploy.yml` 並帶 `target: demo`），正式站維持手動——
客戶的安裝什麼時候升級由持有它的人決定，而示範站沒有持有人也沒有東西可失去。

**每晚整份重建**（`deploy/reset-demo.sh`，systemd timer 台北時間 04:00）。共用的評估
安裝沒有第二條路可以維持可用：第一個過帳、簽核或改公司名稱的訪客就改變了第二個訪客
看到的東西，而把昨天的改動分成「示範」與「有人在試」比整份丟掉貴。**`secrets` 資料卷
不動**，簽章之外資料庫密碼與初始化代碼也在裡面。

**五個共用帳號由 `demo_freeze_shared_login` 凍結**，只認 `admin`／`boss`／`hr`／
`acct`／`demo` 這五個名字：改密碼、停用、換角色與刪除一律還原。只綁那五個名字是刻意的
——訪客自己建的帳號照常改得動、停得掉、刪得掉，帳號管理示範的仍然是真的行為。

**示範站要有真授權**：全新的資料庫沒有 90 天寬限期（0007 只補給看得到
`install_bootstrap` 的既有安裝），而未授權的安裝唯讀。金鑰放在主機的
`~/.config/laysi-demo.env`，重建時由 `POST /v1/auth/bootstrap` 一併啟用——那是唯一
不需要瀏覽器就能啟用授權的路。其餘見 `deploy/README.md`。

**MCP 跑著但沒有對外入口**：`docker-compose.demo.yml` 不發佈埠也不接 `proxy` 網路，
tunnel 沒有東西可以指。關掉它是少一條 ingress，不是一個設定。
