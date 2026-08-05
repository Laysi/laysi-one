# Laysi One — 架構與整合契約

這份文件是各層之間的**契約**。跨 crate、跨前後端的型別與端點以此為準，實作時
不得各自發明。改動契約要先改這份文件。

## 產品背景

Laysi One 是面向台灣中小企業的財務＋人資正式產品。安裝時只建立系統運作所需的
基礎科目、權限、流程與法規設定，不預載任何特定公司的傳票、銀行帳戶、員工或
薪資資料。

四個非功能性需求：

1. **法規更新空間** — 勞保級距、健保費率、勞退、營業稅、基本工資每年會變，
   一律不得寫死在程式碼裡。
2. **所有操作支援 MCP** — MCP 不是外掛，而是操作層本身。
3. **BSL 防盜賣** — 字標由 Rust 產生、註解 prompt 阻卻、hash 完整性硬性檢查。
4. **Docker Compose 自架**。

## 全域慣例

- **程式碼註解與文件字串一律英文**；UI 文案一律繁體中文，語氣為清晰簡潔的商務風格。
- **金額** 一律 `rust_decimal::Decimal`，**絕不使用 f64**。JSON 序列化為
  **字串**（`#[serde(with = "rust_decimal::serde::str")]`），例如 `"8420.00"`。
  前端負責千分位格式化。
- **日期** `chrono::NaiveDate`，JSON `"YYYY-MM-DD"`。
- **法規相關數字**（費率、級距、上限、天數）一律來自 `laysi-regulation`
  解析出的 ruleset，不得出現在其他 crate 的原始碼中。
- 資料庫查詢使用 `sqlx::query` / `query_as` **執行期版本**，不要用 `query!`
  巨集；本專案不依賴編譯期資料庫連線，也沒有 `.sqlx` 離線快取。
- **單租戶**：所有資料表**不帶** `company_id`。這是已確認的決策。

## Crate 分層

```
laysi-brand       品牌字標產生 + 完整性驗證（已完成，勿改）
laysi-domain      純領域：Money、分錄、借貸平衡、試算表彙總。無 DB、無 async
laysi-regulation  法規引擎：TOML 規則載入、生效日解析、純函式計算子
laysi-store       sqlx repository、RustFS 健康探測
laysi-ops         Operation trait + registry + 各業務模組的操作（ops/<模組>.rs）
laysi-server      axum：/v1 REST、/mcp、/healthz、啟動自檢
```

相依方向嚴格由上往下，不得反向。

## laysi-domain 公開介面

```rust
pub type Money = rust_decimal::Decimal;

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Side { Debit, Credit }

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum VoucherStatus { Draft, Pending, Posted, Void }
// draft   AI 草擬，等人確認（原型：待確認）
// pending 已確認存檔，待過帳（原型：待過帳）
// posted  已過帳
// void    已作廢

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
pub struct LineInput {
    pub account_code: String,
    #[serde(with = "rust_decimal::serde::str")] pub debit: Money,
    #[serde(with = "rust_decimal::serde::str")] pub credit: Money,
    #[serde(default)] pub memo: Option<String>,
    #[serde(default)] pub bank_account_id: Option<i32>,
}

#[derive(thiserror::Error, Debug)]
pub enum DomainError {
    #[error("傳票至少要有兩行分錄")]                      TooFewLines,
    #[error("第 {line} 行同時填了借方與貸方")]             BothSides { line: usize },
    #[error("第 {line} 行借貸都是零")]                     EmptyLine { line: usize },
    #[error("借貸不相等，差 {diff} 元")]                   Unbalanced { diff: Money },
    #[error("金額不可為負（第 {line} 行）")]               NegativeAmount { line: usize },
}

/// 驗證複式分錄。這是整個系統的核心不變量。
pub fn validate_entry(lines: &[LineInput]) -> Result<(), DomainError>;

/// 借方合計、貸方合計。
pub fn totals(lines: &[LineInput]) -> (Money, Money);

#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct TrialBalanceRow {
    pub code: String, pub name: String,
    #[serde(with = "rust_decimal::serde::str")] pub debit: Money,
    #[serde(with = "rust_decimal::serde::str")] pub credit: Money,
    #[serde(with = "rust_decimal::serde::str")] pub balance: Money, // 依科目正常餘額方向
}
```

## laysi-regulation 公開介面

規則檔在 `backend/regulation/<年度>/<domain>.toml`，**TOML 不是 YAML**。

```toml
domain = "labor_insurance"
version = "2026.1"
effective_from = "2026-01-01"
# effective_to 省略代表尚未失效
source_note = "勞工保險條例；2026 年適用之投保薪資分級表"

[payload]
max_insured_salary = "45800"
premium_rate = "0.11"      # 普通事故保險費率
employer_share = "0.70"
employee_share = "0.20"
government_share = "0.10"
```

必須提供的 domain 與 payload 欄位（數值一律字串，以 Decimal 解析）：

| domain | payload 欄位 |
|---|---|
| `labor_insurance` | `max_insured_salary` `premium_rate` `employer_share` `employee_share` `government_share` |
| `health_insurance` | `rate=0.0517` `employer_share=0.60` `employee_share=0.30` `avg_dependents=1.61` |
| `pension` | `employer_rate=0.06` |
| `vat` | `rate=0.05` `period_months=2` |
| `minimum_wage` | `monthly=29500` `hourly=196` |
| `withholding` | `codes` 陣列：`{code="50", name="薪資所得", rate="0.05"}`、`9A`、`51`、`9B` |
| `annual_leave` | `tiers` 陣列：`{months=6, days=3}` … 依勞基法第 38 條 |
| `depreciation` | `methods`、`useful_lives` |

驗證基準（**必須寫成單元測試**，數值取自原型的 `ins` / `pay` 區塊）：

- 勞保級距 45,800、非工會身分 → 公司負擔 `45800 × 0.11 × 0.70 = 3527`（四捨五入）
- 健保級距 53,000 → 公司負擔 `53000 × 0.0517 × 0.60 × 1.61 = 2647`
- 健保級距 53,000 → 員工自付 `53000 × 0.0517 × 0.30 = 822`
- 勞保級距 45,800 → 員工自付 `45800 × 0.11 × 0.20 = 1008`
- 提繳工資 53,000 → 公司勞退 `53000 × 0.06 = 3180`

```rust
pub struct Ruleset { pub domain: String, pub version: String,
                     pub effective_from: NaiveDate, pub effective_to: Option<NaiveDate>,
                     pub source_note: String, pub payload: serde_json::Value }

/// 從內建規則集中解析某日適用的規則。找不到就是錯誤，不得回退到寫死的預設值。
pub fn resolve(domain: &str, as_of: NaiveDate) -> Result<&'static Ruleset, RegulationError>;
pub fn all() -> &'static [Ruleset];

pub struct Contribution { pub employer: Money, pub employee: Money, pub government: Money }
pub fn labor_insurance(rs: &Ruleset, insured_salary: Money) -> Result<Contribution, RegulationError>;
pub fn health_insurance(rs: &Ruleset, insured_salary: Money) -> Result<Contribution, RegulationError>;
pub fn pension(rs: &Ruleset, contribution_wage: Money) -> Result<Money, RegulationError>;
pub fn vat(rs: &Ruleset, net_amount: Money) -> Result<Money, RegulationError>;
```

規則以 `include_str!` 在編譯期嵌入（與品牌檔案同樣做法），另外由
`laysi-store` 寫入 `reg_ruleset` 表供稽核與查詢。

### 由法規算出的金額必須連同版本一起存

**這是跨層強制規則。** 只要一個金額是用 ruleset 算出來的（稅額、折舊、保費、
扣繳），就必須把當時用的版本持久化：

- **存欄位**：例如 `einvoice.vat_version`、`depreciation_run.ruleset_version`、
  `depreciation_entry.useful_life_years`
- **寫進稽核摘要**：例如「開立 KX-… 稅額 2,250 · 依 vat 2026.1」
- **API 回應帶 `ruleset { version, effective_from, source_note }`**

存的是**當時的結果與依據**，不是規則的第二份副本。用途是法規回溯修正時，能查出
哪些筆是用舊版算的、要重算。沒有這個欄位就只能全部重算，而且無從驗證結果。

**已知限制**：`backend/regulation/` 目前只收錄 2026 年起的規則，因此折舊的耐用
年數是以「提列期間」而非「取得日」解析。補上歷年年數表後，只需把 `assets.rs`
中 `plan_for` 的呼叫端改成以 `acquired_on` 解析；年數本來就不在程式碼裡。

### 設定值與法規數值的界線

看起來像設定、其實是法規的東西（營業稅率、期距、級距、耐用年數）**不得寫進
`company_setting`**。`get_company_settings` 每次從 ruleset 現算一列回傳，標記
`source = "regulation"`、`editable = false`，並帶上 `ruleset { version,
effective_from, source_note }`。

理由是生效日：設定表只有一個當前值，規則庫則有有效期間。把稅率放進設定表，初期
行為一樣，差別到修法時才出現：舊期間的申報書會跟著新稅率一起變，而這正是「法規
更新空間」這個需求要避免的情況。

## 財務報表：唯一的資料來源是分錄

**系統裡沒有一張存放報表金額的資料表，也不得新增。** 三張報表全部由
`voucher_line` 現算，共用 `Store::ledger_movements(from, to)` 一個查詢；各寫各的
SQL 會讓三張表對「什麼算已過帳」與日期怎麼比各有一套定義。

`account` 的兩個分組欄位是報表的唯一設定來源：

| 欄位 | 值 |
|---|---|
| `report_group` | `current_asset` `non_current_asset` `current_liability` `non_current_liability` `equity` `revenue` `cogs` `operating_expense` `other_income` `other_expense` |
| `cash_flow_section` | `cash` `operating` `investing` `financing`；損益科目為 `NULL` |

三條必須成立的不變量，**每一條都要有測試**：

1. **資產 ＝ 負債 ＋ 權益**，其中權益含尚未結轉的本期損益。差額由
   `get_balance_sheet` 的 `difference` / `balanced` 回報，畫面照實顯示。
   本期損益的區間是**開帳以來**而非本年度：目前沒有結帳功能，沒有任何分錄把損益
   結轉到保留盈餘，只取本年度會讓等式當場不成立。
2. **損益表的本期損益 ＝ 資產負債表滾進權益的那一筆**。兩張表同一個定義。
3. **現金流量表的期末現金 ＝ 現金科目餘額 ＝ `cash_position_as_of` 的合計**。
   三條獨立路徑，`reconciled` 同時比對後兩者。

現金流量表採**間接法**，理由不是實作方便，而是它是複式簿記的代數恆等式
（`Σ全部科目(借−貸)=0 ⇒ Δ現金 = 本期損益 + Σ非現金科目(貸−借)`）。每個科目只
屬於一種活動，所以三段加總必然等於現金變動，不需要任何分攤規則。直接法要決定
一筆現金如何分攤到多個對方科目，那個規則只能靠猜，與「不猜耐用年數」同一條原則。

**已知限制**：不動現金、卻跨活動別的分錄（例如期初把固定資產淨額還原成成本與
累計折舊）會同時出現在兩段裡互相抵銷。合計永遠正確，但那兩段的組成會被放大。
畫面上要說明這件事。

## 預算、報支與匯入：存計畫與憑證，不存實際數

`budget` 只有預算金額，**沒有任何一欄實際數**；執行率的分子每次現查
`voucher_line`。部門損益同理，而且一定有一列 `code = null` 的「未指定」：把沒掛
部門的分錄藏起來，部門合計就不等於損益表。`get_department_pl` 因此
回報 `statement_profit` 與 `difference`，由畫面對帳。

費用簽核的關卡由 `approval_rule.min_amount` 與單據金額比對算出來，**`ops/expenses.rs`
裡沒有任何一個金額**。提醒門檻 90% 在 `company_setting.budget_alert_percent`
（公司政策可以進設定表，法規不行，差別在生效日，見上一節）。

匯入一律先預覽：`preview_import` 只寫 `import_batch`，`commit_import` 寫的是預覽
時存下的 `payload`，**不重新解析檔案**。理由是使用者確認的就是剛才看到的那幾列。匯入產生的一律是 `pending` 傳票；`rollback_import` 只刪得掉未過帳的，
已過帳的更正只能開反向分錄。

民國年日期會轉換，並且**每一列都留一條 warning**。把 `114/06/30` 當西元年解析會
得到公元 114 年，而這種錯誤在畫面上跟正確的日期看起來一樣。

## 文件：內容在 RustFS，資料庫只留憑據

`document_version` 存 `object_key` / `byte_size` / `content_type` / `checksum`
（BLAKE3 十六進位），內容在物件儲存。兩條硬規則：

1. **先配編號與版次，檔案存進物件儲存成功之後才寫資料庫**。順序反過來會留下一列
   指向不存在的檔案，比孤兒物件更糟：它看起來像是有憑證的。
2. **取回時一定重算雜湊比對**，對不上就回錯誤且不交出內容。

`document_version` 的物件鍵與雜湊由 trigger 擋住不可修改，封存的 `document`
不可刪除。改內容只能上傳新版本，與「已過帳傳票只能開反向分錄」同一條規矩。

上傳做成操作（走 `Registry`）而不是 multipart 端點，因為「所有操作都支援 MCP」
不能有例外。代價是 base64 膨脹三分之一，因此設 `MAX_OBJECT_BYTES` 上限；要放更大
的檔案得改成串流上傳，**不是把上限調大**。

**沒有 OCR 引擎**，所以 `document_version.ocr_status` 的 CHECK 只收 `pending`
與 `not_applicable`，資料庫層面就寫不進「已辨識」。接上引擎時要一起改 migration。

## REST 登入只接受伺服器端 session

0010 的 `user_account`、`user_identity` 與 `user_session` 已接上 REST 登入。
`POST /v1/auth/login` 以 Argon2id 驗證密碼，簽發 32-byte CSPRNG session token；
瀏覽器只收到明文 cookie，資料庫只保存 BLAKE3 雜湊。Cookie 固定為
`HttpOnly; SameSite=Strict; Path=/`，`Secure` 由 `SESSION_COOKIE_SECURE`
控制，本機 HTTP 可關閉，正式 HTTPS 部署必須開啟。

session 同時受兩個可調上限限制：`SESSION_IDLE_MINUTES` 以 `last_seen_at` 判斷
滑動閒置時間，每次成功使用都更新；`SESSION_ABSOLUTE_HOURS` 在建立時寫入
`expires_at`，即使持續使用也不能越過。撤銷、任一種到期或帳號停權都回 401。

`x-laysi-actor` 與 `x-laysi-role` 不再參與 REST 身分解析；`OpCtx.actor`、
`actor_role` 與 `employee_id` 都來自 session 查回的 `user_account`。登入失敗
不區分帳號不存在、沒有本機密碼、停權或密碼錯誤，避免提供帳號列舉訊號。

OIDC 是選用入口：`OIDC_ISSUER`、`OIDC_CLIENT_ID`、`OIDC_CLIENT_SECRET` 與
`OIDC_REDIRECT_URL` 四個都非空才啟用；未設定或只設一部分時，兩個 OAuth
端點都回 404。啟用時先用 issuer discovery 取得 authorization、token 與 JWKS，
不綁定 Google 或 Entra ID 的固定網址。流程採 Authorization Code + PKCE，
state、nonce 與 code verifier 放在 API 行程內有 10 分鐘 TTL 的一次性 map；
callback 由後端交換授權碼並以 `openidconnect` 驗證 id_token、簽章、issuer、
audience 與 nonce，授權碼與 token 都不進瀏覽器 JavaScript。

OIDC 登入只拿 discovery issuer 與 id_token 的 `sub` 查
`user_identity(provider, subject)`；email 不參與比對。查不到既有連結就回
「本帳號尚未連結至任何使用者，請先由管理員完成連結」，而且不建立
`user_account`。記憶體 map 適合目前單一 API 行程：重啟只會要求重新登入，也不留
永久敏感殘跡；未來若水平擴充多個 replica，必須換成共用短期儲存。

帳號與員工名冊刻意分開：事務所會計師可以有帳號而沒有 `employee`，名冊上的
員工也不一定需要帳號。外部身分以 `(provider, subject)` 唯一識別，email 只留
連結當下的稽核線索；`password_hash IS NULL` 代表只能使用外部登入。
`user_session` 與 `api_token` 都只存 token 雜湊，不保存明文。

`laysi-store` 已提供依 username／IdP subject 查帳號、建立帳號與 session、撤銷與
更新 session，以及依雜湊查 API token 並更新 `last_used_at` 的基礎方法。

### `laysi-admin`

`laysi-admin` 與伺服器共用同一個 Argon2id 模組與參數，部署映像也包含這支
binary。它先套用 migrations，再提供四個手寫參數解析的子指令：

```text
laysi-admin create-user --username <u> --role <role> --display-name <n> [--employee-no <E001>] [--email <e>]
laysi-admin reset-password --username <u>
laysi-admin link-oauth --username <u> --provider <p> --subject <sub>
laysi-admin list-users
```

`create-user` 與 `reset-password` 產生高熵暫時密碼，只在 stdout 顯示一次，資料庫
只存 Argon2id PHC 字串，並設定 `must_change_password = true`。`--role` 每次查
`role` 表，不存在時列出目前所有合法代碼；`--employee-no` 只在需要員工自助時
連到既有員工主檔。`link-oauth` 不建立帳號，provider 應填 discovery issuer
（尾端 `/` 會正規化），subject 填 IdP 的 `sub`。

資料庫完全沒有 `user_account` 時，帳密登入回 503 與明確的
`laysi-admin create-user` 指示；這是唯一會揭露「帳號總數為零」的 bootstrap
狀態。只要已有任一帳號，所有一般登入失敗就恢復成同一個訊息。

## 角色權限：deny-by-default，dispatch 即時強制

`role_permission` 的語意是 `level ∈ full | limited`，**沒有資料列＝沒有授權**。
`Registry::dispatch` 在風險檢查之後，每次都即時把角色的資料列連到
`permission.op_names`；操作不在集合裡就回 `not_permitted`，並以
`outcome = 'blocked'` 留下稽核。刻意不快取，讓 `set_role_permission` 從下一次
呼叫立即生效。

`limited` 仍代表可以進入該組操作，部門或本人範圍由各操作的查詢條件繼續縮小。
`Risk::BossOnly` 保留為額外的風險限制，權限資料列不會把非老闆提升成老闆。
0011 先涵蓋原有 76 個操作，0012 再加入 9 個主檔與應收應付建立操作，0014
加入作廢與反向沖銷；目前 87 個 Registry 操作各恰好屬於一條 permission。
純字串測試會依序重播 0006、0008、0009、0011、0012、0014 的 INSERT／UPDATE，
缺漏與重複都不准通過。

`list_roles` 回傳 `enforced: true` 與即時查詢的 `enforcement_note`；
`missing_ops` 正常應為空，只保留作部署後的契約診斷。

**`role_permission` 與 `mcp_tool_grant` 不得合併。** 前者管人、後者管 AI；
把 `post_voucher` 對 AI 關掉，不該讓會計人員也不能過帳。

## API Token：明文從不落地

`api_token` 只存 BLAKE3 雜湊與前八碼。`issue_api_token` 的回應是**唯一**看得到
明文的地方，`ApiToken` 型別沒有明文欄位，也不提供取回的方法。稽核摘要同樣不得
包含明文，因為稽核紀錄是給人翻閱的。

`issue_api_token` 是 `BossOnly`，`revoke_api_token` 刻意只是 `HighRisk`：撤銷是往
安全的方向走，Token 外洩時不該還要等老闆到場。

`/mcp` 的 Bearer 明文先以與簽發端相同的 `token_hash()` 算 BLAKE3，再由
`token_by_hash` 查回資料列（`token_by_hash` 刻意不在 SQL 篩掉撤銷或過期的
列，理由見下段）。`OpCtx.actor` 與 `actor_role` 只取該列，請求標頭與
JSON-RPC params 都不能覆寫；成功驗證後呼叫 `touch_api_token_used` 更新
`last_used_at`。撤銷若剛好發生在查詢與更新之間，更新會回 false，本次呼叫仍
回 401。

**Token 會過期，不只能撤銷。** `rotate_days` 有值時，`created_at +
rotate_days` 之後即強制失效（`ApiToken::is_expired`），不必等人手動撤銷。這與清單
畫面上「距離下次輪替還有幾天」的提醒（以 `last_used_at` 估算）是兩件事：一個決定
Token 還能不能用，一個只是提醒該換了。撤銷與過期的原因會
分別寫進稽核摘要（`mcp_auth` 這筆），但**對外的 HTTP 回應一律是同一句 401**，
不讓探測者從回應內容反推出這把 token 是撤銷還是過期。
`list_api_tokens` 同時回傳 `expires_at`／`expired` 與既有的
`rotate_due_on`／`overdue`，兩組數字不要混用。

## 資料庫 schema（PostgreSQL 17）

Migration 放 `backend/migrations/`，以 `sqlx::migrate!()` 在啟動時執行。

```
account(code PK text, name text, kind text, normal_side text,
        is_postable bool, is_active bool, parent_code text null, sort_order int,
        report_group text, cash_flow_section text null)
    kind ∈ asset | liability | equity | revenue | expense
    normal_side ∈ debit | credit

bank_account(id serial PK, name text, account_no text, gl_code text → account,
             sort_order int)

voucher(id uuid PK, no text unique, voucher_date date, memo text,
        status text, source text, created_by text,
        created_at timestamptz, posted_at timestamptz null,
        ai_audit_id bigint null → ai_audit_log,
        reverses uuid null unique → voucher)
    source ∈ manual | ai_draft | import | payroll | invoice | depreciation |
             expense | reversal

voucher_line(id bigserial PK, voucher_id uuid → voucher on delete cascade,
             line_no int, account_code text → account,
             debit numeric(18,2), credit numeric(18,2),
             memo text null, bank_account_id int null → bank_account)

doc_sequence(prefix text PK, period text, next_no int)   -- V2607 → 19

reg_ruleset(id serial PK, domain text, version text, effective_from date,
            effective_to date null, source_note text, payload jsonb,
            checksum text, unique(domain, version))

mcp_tool_grant(tool_name text PK, enabled bool, risk text, note text,
               updated_at timestamptz)

-- 收付模組（0004）
ar_invoice / ap_bill        明細帳。餘額的真相仍是總帳 1131 / 2101，
                            這兩張表只承載明細與到期日
bank_txn                    銀行匯入的流水；核銷是把它對到一張傳票，不產生分錄

-- 稅務與資產模組（0005）
einvoice_track              國稅局配發的字軌與號碼區間（next_no 存表，
                            不用 MAX+1，作廢的號碼也算用掉了）
einvoice                    開立紀錄，含 vat_version 與上傳狀態機
withholding_record          給付事實。**不存稅額**，稅額由 ruleset 現算
fixed_asset                 **不存耐用年數**，只存 category；年數來自 ruleset
depreciation_run            一期一筆（period UNIQUE），存 ruleset_version
depreciation_entry          逐項金額，含當時解析出的 useful_life_years

-- 報表分組、角色權限、安全（0006）
account.report_group        資產負債表／損益表的分段，NOT NULL，以 CHECK 與 kind 綁死
account.cash_flow_section   現金流量表活動別；損益科目必須為 NULL（CHECK 強制）
company_setting             公司與帳務設定。**法規決定的值不得進這張表**
dimension / dimension_value 部門、專案、成本中心、付款條件；department 已以
                            複合外鍵掛到分錄與員工，其他維度尚未掛上
role / permission / role_permission
                            人的權限矩陣。與 mcp_tool_grant 是兩件事：前者管人，
                            後者管 AI。dispatch 每次呼叫即時強制 deny-by-default
approval_rule               費用簽核流程與金額門檻（公司政策，不是法規）
security_policy             控制規則，含 `enforced`，標示有沒有實際執行
api_token                   **不存明文**，只有 BLAKE3 雜湊與前八碼

-- 費用、預算、文件、匯入（0007）
voucher_line.dept_id        分錄的部門維度。以 (id, dimension_code) 複合外鍵
                            綁死只能指向「部門」那一組值
document / document_version 文件身分與版本。**內容不在資料庫**，只有物件鍵、
                            位元組數、MIME 與 BLAKE3；版本不可異動（trigger）
expense_claim / expense_claim_step
                            報支單與每一關的簽核紀錄。CHECK 綁死
                            「已核准 ⇔ 有傳票」
budget_version / budget     **只有預算數，沒有實際數**。實際數一律現查
import_source / import_batch / import_issue
                            `supported` 是欄位不是文案；`payload` 存預覽結果
integration                 對外整合，`live` 標明有沒有實際接上

-- 登入、權限、主檔操作、MCP 憑證與傳票更正（0010／0011／0012／0013／0014）
user_account                登入帳號；可選擇連到 employee，密碼雜湊可為 NULL
user_identity               以 IdP 的 provider + subject 唯一識別；email 只供稽核
user_session                只存 token 雜湊、到期與撤銷狀態，明文不落地
permission.op_names         87 個 Registry 操作各恰好屬於一條 permission
account.is_active           0012 新增；停用阻止新分錄，但保留歷史帳
integration.mcp             0013 更新為 api_token 雜湊驗證與資料列身分綁定
voucher.reverses            0014 新增；沖銷傳票指回原傳票，UNIQUE 防止重複沖銷

ai_audit_log(id bigserial PK, at timestamptz, actor text, actor_role text,
             tool_name text, risk text, prompt text null,
             params jsonb, result_summary text, outcome text,
             voucher_no text null, reverted bool default false)
    outcome ∈ ok | blocked | error
```

必要的不變量，以 SQL 強制（不能只靠 Rust）：

1. `voucher_line`：`debit >= 0 AND credit >= 0 AND NOT (debit > 0 AND credit > 0)`
2. 每張傳票借貸相等：**deferred constraint trigger**，於交易結束時檢查
   `SUM(debit) = SUM(credit)`，且行數 ≥ 2。
3. `status = 'posted'` 的傳票不可 UPDATE／DELETE（trigger 擋下），唯一狀態例外是
   `posted → void`；確實發生但記錯的交易則另開反向分錄。

### 傳票編號

格式 `V{西元年末兩碼}{月份兩碼}-{三碼流水}`，例：2026/07 第 18 張 →
`V2607-018`。以 `doc_sequence` 取號，同一交易內遞增。

### 預載基礎資料

- `account`：提供可直接開始設定的基礎科目表；報表分組與現金流量活動別一併建立。
- `role`、`permission`、`role_permission`、`mcp_tool_grant`：提供預設拒絕的權限
  與 AI 工具開關。
- `security_policy`、`approval_rule`、`hr_flow_step`：提供系統控制與流程定義。
- `dimension`：只建立維度定義；部門、專案、成本中心與付款條件的值由各公司建立。
- `company_setting`：保留設定鍵與標籤，值一律為空字串，等公司自行填寫。
- `integration`、`import_source`、`reg_ruleset`：保留功能狀態、匯入來源與法規投影。

正式安裝不預載銀行帳戶、傳票、應收應付、電子發票、固定資產、預算、報支、
員工、差勤、薪資、API Token、維度值或單據流水起始值。

## laysi-ops：Operation Registry

「所有操作支援 MCP」的保證機制。每個操作只定義一次，registry 同時餵給
REST、MCP 與 TypeScript 產生器。

```rust
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Risk { ReadOnly, NeedsConfirm, HighRisk, BossOnly }
// 對應原型：唯讀 / 需人確認 / 高風險 / 限老闆

impl Risk { pub fn label_zh(&self) -> &'static str; }

pub struct OpCtx { /* pool、actor、actor_role、employee_id、channel、now */ }

#[async_trait]
pub trait Operation: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn risk(&self) -> Risk;
    fn summary(&self) -> &'static str;      // 中文，給 MCP tools/list
    fn input_schema(&self) -> serde_json::Value;   // schemars 產生
    fn output_schema(&self) -> serde_json::Value;
    async fn call(&self, ctx: &OpCtx, input: serde_json::Value)
        -> Result<serde_json::Value, OpError>;
}

pub struct Registry { /* … */ }
impl Registry {
    pub fn new() -> Self;             // 註冊目前全部 87 個操作
    pub fn get(&self, name: &str) -> Option<&dyn Operation>;
    pub fn iter(&self) -> impl Iterator<Item = &dyn Operation>;
    /// 統一入口：工具開關 → 風險 → 角色權限 → 執行 → 稽核。
    pub async fn dispatch(&self, ctx: &OpCtx, name: &str, input: serde_json::Value)
        -> Result<serde_json::Value, OpError>;
}
```

`dispatch` 的責任順序（不可調換）：

1. MCP channel 查 `mcp_tool_grant`：未啟用 → 回 `OpError::ToolDisabled`，並以
   `outcome = 'blocked'` 寫入 `ai_audit_log`。
2. 風險檢查：`BossOnly` 需 `actor_role = "boss"`。
3. 即時查 `role_permission` → `permission.op_names`：操作不在角色集合裡 →
   回 `OpError::NotPermitted`，並以 `outcome = 'blocked'` 寫入稽核。
4. 執行 `call`。
5. 寫 `ai_audit_log`（`ReadOnly` 也要記，對應原型的唯讀稽核列）。

### 核心操作

| name | risk | input | output |
|---|---|---|---|
| `get_dashboard` | ReadOnly | `{}` | `{ stats[], todos[], cash_bars[], ai_recent[] }` |
| `get_trial_balance` | ReadOnly | `{ as_of?: date }` | `{ rows: TrialBalanceRow[], total_debit, total_credit }` |
| `get_cash_position` | ReadOnly | `{}` | `{ accounts: [{name, account_no, balance}], total }` |
| `search_vouchers` | ReadOnly | `{ status?, q?, limit? }` | `{ vouchers: Voucher[] }` |
| `list_ai_audit` | ReadOnly | `{ limit? }` | `{ entries: [...] }` |
| `list_regulations` | ReadOnly | `{ as_of?: date }` | `{ rulesets: [{domain, version, effective_from, effective_to, source_note}] }` |
| `draft_voucher` | NeedsConfirm | `{ date, memo, lines: LineInput[] }` | `{ voucher }` 狀態 `draft` |
| `post_voucher` | HighRisk | `{ no: string }` | `{ voucher }` 狀態 `posted` |
| `void_voucher` | HighRisk | `{ no, reason }` | `{ voucher }` 狀態 `void`；分錄不動，理由進稽核摘要 |
| `reverse_voucher` | HighRisk | `{ no, date }` | `{ voucher, original }`；新傳票狀態 `posted` |

### 傳票更正（0014）

`void_voucher` 與 `reverse_voucher` 都是 `HighRisk`，但語意不可互換：

- 原傳票自始不該存在（重複過帳、交易對象完全錯誤）才作廢。只接受
  `posted → void`，理由必填；分錄保留，試算表與報表只計 `posted`，所以作廢後
  不再計入。
- 交易確實發生、只是金額或科目記錯時用反向沖銷。呼叫者指定更正日期；新傳票
  重用 `insert_voucher` 先以 `pending` 寫完相反分錄、銀行帳戶與部門，再改為
  `posted`。`voucher.reverses` 指回原傳票，唯一限制讓同一張不能重複沖銷。

兩個操作的 MCP grant 都預設關閉，人的權限沿用 `post_voucher` permission：
能把帳正式過帳的人才可更正。兩個 `summary` 都會明說何時應改用另一個操作，
避免把「交易不存在」與「交易記錯」混為一談。

### 報表與設定模組（0006）

| name | risk | input | output 重點 |
|---|---|---|---|
| `get_balance_sheet` | ReadOnly | `{ as_of? }` | `rows[]`、`assets`、`liabilities`、`equity`、`profit`、`difference`、`balanced` |
| `get_income_statement` | ReadOnly | `{ from?, to? }` | `rows[]`、`revenue`、`cogs`、`gross_profit`、`operating_profit`、`profit` |
| `get_cash_flow` | ReadOnly | `{ from?, to? }` | `rows[]`、`opening`、三段合計、`closing`、`ledger_closing`、`cash_accounts[]`、`reconciled` |
| `get_company_settings` | ReadOnly | `{}` | `settings[]`，含 `source: setting \| regulation` |
| `update_company_setting` | BossOnly | `{ code, value }` | `{ setting }`；`editable = false` 的擋下 |
| `list_chart_of_accounts` | ReadOnly | `{ only_used? }` | 科目＋報表分組＋活動別＋餘額 |
| `list_dimensions` | ReadOnly | `{}` | `dimensions[]`、`entry_dimension: department`、`applied_to_entries: true` |
| `list_roles` | ReadOnly | `{}` | `roles[]`、`permissions[]`、`grants[]`、**`enforced: true`** |
| `set_role_permission` | BossOnly | `{ role_code, permission_code, level: full\|limited\|none }` | `{ cell, enforced: true }`；下次呼叫立即生效 |
| `list_approval_rules` | ReadOnly | `{}` | `steps[]`、`enforced: true` |
| `get_security_policy` | ReadOnly | `{}` | `rules[]`，每條帶 `enforced` 與 `source` |
| `list_api_tokens` | ReadOnly | `{ include_revoked? }` | `tokens[]`（**無明文**）、`storage_note`、`wiring_note` |
| `issue_api_token` | BossOnly | `{ name, kind, scope, actor, actor_role, rotate_days?, expiry_note? }` | `{ token, secret }`，`secret` 只顯示這一次 |
| `revoke_api_token` | HighRisk | `{ id }` | `{ token }` |
| `get_system_health` | ReadOnly | `{}` | `migration`、`stats`、`rulesets[]` |

報表三個操作、讀取類全部 `ReadOnly` 並對 AI 開放；`update_company_setting`、
`set_role_permission`、`issue_api_token` 在 `mcp_tool_grant` 中**預設關閉**：能自己
開通自己權限的 AI 等於沒有權限系統。

### 主檔與應收應付建立操作（0012）

| name | risk | input／行為重點 |
|---|---|---|
| `create_account` | NeedsConfirm | `report_group` 與 `cash_flow_section` 都必填；損益科目明確送 `null`。`normal_side` 依 kind 採慣例值，但可覆寫 |
| `update_account` | NeedsConfirm | 完整更新或以 `is_active = false` 停用；不提供刪除，既有分錄仍保留 |
| `create_employee` | NeedsConfirm | 只接受遮罩 `id_no`；三個投保級距照申報事實保存；不建立登入帳號 |
| `update_employee` | NeedsConfirm | 完整更新；`leaving`／`left` 必須有 `left_on`；不修改登入帳號 |
| `create_bank_account` | NeedsConfirm | `gl_code` 必須指向啟用中的可入帳資產科目 |
| `create_dimension_value` | NeedsConfirm | 只在既有 `dimension` 下新增值；不建立維度 |
| `set_approval_rule` | BossOnly | 依 `step_no` upsert 或刪除；`min_amount = null` 代表一律需要，流程至少留一關 |
| `create_ap_bill` | NeedsConfirm | 同一交易寫 `ap_bill` 與待過帳傳票；不會直接付款 |
| `create_ar_invoice` | NeedsConfirm | 同一交易寫 `ar_invoice` 與待過帳傳票；不要求電子發票，供 B2B 月結與國外客戶使用 |

`issue_einvoice` 與 `create_ar_invoice` 共用 store 的應收建立 helper；電子發票只多
字軌取號、稅額與上傳狀態，不再各自維護一份應收／傳票邏輯。科目、員工、銀行、
維度與簽核規則是結構性主檔，MCP grant 預設關閉；AR/AP 是仍需人工過帳的日常
單據，預設開啟。人的權限另由 permission 管理：`manage_masters` 只給老闆與
內部會計；出納不會因為能核銷銀行流水，就取得改科目表的能力。

### 費用與文件模組（0007）

| name | risk | input | output 重點 |
|---|---|---|---|
| `list_expense_claims` | ReadOnly | `{ include_done?, limit? }` | `claims[]`（含 `required_steps` 與每一關的 `reason`）、`flow[]`、`thresholds_source` |
| `submit_expense_claim` | NeedsConfirm | `{ title, amount, spent_on?, dept_code?, suggested_account_code?, … }` | `{ claim }` |
| `approve_expense_claim` | HighRisk | `{ claim_no, account_code? }` | `{ claim, voucher, step_taken, finished }` |
| `reject_expense_claim` | NeedsConfirm | `{ claim_no, note? }` | `{ claim }` |
| `get_budget_status` | ReadOnly | `{ version?, from?, to? }` | `rows[]`、`budget_total`、`actual_total`、`alert_percent`、`actual_source` |
| `get_department_pl` | ReadOnly | `{ from?, to? }` | `rows[]`（含 `unallocated`）、`total_profit`、`statement_profit`、`reconciled` |
| `set_budget` | BossOnly | `{ account_code, amount, version?, dept_code? }` | `{ budget }` |
| `list_documents` | ReadOnly | `{ q?, limit? }` | `documents[]`、`stats`、`bucket`、`bucket_reconciled`、`ocr_note` |
| `upload_document` | NeedsConfirm | `{ file_name, content_base64, kind?, link_type?, link_ref?, doc_no? }` | `{ document, new_version }` |
| `get_document` | ReadOnly | `{ doc_no, version?, include_content? }` | `{ document, verified, checksum_recomputed }` |
| `list_imports` | ReadOnly | `{}` | `sources[]`、`batches[]`、`integrations[]`、`csv_columns` |
| `preview_import` | NeedsConfirm | `{ source_code, file_name, content }` | `{ batch, drafts, can_commit, blocked_note }` |
| `commit_import` | HighRisk | `{ batch_no }` | `{ batch, posted: false }` |
| `rollback_import` | HighRisk | `{ batch_no }` | `{ batch, removed }` |

`approve_expense_claim`、`commit_import`、`set_budget` 在 `mcp_tool_grant` 中
**預設關閉**：自己送自己簽等於沒有簽核流程；一次寫進幾百張傳票，而回滾只救得了
未過帳的；預算是老闆定的，AI 不該改掉自己的比較基準。`rollback_import` 維持開啟，
理由與 `revoke_api_token` 相同。

### 人資模組（0008）

| name | risk | input | output 重點 |
|---|---|---|---|
| `get_hr_dashboard` | ReadOnly | `{}` | 在職人數、待辦與人資／財務銜接狀態 |
| `list_employees` | ReadOnly | `{ q?, include_left? }` | 名冊與特休額度；**不含月薪與投保級距** |
| `list_employee_movements` | ReadOnly | `{ limit? }` | 到離職與異動單、逐關完成狀態 |
| `record_employee_movement` | NeedsConfirm | `{ emp_no, kind, effective_on?, … }` | `{ movement }` |
| `complete_movement_step` | NeedsConfirm | `{ movement_no, step_no?, note? }` | `{ movement, step_taken, finished }` |
| `get_attendance` | ReadOnly | `{ month? }` | 逐人出勤、請假、遲到與來源系統帶入的加班費 |
| `list_leave_requests` | ReadOnly | `{ include_done?, limit? }` | 請假單與依 `annual_leave` ruleset 現算的逐人額度 |
| `decide_leave_request` | NeedsConfirm | `{ leave_no, approve, note? }` | `{ request, action }` |
| `get_payroll_summary` | ReadOnly | `{ period? }` | 薪資彙總與總帳對帳；不含個人金額 |
| `list_payslips` | BossOnly | `{ period? }` | 逐人薪資與保費重算對帳 |
| `run_payroll` | HighRisk | `{ period?, pay_on?, bank_account_id?, note? }` | 薪資單與待過帳傳票 |
| `get_insurance_summary` | ReadOnly | `{ as_of? }` | 逐人投保級距與三項費額 |
| `compute_insurance` | ReadOnly | `{ insured_salary, health_insured_salary?, pension_wage?, as_of? }` | 單一級距試算與三份 ruleset |
| `get_hr_compliance` | ReadOnly | `{ as_of? }` | `checks[]`；逐項帶 `checked`、`passed`、ruleset 與差異 |
| `list_withholding_statements` | BossOnly | `{ year? }` | 逐給付對象年度所得；`table` 稅額缺值保持 `null` |

法遵檢核的 `checked_all_passed` **只涵蓋實際算過的項目**。完整投保薪資級距與
加班費倍數尚未完整收錄，會以 `checked = false`、`passed = null` 與
`not_checked_reason` 回傳；`coverage_complete` 因此是 `false`，畫面不得把它
改寫成「全部法規皆符合」。

年度扣繳憑單以 `withholding_record` 的 `payee`／`payee_id` 分組。`basis = rate`
依給付日 ruleset 計算，`basis = table` 只採來源系統帶入的
`withheld_amount`；來源沒帶就保留 `null`。來源若只有多人合併總額，回應會標成
`person_specific = false`，不自行拆出一份不存在的逐人資料。薪資模組的
`payroll_year_totals` 另列為 `employee_income`，並與這些 `table` 紀錄對帳；
兩邊若不同就回報差額，不用其中一邊覆蓋另一邊。

0008 的十五個 `mcp_tool_grant` 名稱必須與上述五個人資操作模組在 Registry
裡的名稱集合完全相等；`laysi-ops` 以 `include_str!` 直接讀 migration 的測試
守住這條契約。完成本模組後 Registry 共 **71 個操作**。

### 員工自助模組（0009）

| name | risk | input | output 重點 |
|---|---|---|---|
| `get_my_home` | ReadOnly | `{}` | 本人基本資料、年資、本月出勤、特休餘額、送出的單走到哪 |
| `list_my_payslips` | ReadOnly | `{}` | **只有本人的**薪資單 |
| `list_my_leave_requests` | ReadOnly | `{}` | 本人請假單與特休額度／已休／剩餘 |
| `request_leave` | NeedsConfirm | `{ kind, from_on, to_on, days, reason? }` | `{ request }`，編號 `LV{民國年月}-{三碼}` |
| `list_my_expense_claims` | ReadOnly | `{}` | 本人送出的報支與逐關進度 |

**這個模組的授權不靠 `Risk`，靠輸入形狀。** `list_payslips` 是 `BossOnly`，
但員工本人必須看得到自己的薪資單，而他不是老闆。作法是：**自助操作一律不接受
任何指定對象的參數**，對象由已驗證 session 的 `ctx.employee_id` 決定。沒有欄位可
以填，就沒有辦法填別人的名字；這比在查詢裡加過濾條件更難寫錯。

`ops/selfservice.rs` 的 `no_self_service_input_can_select_another_employee`
測試把五個操作的 `Input` 屬性集合**逐字釘死**（`request_leave` 只有
`kind`／`from_on`／`to_on`／`days`／`reason`，其餘四個是空集合），另加一份
`employee_id`／`emp_no`／`name`／`actor`／`claimant`／`user_id` 的黑名單。
**加任何一個欄位都會讓測試紅**，因為這是本模組唯一的授權保證。

`ctx.employee_id` 直接取自已驗證 session 所屬的 `user_account.employee_id`，
再以主鍵查 `employee`；不使用姓名比對，也沒有同名時任選一筆的問題。未連結
員工資料的外部會計師帳號會回 `BadInput`，明確說明自助功能需要員工主檔，
不回空清單；空清單容易被讀成「我沒有薪資單」。

0009 的五個 `mcp_tool_grant` **一律 `enabled = false`**：MCP token 沒有
`user_account.employee_id` 連結，而且它代表的系統身分與坐在畫面前的人是兩回事，
預設對 AI 開放沒有道理。
上節的 migration parity 測試同時涵蓋 0008 與 0009。完成本模組後 Registry
共 **76 個操作**；0012 再加入 9 個主檔與應收應付建立操作，0014 再加入作廢與
反向沖銷，目前共 **87 個**。

`Voucher` 的 JSON 形狀：

```json
{ "no": "V2607-018", "date": "2026-07-24", "memo": "支付本月電費",
  "status": "pending", "reverses": null, "reversed_by": null, "amount": "8420.00",
  "accounts_label": "6118 水電費 / 1102 銀行存款",
  "lines": [ { "account_code": "6118", "account_name": "水電費",
               "debit": "8420.00", "credit": "0.00", "memo": null } ] }
```

`get_dashboard` 的欄位對應原型「總覽」畫面，全部由總帳推導：

- `stats`：銀行＋現金（1101+1102 餘額）、應收帳款（1131 餘額）、
  應付帳款（2101 餘額）、本月損益（本月收益−費用）。
  形狀 `{ kicker, value, note }`，`value` 為金額字串。
- `todos`：`{ count, label, due, screen }`，`screen` 是前端路由 key。
- `cash_bars`：近 6 個月現金餘額 `{ month, value }`。
- `ai_recent`：`ai_audit_log` 最近 3 筆 `{ what, time, who }`。

## REST 契約（laysi-server）

| 方法 | 路徑 | 說明 |
|---|---|---|
| GET | `/healthz` | `{ status, brand: {ok, fingerprint, files}, db: {migration}, rustfs: {ok} }` |
| — | — | 前端也讀這一條（Caddy 與 vite 都要 proxy）：品牌指紋與 RustFS 狀態只有伺服器行程知道，`get_system_health` 從資料庫查不到 |
| POST | `/v1/auth/login` | 帳密登入並設定 session cookie；失敗訊息不區分帳號或密碼 |
| POST | `/v1/auth/logout` | 撤銷目前 session 並清除 cookie |
| GET | `/v1/auth/me` | 目前登入帳號；未登入回 401 |
| POST | `/v1/auth/change-password` | 驗證目前密碼後更新 Argon2id 雜湊並清除 `must_change_password` |
| GET | `/v1/auth/oauth/start` | 啟用時建立 PKCE 流程並以 302 前往 discovery 的 authorization endpoint |
| GET | `/v1/auth/oauth/callback` | 後端交換授權碼、驗證 id_token、建立 session，再以 302 回 `/` |
| GET | `/v1/meta` | 公司、會計期間、身分、版本、授權、品牌出處字串 |
| GET | `/v1/brand/mark.svg?h=28` | 字標，`Content-Type: image/svg+xml` |
| GET | `/v1/brand/monogram.svg?h=64` | 組合標記 |
| GET | `/v1/ops` | registry 清單：`[{name, risk, summary, enabled, input_schema}]` |
| POST | `/v1/ops/{name}` | body 即 input；回 `{ ok: true, data }` 或 `{ ok: false, error: {code, message} }` |
| POST | `/mcp` | MCP streamable HTTP，Bearer token |

`/healthz` 與 `/v1/brand/*.svg` 是公開端點。`/v1/meta`、`/v1/ops*` 必須有
有效 session；`/mcp` 必須有自己的 Bearer 憑證。認證路由是操作註冊表唯一例外：
登入若經過 dispatch，就會先被角色權限步驟要求一個尚未取得的身分，形成循環。

`/v1/meta` 回傳：

```json
{ "company": "", "tax_id": "", "setup_required": true,
  "period": "2026 / 07", "period_state": "未結帳",
  "actor": "目前使用者", "actor_role": "acct", "employee_id": 1,
  "oauth": { "enabled": true, "provider": "https://accounts.example.com" },
  "version": "0.1.0", "licence": "BSL",
  "attribution": "Laysi One · Business Source License 1.1",
  "brand_build": "<16 碼指紋>" }
```

`setup_required` 只看 `company_setting.company_name`：空字串（包含只有空白）代表
首次設定尚未完成，回 `true`；填入公司名後回 `false`。這只是帶前端進設定精靈的
bootstrap 訊號，不會自行建立任何公司資料或示範資料。

### MCP

`POST /mcp`，JSON-RPC 2.0，實作 `initialize`、`tools/list`、`tools/call`。
protocol version `2025-06-18`。`tools/list` 的 `inputSchema` 直接取
`Operation::input_schema()`。`tools/call` 走 `Registry::dispatch`，回傳
`{ content: [{ type: "text", text: <JSON 字串> }] }`；被擋下時
`isError: true`。認證：`Authorization: Bearer <api_token 明文>`；伺服器只保存
BLAKE3 雜湊，且 Token 必須是未撤銷、未過期的 `kind = mcp` 資料列。認證先於
JSON-RPC body 解析，未通過一律回 401（撤銷與過期不對外分辨，見上一節）。

對外開放之後，認證與 JSON-RPC 之間還有兩層防護（`crates/laysi-server/src/mcp.rs`）：

- **Origin 驗證**：streamable HTTP 規格要求防 DNS rebinding。有帶
  `Origin` 就要在 `MCP_ALLOWED_ORIGINS`（逗號分隔，預設空）內，沒帶則放行：正常
  的 AI connector 不會帶這個標頭，瀏覽器帶了就必須在白名單裡。
- **per-token 速率限制**：`MCP_RATE_LIMIT_PER_MINUTE`（預設 60，0＝不限制）
  控制的固定視窗計數器，行程內用 `HashMap` 記，**不依賴 redis**，這個容器本來就是
  單機部署。被擋下時回自訂的 JSON-RPC 錯誤碼並寫稽核。

### 部署角色：`LAYSI_SERVE`

`laysi-server` 由環境變數 `LAYSI_SERVE` 決定要掛哪些路由，三選一：

| 值 | 掛什麼 | 用途 |
|---|---|---|
| `full`（預設） | `/healthz`、`/v1/*`、`/mcp` | 單一程序的簡單部署，維持原本行為 |
| `app` | `/healthz`、`/v1/*`（不含 `/mcp`） | compose 的 `api` 服務 |
| `mcp` | `/healthz`、`/mcp`（不含任何 `/v1/*`） | compose 的 `mcp` 服務 |

無法辨識的值在啟動時就報錯退出，不會默默當成 `full`。`--healthcheck` 三種
角色下都能用，因為 `/healthz` 一律掛上。

**MCP 為什麼要獨立程序**：把 `/mcp` 拆成獨立容器並發布獨立的埠
（`MCP_PORT`，預設 `8089`），是這套系統唯一對外暴露的後端服務；`api` 不發布任何
埠，只有 `web` 的 Caddy 連得到它。這樣一來，對外可及的那個程序
**結構上就沒有** REST、沒有 session cookie、沒有登入端點：即使 `/mcp` 的
認證或某個操作出了漏洞，攻擊者能碰到的介面本來就只有 Registry 那一組工具，
不會意外連到 `/v1/auth/login` 或其他 REST 端點。攻擊面的縮小來自「這個程序根本沒
掛那些路由」，不是靠反向代理在網路層擋掉部分路徑；後者只要設定錯一次就整個失效，
少掛路由則從編譯與啟動就不可能出錯。

`mcp` 服務與 `api` 服務共用同一個映像檔（`backend/Dockerfile`）、同一個
postgres，只有 `LAYSI_SERVE` 與對外的埠不同。是否要給 `mcp` 服務 RustFS
金鑰是刻意的決定：文件模組的 `upload_document`／`get_document` 也是 Registry 操作，
因此也是 MCP 工具；不給金鑰不會讓這兩個工具從清單消失，只會讓它們被呼叫時回
「尚未接上物件儲存」，等於文件模組對 AI 是壞的而不是關閉的。真正管得住 AI 能不能用這兩個工具的是 `mcp_tool_grant` 與角色權限，
兩者都先於實際存取物件儲存執行；因此目前 compose 選擇兩個服務都給金鑰。

### 必要測試

`registry parity`：`/v1/ops` 的名稱集合 == MCP `tools/list` 的名稱集合 ==
`Registry::iter()`，三者數量相等；少接一面測試就要紅。

## 前端契約（web/）

Vite + React + TypeScript。設計語彙來自 **Classical** 設計系統：

- 底色 `#f3f2f2`、表面 `#eae9e9`、卡片底 `#f8f4f4`、文字 `#201f1d`
- 單一重點色 `#b68235`，深階 `#7d5411`、`#a06f24`、`#5a3b0a`
- 中性階：`#605d5d`（次要文字）、`#9b9797`、`#444141`、`#bab6b6`
- 標題 `Cormorant Garamond` 600／內文 `Lora`，Google Fonts
- **以線條與框線構圖，不用色塊填滿**；按鈕是外框式；圓角 4px
- 數字一律 `font-variant-numeric: tabular-nums`
- `:focus-visible { outline: 2px solid #b68235; outline-offset: 2px }`

`web/src/styles/classical.css` 必須是設計系統 `styles.css` 的移植：把上述
色階、字體、`--space-*`、`--radius-*`、`--shadow-*` 定義成 CSS 變數，元件
**只能引用變數**，不得寫死 hex／字體名／px。

Logo 一律 `<img src="/v1/brand/mark.svg">`，**前端不得自帶任何 logo 檔案或
以文字重繪品牌名**。頁尾顯示 `/v1/meta` 回傳的 `attribution`。

**前端同樣不得自帶任何法規數字。** 稅率、級距、耐用年數一律由後端從 ruleset
帶下來，這條規則與「不得自帶 logo」同級。畫面上要即時試算稅額時，用後端
回傳的費率，不要在 TS 裡寫 `0.05`。金額運算用 `client.ts` 的
`multiplyToDollars` / `addDecimals`（BigInt），不經過 `number`。
