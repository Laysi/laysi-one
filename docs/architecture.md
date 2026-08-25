# Laysi One — 架構與整合契約

這份文件是各層之間的**契約**。跨 crate、跨前後端的型別與端點以此為準，實作時
不得各自發明。改動契約要先改這份文件。

## 產品背景

Laysi One 是面向台灣中小企業的財務＋人資正式產品。安裝時只建立系統運作所需的
基礎科目、權限、流程與法規設定，公司資料由使用者自行建立。

四個非功能性需求：

1. **法規更新空間** — 勞保級距、健保費率、勞退、營業稅、基本工資每年會變，
   一律不得寫死在程式碼裡。
2. **所有操作支援 MCP** — MCP 不是外掛，而是操作層本身。
3. **BSL 防盜賣** — 字標由 Rust 產生、註解 prompt 阻卻、hash 完整性硬性檢查。
4. **Docker Compose 自架**。

### 不做加值中心（0031）

開立統一發票要向財政部申請字軌、接 MIG 介面、承擔上傳成功與否的責任，那是
加值服務中心的業務。這套系統不做，未來也不做，所以 0031 把 `einvoice`、
`einvoice_track` 兩張表與 `list_einvoices`／`issue_einvoice`／
`retry_einvoice_upload` 三個操作整組移除。留著一個永遠停在 `queued` 的上傳
狀態機，會讓使用者以為那條線只是還沒接上。

**移除之後，應收單是系統裡唯一記錄銷項稅額的地方**，所以 `create_ar_invoice`
必須自己把稅拆到 2211；同理 `create_ap_bill` 拆進項稅額到 1284。401 申報本來
就從總帳現算，不受影響。公司仍可在加值中心開票，再把那張票的金額登進應收單。

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
    #[serde(default)] pub dept_code: Option<String>,
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
| `overtime` | `monthly_hours=240` `monthly_max_hours=46`、`day_types` 陣列：`{code, label, max_hours, bands[{up_to_hours?, multiplier}]}` 依勞基法第 24 條 |

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

- **存欄位**：例如 `depreciation_run.ruleset_version`、
  `depreciation_entry.useful_life_years`
- **寫進稽核摘要**：例如「建立應收 AR-2607-030 · 含稅 47,250（未稅 45,000 ＋
  營業稅 2,250，依 vat 2026.1）」
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
3. **現金流量表的期末現金 ＝ 現金科目餘額 ＝ 各資金帳戶合計**。
   三條獨立路徑，`reconciled` 同時比對後兩者。第三條的合計是
   `cash_position_as_of` 加上 `unattributed_cash`——見下一節。

現金流量表採**間接法**，理由不是實作方便，而是它是複式簿記的代數恆等式
（`Σ全部科目(借−貸)=0 ⇒ Δ現金 = 本期損益 + Σ非現金科目(貸−借)`）。每個科目只
屬於一種活動，所以三段加總必然等於現金變動，不需要任何分攤規則。直接法要決定
一筆現金如何分攤到多個對方科目，那個規則只能靠猜，與「不猜耐用年數」同一條原則。

**已知限制**：不動現金、卻跨活動別的分錄（例如期初把固定資產淨額還原成成本與
累計折舊）會同時出現在兩段裡互相抵銷。合計永遠正確，但那兩段的組成會被放大。
畫面上要說明這件事。

### 報表要說出它漏掉了什麼，而不是只說「還沒過帳」

三張報表都只計已過帳分錄，所以每一張都回一份 `unposted`。一筆分錄不在表上有**兩
個**不同的理由，因此那份說明有兩個數字：

- `count`／`amount`：還沒有人過帳。
- `after_period`：**已經過帳，但日期在報表截止日之後**。

第二個是提列整月折舊之後立刻打開今天的資產負債表會遇到的情況——折舊傳票日期是
月底，報表結到今天，帳完全正確而那 6,250 就是不在表上。原本畫面只數未過帳的
傳票，於是那一筆在每一張能回答它的畫面上都無人認領，而固定資產頁同時寫著「累計
折舊 6,250 就是 1602 的餘額」，兩頁對同一筆錢說了不同的話。截止日是資產負債表的
`as_of`、另外兩張的 `to`。

### 資金帳戶是分錄上的一欄，不是另一套餘額

`cash_position` 把已過帳的 `voucher_line` 依 `bank_account_id` 分組加總——它讀的
是總帳，不是銀行對帳單。但只有 `record_receipt`、`create_payment_batch` 與
`run_payroll` 會寫那一欄，人工建立的傳票一律留空，於是「借 1102 五十萬」過帳之後
資產負債表是 500,000 而銀行卡片是 0，第三條不變量當場宣告對不上——**公司第一天
照系統安排的流程走完，終點是一張紅字的報表**。

兩件事一起修：

1. `insert_voucher` 在寫每一行時，若沒有指定資金帳戶而該科目底下**恰好只有一個**
   `bank_account`，就掛上去（`sole_bank_account`）。只有一個對應時那是讀得出來的
   事實；有多個時問題是真的，該行留空。
2. `unattributed_cash` 另外算出「掛在現金科目、但沒有指定資金帳戶」的金額（零用金
   1101 本來就沒有帳戶列）。`get_cash_position` 與現金流量表都把它列為一筆，合計因此
   **恆等於總帳的現金餘額**，`reconciled` 不再被它拉紅。它不是差額，是「這筆錢還沒說
   放在哪個帳戶」。

## 預算、報支與匯入：存計畫與憑證，不存實際數

`budget` 只有預算金額，**沒有任何一欄實際數**；執行率的分子每次現查
`voucher_line`。部門損益同理，而且一定有一列 `code = null` 的「未指定」：把沒掛
部門的分錄藏起來，部門合計就不等於損益表。`get_department_pl` 因此
回報 `statement_profit` 與 `difference`，由畫面對帳。

費用簽核的關卡由 `approval_rule.min_amount` 與單據金額比對算出來，**`ops/expenses.rs`
裡沒有任何一個金額**。提醒門檻在 `company_setting.budget_alert_percent`
（公司政策可以進設定表，法規不行，差別在生效日，見上一節），**留空就是沒有提醒**：
種下來的值是空字串，把它當成格式錯誤會讓全新安裝的預算頁只剩一行紅字，而那個設定
又只有系統管理員改得動。`alert_percent()` 因此回 `Option<Money>`，`alert_percent`
在 JSON 上是 `null`。

**建得了版本就要編得了預算。** `set_budget` 原本只有既有列上的 ✎ 一個入口，而新版本
的列是空的：畫面自己寫「建立一版之後，逐科目填入年度金額」，卻沒有一個地方可以填。
「＋ 編列科目預算」與 ✎ 走同一個操作（同一個科目再選一次就是修改），科目下拉只列
可過帳的收益與費用科目——執行率的分母只有這兩類講得通。

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

**沒有 OCR**（0003）。`ocr_status` 原本由上傳當下的 MIME 型別推出 `pending` 或
`not_applicable`，沒有任何判斷讀它，而畫面上那張統計卡與逐列標籤在每一套安裝上
都印同一件事。要接引擎時新增欄位，不是留一個永遠停在 `pending` 的欄位等它。

### 身分證字號完整保存（0032）

原本 `employee.id_no` 只收遮罩後的值，理由是「系統沒有功能需要完整字號」。
那個前提在人資模組上線後就不成立了：勞保投保申報與扣繳憑單（`payee_id`）都要
完整字號。擋著的結果不是資料不存在，而是人資另外維護一份 Excel——個資散到系統
外面，讀了也沒有紀錄。

因此 0032 起完整保存，寫入時驗格式與檢查碼（一個英文字母加九碼數字），還沒收到
可以留空。**遮罩是顯示層的事**：名冊預設顯示 `A12*****89`，頁面上一顆切換鈕整批
顯示或整批遮蔽。稽核紀錄則由 `redact_for_audit` 的 `REDACTED_FIELDS` 依欄位名把
`id_no` 換成「[已遮蔽]」——長度規則只擋超過 512 位元組的值，十個字元的字號永遠
碰不到它，而同一批參數裡的 `emp_no` 已經足以回答那次呼叫動的是誰。

### 員工的銀行帳戶不只一個（0049）

`employee.bank_account` 是一個自由文字欄，於是一個人只有一個帳戶，而那個帳戶
是誰輸入的什麼字。兩件事都擋路：薪轉戶與收退款的帳戶常常不是同一個，而一欄
文字回答不了「薪水匯到哪一個」——多打一個帳戶進去，那一欄就成了兩個帳號並排
的字串，沒有任何程式讀得出其中一個。

`employee_bank_account` 一人多筆，銀行名稱與帳號分成兩欄：自助頁只印銀行與
尾碼，欄位分開就不必去猜一個字串在哪裡結束（原本的遮罩函式從第一個數字切開，
「第一銀行」這種名字會被切在中間）。`is_payroll` 標出薪轉的那一筆，以
`WHERE is_payroll` 的 partial UNIQUE 索引限制至多一筆——那是發薪當天唯一的
去向，兩筆等於沒有答案。它是索引不是 `employee` 上的一欄，所以指不到別人的
帳戶，也不會在帳戶被刪掉之後留一個指向空處的欄位。

`bank_accounts` 是整份清單一次取代，不帶就不動——與四個金額同一族的三態，理由
也相同：抽屜握有的就是整份清單，而逐筆增刪要另外三個操作與三份權限。至多一筆
可標為薪轉，一筆都沒標時以第一筆為準（單一帳戶的呼叫端不必重複說一次清單已經
說過的事）。

`account_no` 加進 `REDACTED_FIELDS`：它與 `id_no` 同一類，長度規則碰不到，而
稽核紀錄要回答的是哪一次呼叫動了誰，`emp_no` 已經在同一批參數裡。

0049 把舊欄位的值搬過來時只能盡力拆：第一個數字之前是銀行、之後是帳號。整串
沒有數字的原樣留在帳號欄，於是它會出現在名冊上等人更正，而不是安靜消失。

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

OIDC 是選用入口，**設定在畫面上而不在環境變數裡**（0047）：安全與營運頁的「外部
登入」由系統管理員填供應商、用戶端 ID 與密鑰、轉址網址、允許的網域，以及網域內首次
登入要不要自動建帳號。理由是拿得到 IdP 用戶端資料的就是裝機的人，而要他為了開啟自己
的登入方式去改容器環境變數再重啟，等於把這件事鎖在他構不到的地方。未啟用時兩個 OAuth
端點都回 404。啟用時先用 issuer discovery 取得 authorization、token 與 JWKS，
不綁定 Google 或 Entra ID 的固定網址。流程採 Authorization Code + PKCE，
state、nonce 與 code verifier 放在 API 行程內有 10 分鐘 TTL 的一次性 map；
callback 由後端交換授權碼並以 `openidconnect` 驗證 id_token、簽章、issuer、
audience 與 nonce，授權碼與 token 都不進瀏覽器 JavaScript。授權請求帶
`email`、`profile` 兩個 scope 與 `prompt=select_account`。

Google Cloud Identity 與 Workspace 走的就是這條路：issuer 是
`https://accounts.google.com`，OAuth client 選「Web application」、同意畫面選
Internal，Google 那側就只提供組織帳號。Google 的 discovery 沒有
`end_session_endpoint`，所以登出只結束本站 session，Google 那邊仍登著。

### 設定怎麼生效

`oidc_setting` 是單列表，`client_secret` 原樣保存（token endpoint 需要它），
**任何操作都不回傳它**，稽核紀錄依欄位名遮蔽（`REDACTED_FIELDS`）。
`get_oidc_setting` 回的是 `has_client_secret` 布林值。

伺服器把建好的 client 快取在 `AppState.oidc`（`RwLock<OidcCache>`），指紋是那一列的
`updated_at`。存檔會推進 `updated_at`，下一個請求就重建一次，**不必重新啟動**。
discovery 是網路呼叫，所以只在指紋變動時付這個成本。

建立成敗寫回同一列的 `verified_at`／`verify_error`：discovery 屬於伺服器層，
而按下儲存的人需要知道設定能不能用，設定畫面自己跑不了 discovery。寫回時比對舊值，
一樣就不寫，所以它永遠不會碰 `updated_at`、不會自己追自己的尾巴。
`call_op` 在 `update_oidc_setting` 成功後立刻重建一次，畫面隨後重讀就看得到結果。

### 網域邊界：允許的網域

設了就要求 id_token 的 `hd` claim 與它相符（不分大小寫，前面的 `@` 會去掉），
**claim 不存在一律拒絕**。判準是 `hd` 而不是 email 網域：受目錄管理的 Google 帳號
才帶 `hd`，而個人帳號可以持有一個已驗證的公司網域 email——它通得過看字串的檢查，
但人離職時公司關不掉它。`hd` 不是標準 claim，所以 `oidc.rs` 自訂
`HostedDomainClaims` 當 `AdditionalClaims`，值仍由 `openidconnect` 在驗完簽章後
交出來，這個模組不自己解 JWT。授權請求也帶一個 `hd` 參數，那只是收窄 Google 的
帳號選單，安全性由回程那次比對負責。

### 自動建帳號：首次登入的角色

留空時維持原本的規則：只認管理員預先連結的 `(issuer, sub)`，查不到就回「本帳號
尚未連結至任何使用者」，不建 `user_account`。填了角色代碼，網域內的首次登入就自己
建帳號。角色代碼查不到 `role` 表、或沒有填網域，設定存不進去（`update_oidc_setting`
擋下），就算繞過操作直接改資料庫，client 也建不起來並把理由寫進 `verify_error`，
而不是等到有人登入才發現。

解析順序是三步，每一步的鑰匙都不同：

1. `user_identity(provider, sub)` — 登入的唯一鑰匙，永遠先查。
2. `user_account.email`（不分大小寫）— **只在自動建帳號開啟時**用一次，用途是認出
   管理員已經替同一個人建好的帳號，讓首次登入接上去而不是在旁邊多開一個。比對不限
   啟用中的帳號：停用的帳號必須被找到並擋下，不是被一個全新的帳號取代。對到多筆就
   拒絕，請系統管理員先整理。
3. 都沒有才建帳號。同一個交易寫員工主檔、`user_account` 與 `user_identity`；員工
   編號在交易內用 advisory lock 配號，兩個人同時首次登入不會撞號。

自動建的帳號 `password_hash` 是 NULL（沒有人選過密碼，發一組等於多一條沒人在拿的
路），`created_by` 記「外部登入」。登入名稱就是那個 email 本身——本站不再有獨立於
email 的登入名稱概念（見下方「登入名稱＝email」），所以第 3 步沒有名稱可推導，只需
驗證格式。格式不合法、或那個位址已被別的帳號用掉，就拒絕並請系統管理員建帳號——
帳號建好後，下一次登入會由第 2 步自己接上。

第 2 步的代價要說清楚：`user_account.email` 從此是安全相關欄位，改得動它的人就決定得了
誰的 Google 帳號連到哪個本站帳號。改它的 `update_user_account` 是 `AdminOnly`，
而系統管理員本來就建得出任何角色的帳號，所以這不是新的權限，但它是新的責任。

記憶體 map 適合目前單一 API 行程：重啟只會要求重新登入，也不留永久敏感殘跡；未來若
水平擴充多個 replica，必須換成共用短期儲存。

### 登入名稱＝email

`create_user_account` 與 OIDC 自動建帳號兩個入口，現在都只收一個 email 欄位；
`user_account.username` 在建立當下就是那個 email（小寫化），不再是另外打一個獨立
的登入名稱。兩處各自驗證格式（非空、含 `@`、網域部分含 `.`、長度上限 254），不共用
函式——`setup.rs` 與 `laysi-ops` 之間本來就各自一份的既有作法，兩個 crate 沒有自然
的共用模組可放。

`username` 建立後仍然不可變（既有規則沒變），但 `email` 欄位本身在那之後可以由
`update_user_account` 獨立編輯——聯絡方式會換，登入名稱不必跟著換，兩者從此可能
不同，畫面上帳號管理的「登入信箱」（固定唯讀）與「電子郵件」（選填、可編輯）因此
是兩個欄位。OIDC 依 email 尋找既有帳號的第 2 步（見上）也因此仍然必要：那步找的是
「聯絡信箱等於這個地址」的帳號，不是「登入名稱等於這個地址」，兩者在帳號建立後可
能已經分開。

`laysi-admin create-user --username` 這支主機 CLI 不受影響，仍收任意字串：它是離線
時的逃生門，不是這裡要簡化的網頁流程。

### 系統管理員帳號是安裝本身，不是一個人

網頁初始化（`POST /v1/auth/bootstrap`）是這條規則唯一的例外，而且是往反方向的例外：
它**完全不收** email。登入名稱固定 `admin`（`laysi_ops::ADMIN_USERNAME`）、顯示名稱
固定「系統管理員」、`user_account.email` 留空、`employee_id` 為 NULL，呼叫端只送一個
密碼。

空的 email 不是省略，是這個設計的支點。`resolve_account` 的第 2 步用
`users_by_email` 認人並**沿用該帳號既有的角色**，所以只要系統管理員帳號上有一個位址，
持有那個位址的 Google 帳號首次登入就會連上它、拿到 `admin`。`users_by_email` 本來就
以 `email <> ''` 排除空值，於是「不給它位址」同時關掉了這條路；`update_user_account`
另外拒絕替 `admin` 設 email，避免事後有人把它補回去。

其餘兩個後果同樣重要：

- 裝機的人離職時不會連帶帶走這個帳號。用私人或個人公司信箱當第一個登入名稱，等於
  讓破窗用的鑰匙掛在一個會被 IdP 停用的身分上。
- `password_login_disabled` 的系統管理員例外從此指著一個**本來就沒有 SSO 路徑**的
  帳號。在此之前那顆例外是多開的第二道門（同一個人既能 SSO 也能打密碼）；現在它是
  那個帳號唯一的門，而其他人一道也沒有。

第二個帳號起才是人：首次設定精靈第五步用 `create_user_account` 建負責人，走的是
email 當登入名稱的正常路徑。`create_user_account` 仍然建得出其他 `admin` 角色的帳號
給真人用（那些帳號有 email、連得了 SSO），「最後一個啟用中的系統管理員」的既有保護
因此不必改成「唯一那一列」。

前端的對應約束：登入頁的帳號欄是 `type="text"`、`autoComplete="username"`。
`type="email"` 會讓瀏覽器在送出前擋下 `admin` 這個值，而那正是密碼登入被關掉時唯一
能用的帳號。按下「系統管理員改用密碼登入」時欄位預填 `admin`、標籤換成「登入名稱」。

### 沒有密碼的帳號：`create_user_account` 的 `external_login_only`

負責人不必有密碼。`create_user_account` 帶上 `external_login_only` 時，帳號的
`password_hash` 是 NULL、`must_change_password` 為 false，回應的 `temporary_password`
是 `null`，這個人只能從 IdP 進來。

三個前提在寫入任何一列之前就檢查，缺一即拒絕：

1. 外部登入已啟用（`oidc_setting.enabled`）；
2. 設為自動建立帳號（`provision_role` 非空）；
3. 地址的網域等於 `allowed_domain`。

第 2 點才是關鍵，而且不直覺：`resolve_account` 的第 2 步——依 email 找既有帳號並把
外部身分連上去——寫在 `let Some(provision) = oidc.provision.as_ref() else { ... }`
之後，**只有自動建帳號開啟時才會執行**。關掉它，這個預先建好的帳號永遠不會被首次
登入認出來，而它沒有密碼；`reset_user_password` 又拒絕 `password_hash IS NULL`，
所以那是一個誰也打不開、事後也補不回來的帳號。第 3 點同理：`hd` 對不上允許的網域，
登入在更早的地方就被擋下。

精靈把這三個前提畫成一個唯讀的「登入方式」欄位，跟著 email 即時變動：落在網域內顯示
供應商名稱，否則顯示「密碼」。判斷寫在畫面上，不寫成一句要讀者自己套用的規則。

對稱的第四道檢查在另一個方向：`password_login_disabled` 開著時，
`create_user_account` 拒絕建立**沒有** `external_login_only` 的非 `admin` 帳號。那樣的
帳號只會拿到一組登入端永遠會擋下的密碼。`admin` 角色是例外，與 `auth::login` 的例外
同一個理由。此處讀的是 `oidc_setting` 的欄位本身，不是 `oidc::current()`——建立帳號
問的是「這個安裝打算怎麼登入」，不是「此刻 IdP 通不通」；IdP 壞掉時的逃生門是建一個
`admin` 帳號，那條路沒有被擋。

精靈在這個狀態下把「登入方式」寫成「密碼（已停用）」、欄位轉成強調色並把建立鈕關掉，
同一個判斷因此在按下去之前就看得見。停用的開關本身也放在精靈第五步，而不是留到事後的
安全與營運頁：它與第六步是同一個決定，分開問就會出現「關掉密碼登入，然後發一組密碼給
負責人」這種走得完、但結果沒有人登得進去的路徑。

帳號管理（`screens/Accounts.tsx`）的建立抽屜用的是同一個欄位與同一組判斷，因為精靈之後
建的會計、人資走的是同一條路。它另外多一件事：免密碼的帳號沒有初始密碼卡可以顯示，
所以改發一則 toast——沒有東西要交付不等於什麼都不必說，靜靜關掉抽屜與「按下去看起來
成功、其實什麼都沒發生」是同一個毛病。`get_oidc_setting` 是 `AdminOnly`，與這個畫面
本身的權限相同，讀不到時就退回只有密碼的行為。

### 外部登入會清掉 `must_change_password`

系統管理員用 `create_user_account` 建的帳號帶著一組產生的密碼與這個旗標。從 IdP 進來
的人沒拿過那組密碼，而變更密碼表單會先要求「目前的密碼」，於是旗標會把一次成功的
登入擋在自己的畫面外。`oauth_callback` 因此在發 session 之前清掉旗標。

只清旗標，不動 `password_hash`：那組密碼仍在，本人之後真想改用密碼登入時
`reset_user_password` 還叫得動。清掉雜湊會讓帳號變成只有外部身分進得來，而
`unlink_user_identity` 拒絕沒有密碼的帳號，等於把一個可逆的狀態換成不可逆的。

### 帳號密碼登入可關閉：`password_login_disabled`

`oidc_setting` 多一欄 `password_login_disabled`（0048）。開啟後除系統管理員角色外，
密碼登入一律被 `POST /v1/auth/login` 拒絕，訊息指向外部登入按鈕。只有在密碼**已經
驗證正確之後**才做這個判斷（緊接在 `!verified` 的檢查之後），所以它不會變成第二個
帳號列舉管道——問到這裡的人已經證明知道正確密碼。

保留系統管理員例外是刻意的：開了這個開關又遇到 IdP 設定壞掉，一般帳號與系統管理員
一起被鎖在外面就沒有人能回設定頁把它關掉。網頁初始化建的 `admin` 帳號沒有 email、
連不上任何外部身分（見〈系統管理員帳號是安裝本身，不是一個人〉），所以這顆例外對它
不是多一道門，而是它唯一的門。存檔時只驗證「開啟必須先啟用外部登入」
（`update_oidc_setting`），不驗證「外部登入目前真的可用」，因為此請求可能同時在改
issuer 等其他欄位，驗證結果本來就會被同一次寫入清空。

真正的安全網在讀取端，而非寫入端的驗證：`auth::login` 不直接讀 `password_login_disabled`
欄位，而是呼叫 `oidc::current()`，只有在**目前這一刻**能建出一個可用 client（discovery
成功、四個必填欄位齊全）時才會執行這個限制。IdP 掛掉、client secret 過期、或設定本身
有錯，`current()` 回 `None`，密碼登入立刻對所有人恢復——不必有人注意到並手動把開關關
回去。這與 `verified_at`／`verify_error` 是同一份自我修復機制，此處只是多一個消費端。

登入頁對稱地讀 `/v1/auth/me` 的 `oauth.password_login_disabled`：為真時預設不畫密碼
欄位，只畫 SSO 按鈕與一顆「系統管理員改用密碼登入」的連結，按下去才展開密碼表單並
把帳號欄預填成 `admin`——畫面在看到憑證之前同樣不知道訪客是不是系統管理員，所以入口
本身不能消失，只能收起來。帳號欄一律 `type="text"`：`type="email"` 會讓瀏覽器擋下
`admin`，而那正是這條路上唯一能用的帳號。

### 解除連結與刪除帳號

`unlink_user_identity`（`AdminOnly`，0046）在同一個交易裡刪掉 `user_identity` 並
撤銷該帳號未過期的 session——OIDC 開出來的 session 不會因為連結消失而失效，不撤等於
沒解。它拒絕 `password_hash IS NULL` 的帳號：解完就沒有人登得進去，而畫面上沒有任何
操作能補一組密碼回去（`reset_user_password` 同樣拒絕外部登入專用的帳號），那條路只剩
主機上的 `laysi-admin reset-password`。畫面因此在那種帳號上請人改用停用。

`delete_user_account` 現在拒絕還連著外部身分的帳號。理由是自動建帳號讓刪除自己撤銷
自己：人還在網域裡，下一次登入又建一個新的。先解除連結是那個刻意的步驟。仍要注意
解除連結本身不等於擋住登入——自動建帳號開啟時，同一個 email 再登入就會由第 2 步重新
連上，真正擋住登入的是停用帳號，或在 Cloud Identity 那側停用該帳號。離職流程照舊走
0043：結案那一步停用登入帳號並撤銷 session 與 MCP Token，那才是本站這側的關門動作。

`list_user_accounts` 回連結的顯示名稱（issuer 的 host，`accounts.google.com` 顯示為
「Google」）、連結當下的 email 與連結時間，帳號管理因此看得出誰用外部帳號登入；連著外部
帳號的那一列不畫「刪除」，改畫「解除連結」，因為前者必定被拒。

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

`create-user` 與 `reset-password` 產生暫時密碼，只在 stdout 顯示一次，資料庫
只存 Argon2id PHC 字串，並設定 `must_change_password = true`。`--role` 每次查
`role` 表，不存在時列出目前所有合法代碼；`--employee-no` 只在需要員工自助時
連到既有員工主檔。`link-oauth` 不建立帳號，provider 應填 discovery issuer
（尾端 `/` 會正規化），subject 填 IdP 的 `sub`。自動建帳號開啟後這支指令很少用得上，
但它仍是唯一能在本人登入之前就把身分綁定的方式。`reset-password` 是唯一能給
外部登入專用帳號（`password_hash IS NULL`）一組密碼的入口，兩個操作層的密碼路徑
都擋住那種帳號。

資料庫完全沒有 `user_account` 時，帳密登入回 503 與「請先完成初始化」；這是唯一會
揭露「帳號總數為零」的 bootstrap 狀態。訊息本身不印指令——看到登入畫面的人未必有
主機權限，指令在本文件與 `laysi-admin --help` 裡。只要已有任一帳號，所有一般登入
失敗就恢復成同一個訊息。

### 網頁初始化：`POST /v1/auth/bootstrap`

第一個帳號可以在瀏覽器建立，不必有主機權限。這是 `/v1/auth/*` 例外的下一層：
登入之所以不能是 Registry 操作，是因為要先有身分才有權限可查；初始化更早一步，
連要被檢查的身分都還不存在，所以它的守門條件是**安裝本身的狀態**。

`0021` 的 `install_bootstrap` 是單列表（`id int PRIMARY KEY CHECK (id = 1)`）。
`Store::bootstrap_first_user` 在**同一個交易**裡先 `INSERT ... WHERE NOT EXISTS
(SELECT 1 FROM user_account) ON CONFLICT (id) DO NOTHING`，再插入帳號：

- `WHERE NOT EXISTS` 擋住同時被 `laysi-admin` 建好帳號的情況；
- 主鍵擋住第二個網頁呼叫——後到者會卡在衝突列上等前者提交，然後影響 0 列；
- 事後把 `user_account` 清空**不會**重新打開這個端點，因為 `install_bootstrap`
  還留著那一列。

Rust 那側的 `bootstrap_open()` 只是為了給出比違反約束更好的錯誤訊息，不是保證。
判斷「還能不能初始化」是兩個條件的 AND：沒有任何帳號，**而且**沒有初始化紀錄。

角色寫死 `admin`（`laysi_ops::ROLE_ADMIN`），呼叫端不能指定。第一個帳號如果沒有
授權他人的權限，整套系統就沒有人能再開權限；未登入端點也不該提供一個只有唯一正確值
的自由度。**它不是 `boss`**：裝機的人不該因此看得到全公司薪資，理由見〈兩道風險
底線〉。登入名稱、顯示名稱與空 email 同樣寫死，理由見〈系統管理員帳號是安裝本身，
不是一個人〉。同理不收 `employee_id`、不收 `status`、不收 `must_change_password`——
密碼是本人當場設的，沒有暫時密碼要強迫更換，這點與 `laysi-admin create-user` 相反。
整個請求因此只有 `password` 與選填的 `setup_token` 兩個欄位。

畫面（`screens/Init.tsx`）仍然畫出「登入名稱」欄位，值固定 `admin`、`readonly`，不隨
請求送出。它不是輸入，是憑證的另外一半：寫成散句的話瀏覽器與密碼管理程式收不到，使用者
存下的會是一組沒有帳號的密碼，下一次在登入頁自動填不出來。用 `readonly` 而非 `disabled`
正是為此——`disabled` 的欄位不參與表單，密碼管理程式一樣讀不到。

兩個環境變數：`SETUP_ALLOW_WEB`（預設 `true`）為 `false` 時端點一律回 403；
`SETUP_TOKEN` 非空時必須在畫面上輸入相同代碼。設定值在行程內只保存 BLAKE3
雜湊（`SetupConfig.token_hash`），比對的是雜湊而不是明文，比較的時間差因此
與代碼內容無關。整個端點另有每分鐘 5 次的行程內計數，順序是
**停用檢查 → 安裝狀態 → 欄位驗證 → 速率 → 代碼 → Argon2id → 交易**：
表單打錯字不消耗額度，只有進到代碼比對與雜湊計算的嘗試才算。

`SETUP_TOKEN` 存在的理由是那段時間差：`docker compose up` 到負責人打開瀏覽器
之間，先到的人就是負責人。內網無所謂，公開位址不是。錯誤的代碼寫 tracing
警告而**不寫稽核**——此時安裝上一個帳號都沒有，任何連得到那個埠的人都能灌爆
`ai_audit_log`。成功那一次會寫，`tool_name = 'bootstrap_install'`，
`channel = 'ui'`，在交易提交**之後**寫：帳號已經存在了，因為留不下痕跡就讓整個
初始化失敗，換來的是一台沒有人能登入的安裝。

`/v1/auth/me` 在已登入與未登入兩種回應裡都帶 `setup:
{ bootstrap_available, token_required }`。未登入畫面要據此決定畫初始化還是登入，
而其他端點都需要 session，這裡是唯一問得到的地方。已登入那條不查資料庫——
手上有 session 就證明帳號存在。查詢失敗時一律回報為關閉：端點本來就會拒絕，
用猜的把初始化畫出來是反過來了。

`LAYSI_SERVE=mcp` 的容器不掛任何 `/v1` 路由，初始化端點自然也不在上面。

## 角色權限：deny-by-default，dispatch 即時強制

`role_permission` 的語意是 `level ∈ full | limited`，**沒有資料列＝沒有授權**。
`Registry::dispatch` 在風險檢查之後，每次都即時把角色的資料列連到
`permission.op_names`；操作不在集合裡就回 `not_permitted`，並以
`outcome = 'blocked'` 留下稽核。刻意不快取，讓 `set_role_permission` 從下一次
呼叫立即生效。

`limited` 仍代表可以進入該組操作，部門或本人範圍由各操作的查詢條件繼續縮小。
兩道風險底線保留為權限矩陣之外的額外限制，權限資料列不會把任何角色提升上去。
0011 先涵蓋原有 76 個操作，0012 再加入 9 個主檔與應收應付建立操作，0014
加入作廢與反向沖銷，0023 加入 `create_user_account`，0025 加入
`list_user_accounts`，0026 與 0027 加入 `update_user_account` 與
`delete_user_account`，0029 加入 `reset_user_password`，0031 移除三個電子發票操作，
0034 加入 `create_budget_version`、0039 加入 `create_fixed_asset`、0044 加入
`change_user_role`；目前 105 個 Registry 操作各恰好屬於一條 permission。
純字串測試會依序重播 0006、0008、0009、0011、0012、0014、0015、0016、0017、0020、
0023、0025、0026、0027、0029、0031、0034、0039、0044 的 INSERT／UPDATE／DELETE，
缺漏與重複都不准通過。

### 角色只在帳號上，員工主檔沒有角色（0040）

`employee.role_code` 從 0008 就在，但決定權限的是 `user_account.role_code`。
員工主檔卻給了一個
可編輯的下拉、七個角色任選，連沒有登入帳號的人都存得進去，名冊還把它跟職稱、
僱用型態排在同一串點號裡：「業務專員 · 正職 · 一般職員」，三個不同的東西一個
語氣。除了顯示它的畫面之外沒有任何程式讀它，所以 0040 直接拿掉那一欄，而不是
換一個比較好的標籤。

一個東西一個名字：角色在帳號上。

### 角色可以變更，但不能變更自己的（0044）

角色原本在建立帳號時決定就固定了，理由是系統管理員不該有辦法把自己升成負責人去讀
薪資。代價是升遷、調職與人力調整都得刪掉帳號重建，連同它的登入紀錄一起丟掉——而
那道保證其實只需要擋一個方向：`create_user_account` 本來就指定得了任何角色，所以
改別人的角色不會多出任何權限，只有改自己會。

`change_user_role` 因此擋三件事：

1. 呼叫者自己的帳號。鍵是 `OpCtx.username`——`actor` 是顯示名稱、會同名，不能拿來
   比對；MCP Token 沒有登入名稱，那條路一律拒絕。
2. 最後一個啟用中的系統管理員。與停用、刪除同一條：沒有人打得開帳號管理就沒有人
   救得回來。
3. 沒帶 `confirm` 的呼叫，因為成功會把對方登出。

變更與善後在同一個交易裡。`dispatch` 每次現查 `user_account.role_code`，所以角色
本身立刻生效，但有兩樣東西不跟著走：瀏覽器手上的 `permissions` 是登入時拿的，而
`api_token.actor_role` 是核發當下的快照。前者留著會讓畫面畫出叫不動的入口，後者
留著就是舊角色永久有效，所以 session 全部撤銷、`owner_employee_id` 指向本人的
Token 一併撤銷，操作回報各撤了幾個。

### 兩道風險底線：負責人與系統管理員

`Risk` 有兩個「限某角色」的層級，它們保護的是不同的東西：

| 底線 | 常數 | 保護什麼 | 操作 |
|---|---|---|---|
| `BossOnly` | `ROLE_BOSS = "boss"` | 業務機密與權威 | `list_payslips`、`list_withholding_statements`、`set_budget`、`create_budget_version`、`set_approval_rule` |
| `AdminOnly` | `ROLE_ADMIN = "admin"` | 系統本身的控制項 | `update_company_setting`、`set_role_permission`、`issue_api_token`、`create_user_account`、`update_user_account`、`change_user_role`、`reset_user_password`、`delete_user_account`、`list_user_accounts` |

0023 之前兩者是同一個：`boss` 既改權限也看薪資，於是**裝機的人自動看得到每個人的
薪水**。分開之後，系統管理員管得了帳號、權限、Token 與公司設定，卻讀不到任何一張
薪資單；負責人看得到薪資、定得了預算與簽核門檻，卻改不了權限矩陣。

這條分離之所以成立而不只是慣例，靠的是兩件事：底線在權限矩陣之外，`set_role_permission`
無法把 `admin` 提升到 `BossOnly`；而 `change_user_role`（0044）**拒絕變更呼叫者
自己的帳號**。系統管理員改得動別人的角色——建帳號時本來就指定得了任何角色，這個
方向沒有多出權限——但升不了自己。

系統管理員另外持有 `manage_masters`，因為首次設定要建銀行帳戶與部門；該組裡的
`set_approval_rule` 仍擋在負責人底線之後。`revoke_api_token` 刻意只是 `HighRisk`：
撤銷往安全的方向走。

`create_user_account` 是唯一不需要 shell 就能建立登入的路徑。它不收密碼——密碼由
系統產生、只在該次回應出現一次、`must_change_password = true`，所以建立帳號的人
無法長期使用別人的帳號。連結員工用 `employee_no` 而非姓名，姓名會重複。

`list_user_accounts` 是它的讀取面，同在管理員底線上：誰能登入與誰發帳號是同一件
事。查詢不選 `password_hash`，回傳型別是 `UserAccountRow` 而不是帶雜湊、刻意不實作
`Serialize` 的 `UserAccount`。權限放在 `manage_access` 而非 `view_system`——後者是
負責人也持有的唯讀組。

`list_roles` 回傳 `enforced: true` 與即時查詢的 `enforcement_note`；
`missing_ops` 正常應為空，只保留作部署後的契約診斷。

**`/v1/auth/me` 的 `permissions` 也要扣掉這兩道底線。** `dispatch` 先驗底線再查
矩陣，而權限矩陣把每一項都給了負責人，所以只查矩陣的清單會說負責人可以
`update_company_setting`——基礎設定於是畫出編輯鈕、收下輸入值，按儲存才被擋。畫面
一律以這份清單決定要不要畫入口，所以清單的意思必須是「這個身分真的叫得動的操作」，
而不是「矩陣上有的操作」。`auth::callable_by` 用 Registry 的 `risk()` 做這次過濾；
註冊表查不到的名稱保留下來，因為有測試釘住兩邊的名稱集合相等，對不上是要被看見的錯，
不是要被藏起來的列。

**錯誤訊息與稽核摘要不出現註冊表鍵值與角色代號。** 三個權限錯誤（`NeedsBoss`／
`NeedsAdmin`／`NotPermitted`）在組字串前，用 `op_label(op.summary())` 取操作中文名的
第一段、用 `role.name` 取角色顯示名。名稱從 `summary()` 推導而不另設欄位：那段文字
本來就是 UI 文案，多一個欄位就是多一份會漂走的副本；`every_operation_yields_a_refusal_label_without_its_registry_key`
逐一檢查每個操作都推得出不含識別字的名稱。

**`role_permission` 與 `mcp_tool_grant` 不得合併。** 前者管人、後者管 AI；
把 `post_voucher` 對 AI 關掉，不該讓會計人員也不能過帳。

## API Token

`api_token` 只存 BLAKE3 雜湊與前八碼。`issue_api_token` 的回應是**唯一**看得到
明文的地方，`ApiToken` 型別沒有明文欄位，也不提供取回的方法。稽核摘要同樣不得
包含明文，因為稽核紀錄是給人翻閱的。

`issue_api_token` 是 `AdminOnly`，`revoke_api_token` 刻意只是 `HighRisk`：撤銷是往
安全的方向走，Token 外洩時不該還要等系統管理員到場。

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

Schema 是**一個檔案**：`backend/migrations/0001_initial.sql`，以 `sqlx::migrate!()`
在啟動時執行。上線前那 63 份 migration 已經合併進去，因為當時還沒有任何一個安裝需要
沿用，而新裝的人讀一份 SQL 比讀 63 份差異合理。

**本文與 `CLAUDE.md` 裡的四位數編號（0016、0044、0057……）指的是 git 歷史，不是檔案。**
單一決定的來龍去脈用 `git log --diff-filter=D -- backend/migrations` 找得回來；會影響
跨層契約的結論寫在本文，不靠那些檔案保存。

合併之後**每一次 schema 變更都是新的一份 migration**，0001 不再更動——它已經被套用過，
改一個位元組就會讓現有的資料庫拒絕啟動。目前另有 `0002_dimension_value_rename.sql`
（`update_dimension_value` 的 permission 與 mcp_tool_grant）、`0003_drop_ocr_status.sql`、
`0004_notice.sql`、`0005_notice_versions.sql` 與 `0006_notice_read.sql`（公告與規章）。

**「已套用過的檔案一個位元組都不能改」是實測過的**：0003 曾經在套用之後被補寫一段
`UPDATE`，於是那個安裝的伺服器每次啟動都停在
`migration 3 was previously applied but has been modified`。修法是補跑漏掉的那段、
再把 `_sqlx_migrations.checksum`（檔案的 SHA-384）對齊現在的檔案，不是改檔案。

**新增一份 migration 要同時加進 `laysi-ops` 的 `MIGRATIONS`**：兩個種子比對測試以
`include_str!` 讀那份清單，算出 permission 與 mcp_tool_grant 的最終狀態再與 Registry
對照。permission 的最終狀態取「最後一個提到該 `code` 的敘述」，所以 0002 之後的
migration 要改權限就整份重寫 `op_names` 陣列，不要用陣列附加。

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

-- 收付
ar_invoice / ap_bill        明細帳。餘額的真相仍是總帳 1131 / 2101，
                            這兩張表只承載明細與到期日
bank_txn                    銀行匯入的流水；核銷是把它對到一張傳票，不產生分錄

-- 稅務與資產
withholding_record          給付事實。**不存稅額**，稅額由 ruleset 現算
fixed_asset                 **不存耐用年數**，只存 category；年數來自 ruleset
depreciation_run            一期一筆（period UNIQUE），存 ruleset_version
depreciation_entry          逐項金額，含當時解析出的 useful_life_years

-- 報表分組、角色權限、安全
account.report_group        資產負債表／損益表的分段，NOT NULL，以 CHECK 與 kind 綁死
account.cash_flow_section   現金流量表活動別；損益科目必須為 NULL（CHECK 強制）
company_setting             公司與帳務設定。**法規決定的值不得進這張表**
dimension / dimension_value 部門。0060 刪掉專案、成本中心與付款條件三個沒有
                            消費者的分類；department 以複合外鍵掛到分錄與員工
role / permission / role_permission
                            人的權限矩陣。與 mcp_tool_grant 是兩件事：前者管人，
                            後者管 AI。dispatch 每次呼叫即時強制 deny-by-default
approval_rule               費用簽核流程與金額門檻（公司政策，不是法規）
api_token                   **不存明文**，只有 BLAKE3 雜湊與前八碼

-- 公告與規章（0004、0005、0006）
notice                      身分與現行版本；kind ∈ announcement | policy。內容不在
                            這裡，version_no 以 deferred 外鍵指向現行版本
notice_version              每一個版本，只增不改。標題、內文、生效日與發布人
notice_read                 一人一列，只記「看到哪一刻為止」。不是閱讀確認

-- 費用、預算、文件、匯入
voucher_line.dept_id        分錄的部門分類。以 (id, dimension_code) 複合外鍵
                            綁死只能指向「部門」那一組值
document / document_version 文件身分與版本。**內容不在資料庫**，只有物件鍵、
                            位元組數、MIME 與 BLAKE3；版本不可異動（trigger）
expense_claim / expense_claim_step
                            報支單與每一關的簽核紀錄。CHECK 綁死
                            「已核准 ⇔ 有傳票」
budget_version / budget     **只有預算數，沒有實際數**。實際數一律現查
import_source / import_batch / import_issue
                            `supported` 決定檔案交給哪個解析器；`payload` 存預覽
                            結果。0061 刪掉五列沒有解析器的來源

-- 登入、權限、主檔操作與 MCP 憑證
user_account                登入帳號；可選擇連到 employee，密碼雜湊可為 NULL
user_identity               以 IdP 的 provider + subject 唯一識別；email 只供稽核
user_session                只存 token 雜湊、到期與撤銷狀態，明文不落地
permission.op_names         116 個 Registry 操作各恰好屬於一條 permission
account.is_active           0012 新增；停用阻止新分錄，但保留歷史帳
api_token（0013）           MCP 身分改由 api_token 雜湊驗證與資料列綁定
voucher.reverses            0014 新增；沖銷傳票指回原傳票，UNIQUE 防止重複沖銷

-- 員工的銀行帳戶
employee_bank_account       一人可有多筆；`is_payroll` 標出薪轉的那一筆，
                            以 partial UNIQUE 索引限制至多一筆。
                            原本是 `employee.bank_account` 單一自由文字欄

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

### 單據編號

格式 `{前綴}{西元年末兩碼}{月份兩碼}-{三碼流水}`，例：2026/07 第 18 張傳票 →
`V2607-018`。以 `doc_sequence` 取號，同一交易內遞增。傳票 `V`、付款批次 `P`、
費用報支 `EX`、文件 `D`、匯入批次 `IM`、休假單 `LV` 共用這一套。

**休假單原本用民國年**（`LV11507-001`），於是同一天建立的四張單上並排著兩種
曆法。年份的寫法是編號格式的一部分，不是各模組自己的選擇。

**計數器的鍵是（前綴, 期間），不是前綴（0041）。** 原本一個前綴一列，另存一個
`period` 欄，換期間就把計數器歸 1 並覆蓋期間。往前走是對的——新的月份從 001
開始；往回走就不是。提列 9 月折舊（傳票日期 09-30）之後再算 8 月薪資（傳票日期
08-31），計數器歸 1 而發出 `V2608-001`，那個號碼已經存在，`voucher.no` 是 UNIQUE，
於是薪資作業以資料庫約束錯誤收場，而畫面上繞不過去。0041 把鍵改成兩欄，並用各
單據表已發出的號碼重建每一期的計數器。

### 設定值必須做得動（0033）

`company_setting` 原本種了五筆 `editable = false` 而 `value = ''` 的列，精靈第一步
還叫使用者「請至基礎設定處理」——走到那裡發現五格都按不下去。五筆是五種不同的東西：

- `currency`　單一國家產品只有一種本位幣，種 `TWD`，維持不可編輯。
- `fiscal_year`　真的是一個決定，所以改成可編輯，值是**起始月份**。
  `Store::fiscal_year_start_month` 是資產負債表與損益表解析預設期間的依據，
  所以那一格是被讀的，不是只被存的。
- `period`、`doc_number`　不是設定，是推導結果（前者來自日曆，後者來自單據編號器）。
  存起來就會變成第二份會漂移的副本，因此改由 `get_company_settings` 現算並標
  `source = "derived"`。
- `opening_locked`　期初餘額目前就是開一張傳票，沒有匯入功能可以連過去，所以整列
  移除，而不是留一格永遠寫著「尚未建立」。等期初匯入做出來再放回去。

**可編輯就要驗**：`update_company_setting` 對 `tax_id` 算財政部的統一編號檢查碼
（權數 1、2、1、2、1、2、4、1，逐位數字相乘後各位數相加，總和被 5 整除；第七碼
為 7 時另接受總和加一後整除），對 `fiscal_year` 只收 1 到 12，對 `work_start`／
`work_end` 只收 `HH:MM`（兩位小時、兩位分鐘的 24 小時制），對兩個分鐘數只收整數，
對 `punch_correction_policy` 只收它的兩個值。驗在後端而不只在精靈裡，因為 MCP 與
未來的匯入程式走的是同一個操作。這些值不是法規數值，是公司自己的登記事實與決定，
所以留在 `company_setting` 而不進規則檔。

**只有幾個答案的設定由後端列出來，畫面照著畫**：`get_company_settings` 每一列多回
一個 `options`，有值就是下拉、沒值就是文字欄。`punch_correction_policy` 原本是文字
欄，於是政策只有兩個答案，畫面卻要人把其中一個逐字打對，打錯了就靜靜當成「人資
審核」。清單放在後端，是因為判斷這個值合不合法的也是後端。

**留空是答案，不是缺欄位。** `CLEARABLE_SETTINGS`（`work_start`、`work_end`、
`late_grace_minutes`、`flex_window_minutes`）收得下空字串，其餘仍回「設定值為必填」。
理由在 `attendance::late()` 回的是 `Option<bool>`：`None` 代表沒有人說過幾點上班，
與「沒有人遲到」是兩件事。沒決定的公司要說得出「還沒決定」，設過之後也要收得回來。
首次設定精靈因此多了一步問上下班時間——跑精靈的就是系統管理員，也就是唯一改得動這
一項的人，在那裡問一次，打卡頁那句「請系統管理員去填」就不必有人去執行。

### 預載基礎資料

- `account`：提供可直接開始設定的基礎科目表；報表分組與現金流量活動別一併建立。
- `role`、`permission`、`role_permission`、`mcp_tool_grant`：提供預設拒絕的權限
  與 AI 工具開關。
- `approval_rule`：提供簽核門檻。
- `dimension`：只建立「部門」這一組定義，項目由各公司自行建立。畫面上一律稱
  「分類」「類別」「項目」——「維度」是 BI 與對岸 ERP 的講法，資料表與操作名
  維持英文 `dimension` 不動。
- `company_setting`：保留設定鍵與標籤，值一律為空字串，等公司自行填寫。
- `import_source`、`reg_ruleset`：保留匯入來源與法規投影。

正式安裝不預載銀行帳戶、傳票、應收應付、固定資產、預算、報支、員工、差勤、
薪資、API Token、分類項目或單據流水起始值。

## laysi-ops：Operation Registry

「所有操作支援 MCP」的保證機制。每個操作只定義一次，registry 同時餵給
REST、MCP 與 TypeScript 產生器。

```rust
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Risk { ReadOnly, NeedsConfirm, HighRisk, BossOnly, AdminOnly }
// 對應原型：唯讀 / 需人確認 / 高風險 / 限負責人

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
    pub fn new() -> Self;             // 註冊目前全部 116 個操作
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
2. 風險底線：`BossOnly` 需 `actor_role = "boss"`，`AdminOnly` 需
   `actor_role = "admin"`；擋下來一樣寫 `outcome = 'blocked'` 的稽核。
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
| `update_company_setting` | AdminOnly | `{ code, value }` | `{ setting }`；`editable = false` 的擋下；`tax_id` 驗檢查碼、`fiscal_year` 收 1 到 12、有 `options` 的只收清單內的值 |
| `list_chart_of_accounts` | ReadOnly | `{ only_used? }` | 科目＋報表分組＋活動別＋餘額 |
| `list_dimensions` | ReadOnly | `{}` | `dimensions[]`（只有部門） |
| `list_roles` | ReadOnly | `{}` | `roles[]`、`permissions[]`、`grants[]`、**`enforced: true`** |
| `set_role_permission` | AdminOnly | `{ role_code, permission_code, level: full\|limited\|none }` | `{ cell, enforced: true }`；下次呼叫立即生效 |
| `create_user_account` | AdminOnly | `{ username, display_name, email?, role_code, employee_no? }` | `{ user, temporary_password }`；密碼只顯示這一次，`must_change_password = true` |
| `update_user_account` | AdminOnly | `{ username, display_name?, email?, status?, employee_no? }` | `{ user, changed[] }`；**改不到 `role_code` 與密碼**，`employee_no: ""` 解除連結 |
| `change_user_role` | AdminOnly | `{ username, role_code, confirm }` | `{ user, revoked_sessions, revoked_tokens }`；拒絕呼叫者自己的帳號與最後一個啟用中的管理員 |
| `reset_user_password` | AdminOnly | `{ username, confirm: true }` | `{ user, temporary_password, revoked_sessions }`；舊密碼與該帳號所有登入立刻失效，只用外部帳號登入的帳號拒絕 |
| `delete_user_account` | AdminOnly | `{ username, confirm: true }` | `{ user, remaining }`；session 與外部身分連帶刪除，最後一個帳號與最後一個啟用中的管理員擋下 |
| `list_user_accounts` | AdminOnly | `{}` | `accounts[]`：登入名稱、角色、連結員工與其在職狀態、帳號狀態、`must_change_password`、`has_password`、最後登入；**無密碼雜湊** |
| `list_approval_rules` | ReadOnly | `{}` | `steps[]`、`enforced: true` |
| `get_security_policy` | ReadOnly | `{}` | `rules[]`，每條帶 `source`；四條全部 `enforced` |
| `list_api_tokens` | ReadOnly | `{ include_revoked? }` | `tokens[]`（**無明文**）、`storage_note`、`wiring_note` |
| `issue_api_token` | AdminOnly | `{ name, kind, scope, actor, actor_role, rotate_days?, expiry_note? }` | `{ token, secret }`，`secret` 只顯示這一次 |
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
| `create_employee` | NeedsConfirm | `id_no` 驗檢查碼後完整保存；三個投保級距照申報事實保存；`bank_accounts` 一次收整份清單；不建立登入帳號 |
| `update_employee` | NeedsConfirm | 除四個金額外完整更新；`base_salary` 與三個投保級距是三態（不帶＝不動、`0`＝清為尚未設定、給數字＝更新）；`bank_accounts` 不帶＝不動、給清單＝整份取代；`leaving`／`left` 必須有 `left_on`；不修改登入帳號 |
| `create_bank_account` | NeedsConfirm | `gl_code` 必須指向啟用中的可入帳資產科目 |
| `create_dimension_value` | NeedsConfirm | 只在既有 `dimension` 下新增項目；不建立分類。`code` 留空時取 `label` |
| `update_dimension_value` | NeedsConfirm | 依 `(dimension_code, code)` 改名或切換 `active`；兩個欄位都不帶就回「沒有變更」。改名只在 `code` 仍等於舊 `label` 時一起改寫 `code`——CSV 匯入以 `code` 比對且會把可用的代碼列給使用者看，另外指定過的 `code` 是別人的鍵，不動 |
| `set_approval_rule` | BossOnly | 依 `step_no` upsert 或刪除；`min_amount = null` 代表一律需要，流程至少留一關 |
| `create_ap_bill` | NeedsConfirm | 同一交易寫 `ap_bill` 與待過帳傳票；借方拆出 1284 進項稅額；不會直接付款 |
| `create_ar_invoice` | NeedsConfirm | 同一交易寫 `ar_invoice` 與待過帳傳票；貸方拆出 2211 代收營業稅 |

**應收與應付都要拆稅**：401 申報讀的是 2211 與 1284 的期間變動，所以一張沒拆稅的
單據會讓申報短少剛好那筆稅額。兩個操作的金額欄因此是 `amount`（含稅）、`net_amount`
（未稅）、`tax_amount` 三選一或二，其餘由 `ops::tax::split_vat` 依單據日期解析出的
`vat` ruleset 補齊；稅率不進 Rust，反算含稅金額的 `laysi_regulation::vat_from_total`
是 `vat` 的反函數。零稅率與免稅由明確的 `tax_amount = 0` 表示，不設旗標；稅額為零
時不產生那一行分錄，因為金額為零的分錄不成立。`list_receivables` 與 `list_payables`
會回一份 `vat`，畫面用它把三欄補滿——前端不得自帶稅率。

科目、員工、銀行、
分類與簽核規則是結構性主檔，MCP grant 預設關閉；AR/AP 是仍需人工過帳的日常
單據，預設開啟。人的權限另由 permission 管理：`manage_masters` 只給老闆與
內部會計；出納不會因為能核銷銀行流水，就取得改科目表的能力。

### 固定資產：登記（0039）

`list_fixed_assets`、`preview_depreciation`、`run_depreciation` 從 0005 就在，但沒有
任何一條路把資產放上登記簿——沒有操作、沒有 MCP 工具、匯入也沒有來源。整個模組
只讀不寫，買了機器的公司在那一頁做不了任何事，而頁尾仍在說明折舊傳票長什麼樣。

| name | risk | input／行為重點 |
|---|---|---|
| `create_fixed_asset` | NeedsConfirm | 同一交易寫 `fixed_asset` 與取得傳票（借資產科目與 1284 進項稅額／貸付款科目）；`cost` 是未稅成本，`tax_amount` 留空時依取得日的 `vat` ruleset 拆出 |

三條規則決定了它的形狀：

- **成本是未稅金額**。它同時是 1601 的借方與折舊的計算基礎，不能含稅。營業稅走
  `split_vat` 與應收應付同一條路，免稅寫明確的 `tax_amount = 0`。
- **每一筆資產都要指向一張傳票**：`paid_from_account_code`（本次產生）或
  `origin_voucher_no`（已存在的供應商帳單、匯入的期初分錄）二擇一，兩個都不填
  即拒絕。少了這條，登記簿會宣稱一筆總帳從未見過的成本，而這一頁的第一句話就是
  拿登記簿跟 1601 對帳。
- **賒購不由這裡貸 2101**。應付帳款頁把未付合計與總帳 2101 對帳，直接貸 2101 會
  在總帳留下一筆應付帳款頁看不到的負債。賒購的正解是在應付帳款以 1601 為借方
  科目開單，再把那張帳單的傳票號填進來。

耐用年數仍然只在耐用年數表裡。`list_fixed_assets` 另回一份 `categories`（代碼、
名稱、年數）與 `vat`，畫面的類別下拉與稅額三欄都讀它——**前端不得自帶年數或稅率**。
未收錄的類別在登記時就回錯誤並列出可選項目，而不是等到幾個月後第一次提列折舊。

**資產科目與累計折舊科目由使用者選，所以對帳與折舊都不能寫死 1601／1602。**
一開始兩者是常數，機械與運輸設備因此全部掛在「辦公設備」底下。改成可選之後有三
件事跟著動：

- `create_fixed_asset` 驗證兩個科目是 `non_current_asset`、可入帳、啟用中，且
  借方那個 `normal_side = debit`、累計折舊那個 `= credit`。挑錯科目當場回錯誤，
  而不是讓對帳從此對不上。
- `list_fixed_assets` 拿登記簿實際用到的科目集合去查總帳餘額，並回
  `cost_accounts_label`／`accum_accounts_label` 給畫面。寫死一組科目報出來的差額，
  真正的意思是「你看錯科目了」。
- `record_depreciation_run` 的貸方**依累計折舊科目分組，一個科目一行**。原本合併
  成一行貸 1602，一旦有第二組科目，那一組的對帳就永遠差一個數。

### 費用與文件模組（0007）

| name | risk | input | output 重點 |
|---|---|---|---|
| `list_expense_claims` | ReadOnly | `{ include_done?, limit? }` | `claims[]`（含 `required_steps` 與每一關的 `reason`）、`flow[]`、`thresholds_source` |
| `submit_expense_claim` | NeedsConfirm | `{ title, amount, spent_on?, dept_code?, suggested_account_code?, … }` | `{ claim }` |
| `approve_expense_claim` | HighRisk | `{ claim_no, account_code? }` | `{ claim, voucher, step_taken, finished }`；月底彙總匯款的單據同一交易寫一筆 `ap_bill`，帳單號碼即單號 |
| `reject_expense_claim` | NeedsConfirm | `{ claim_no, note? }` | `{ claim }` |
| `get_budget_status` | ReadOnly | `{ version?, from?, to? }` | `rows[]`、`budget_total`、`actual_total`、`alert_percent`、`actual_source`；一版都還沒建時 `version` 為 `null`，回空狀態而不是錯誤 |
| `get_department_pl` | ReadOnly | `{ from?, to? }` | `rows[]`（含 `unallocated`）、`total_profit`、`statement_profit`、`reconciled` |
| `create_budget_version` | BossOnly | `{ code, label?, fiscal_year?, is_current?, note? }` | `{ version }` |
| `set_budget` | BossOnly | `{ account_code, amount, version?, dept_code? }` | `{ budget }` |
| `list_documents` | ReadOnly | `{ q?, limit? }` | `documents[]`、`stats`、`bucket`、`bucket_reconciled` |
| `upload_document` | NeedsConfirm | `{ file_name, content_base64, kind?, link_type?, link_ref?, doc_no? }` | `{ document, new_version }` |
| `get_document` | ReadOnly | `{ doc_no, version?, include_content? }` | `{ document, verified, checksum_recomputed }` |
| `list_imports` | ReadOnly | `{}` | `sources[]`、`batches[]`、`csv_columns` |
| `preview_import` | NeedsConfirm | `{ source_code, file_name, content }` | `{ batch, drafts, can_commit, blocked_note }` |
| `commit_import` | HighRisk | `{ batch_no }` | `{ batch, posted: false }` |
| `rollback_import` | HighRisk | `{ batch_no }` | `{ batch, removed }` |

`approve_expense_claim`、`commit_import`、`set_budget`、`create_budget_version` 在
`mcp_tool_grant` 中**預設關閉**：自己送自己簽等於沒有簽核流程；一次寫進幾百張傳票，
而復原只救得了未過帳的；預算是老闆定的，AI 不該改掉自己的比較基準。
`rollback_import` 維持開啟，理由與 `revoke_api_token` 相同。

**0034 補上 `create_budget_version`，因為空狀態原本沒有出口**：一版都還沒建時
`get_budget_status` 回的是一句「請先建立一版」，而畫面上沒有任何按鈕可以建——
那句話指的動作做不到。現在該操作與 `set_budget` 同一條 permission、同一道底線，
理由也一樣：版本是每一個執行率的分母。

**月底彙總匯款的報支單同時是一張應付帳單（0042）**。最後一關產生的傳票貸 2101，
而應付帳款頁比對的是 `ap_bill` 明細合計與 2101 的總帳餘額，所以只動總帳的那一筆
會讓兩邊永遠差一個報支金額；畫面上也修不掉，因為「新增應付帳單」一定會自己再開一張
傳票，補一張只會把差額變成兩倍。核准因此在同一個交易裡寫 `ap_bill`，帳單號碼就是
報支單號（`EX2608-001`），供應商記申請人，到期日為傳票月份的月底。這也是這筆錢
唯一的匯出路徑——付款單只讀 `ap_bill`。零用金支付的單據貸 1101，錢已經付掉，不建帳單。

**能不能簽由後端回答，不由畫面推**。`list_expense_claims` 逐列附上 `can_approve`
與 `approve_blocked_reason`，`list_leave_requests` 附上 `can_decide` 與
`decide_blocked_reason`；`false` 的那一列畫面**畫不出按鈕，改印那一句原因**。原本
按鈕一律是亮的，按下去才回一句拒絕，而頁面上還寫著「無法簽核時，下方會顯示原因」
——原因其實是螢幕底部一閃即逝的浮動訊息。規則只有一份（`may_decide` 與
`decide_leave_request` 本身），畫面讀的是它的結論。

**申請人不得核准本人送出的單據，休假與報支同一條規則**。休假原本沒有這道限制，
於是同一套「職責分離」在兩條流程上給出相反的答案。休假比對的是
`leave_request.employee_id` 與 `ctx.employee_id`（`decided_by` 是顯示名稱，會同名，
不能當鍵）。一人公司因此兩條都簽不動，這是正確答案而不是缺口：拒絕訊息直接說出唯一
的出路——請系統管理員在「帳號管理」建立第二個帳號，因為建帳號是 `AdminOnly`，
負責人自己做不到。

**休假是一段時間，時數與天數由它算出來（0056）**。原本的請假單是兩個日期加一個
申請人自己填的 `days`，於是同一張單上有兩個可以互相矛盾的說法，而「半天」是誰填誰
算數。現在 `from_at`／`to_at` 是牆上時鐘的當地時刻（單據講的是上班日，不是全球
時間軸上的一個瞬間），`from_on`／`to_on` 改由它們推導，是資料庫的 generated
column，因此不可能與上面的區間不一致。

`hours` 是這段區間落在上下班時間內的部分，逐日相加，週六與週日不計；`days` 等於
`hours` 除以一天的上班時數。兩者在送出當下算好存下來——上下班時間是
`company_setting` 的單一現值、沒有生效日，事後改動它不該回頭改寫已經核准的單。
**上下班時間沒填就送不出休假單**：那個區間是把時間換算成時數的唯一依據，退而求其次
就是自行編一個工作日。畫面在這個狀態不畫表單，改請系統管理員去填。

國定假日與週休的哪兩天目前不在系統裡（規則庫還沒有這份日曆），所以跨到國定假日的
休假會多算那一天，程式裡以 `ponytail:` 註記。

`decide_leave_request` 順便補上一個對不上的宣告：模組註解說狀態由更新本身判定，
程式卻是先讀再無條件更新。現在 `UPDATE` 自己帶 `status = 'pending'` 並檢查
`rows_affected`，兩位主管同時按下去只有一位會成功。

### 人資模組（0008）

| name | risk | input | output 重點 |
|---|---|---|---|
| `get_hr_dashboard` | ReadOnly | `{}` | 在職人數、待辦與人資／財務銜接狀態 |
| `list_employees` | ReadOnly | `{ q?, include_left? }` | 名冊、特休額度、約定的底薪與三個投保級距；**不含系統角色** |
| `list_leave_requests` | ReadOnly | `{ include_done?, limit? }` | 請假單與依 `annual_leave` ruleset 現算的逐人額度 |
| `decide_leave_request` | NeedsConfirm | `{ leave_no, approve, note? }` | `{ request, action }` |
| `list_overtime_requests` | ReadOnly | `{ month?, include_done? }` | 加班申請、逐人本月已核准時數與加班費，加班費現算 |
| `decide_overtime_request` | NeedsConfirm | `{ request_no, approve, note? }` | `{ request, action }` |
| `get_payroll_summary` | ReadOnly | `{ period? }` | 薪資彙總與總帳對帳；不含個人金額 |
| `list_payslips` | BossOnly | `{ period? }` | 逐人薪資與保費重算對帳 |
| `run_payroll` | HighRisk | `{ period?, pay_on?, bank_account_id?, note? }` | 薪資單與待過帳傳票 |
| `get_insurance_summary` | ReadOnly | `{ as_of? }` | 逐人投保級距與三項費額 |
| `compute_insurance` | ReadOnly | `{ insured_salary, health_insured_salary?, pension_wage?, as_of? }` | 單一級距試算與三份 ruleset |
| `get_hr_compliance` | ReadOnly | `{ as_of? }` | `checks[]`；逐項帶 `checked`、`passed`、ruleset 與差異 |
| `list_withholding_statements` | BossOnly | `{ year? }` | 逐給付對象年度所得；`table` 稅額缺值保持 `null` |

法遵檢核的 `checked_all_passed` **只涵蓋實際算過的項目**。完整投保薪資級距、
國定假日與例假出勤工資尚未收錄，會以 `checked = false`、`passed = null` 與
`not_checked_reason` 回傳；`coverage_complete` 因此是 `false`，畫面不得把它
改寫成「全部法規皆符合」。每月延長工時上限自 0057 起是**實際檢查**的項目：逐人
加總當月已核准的加班時數，與 `overtime` ruleset 的上限比較。

年度扣繳憑單以 `withholding_record` 的 `payee`／`payee_id` 分組。`basis = rate`
依給付日 ruleset 計算，`basis = table` 只採來源系統帶入的
`withheld_amount`；來源沒帶就保留 `null`。來源若只有多人合併總額，回應會標成
`person_specific = false`，不自行拆出一份不存在的逐人資料。薪資模組的
`payroll_year_totals` 另列為 `employee_income`，並與**薪資所得代號（50）**的紀錄
對帳；兩邊若不同就回報差額，不用其中一邊覆蓋另一邊。

**`run_payroll` 在同一個交易裡寫入這些給付紀錄。** 沒有它，扣繳頁永遠寫著「對不上，
差一整年的薪資」，而畫面上沒有任何動作可以消除——那不是對帳，是一個做不完的待辦。
`basis` 記 `rate`：稅額本來就是由給付日的 ruleset 以扣繳率現算，存一份等於同一個問題
有兩個答案，而 `withholding_record` 的 CHECK 也正是為此只在 `table` 時要求稅額。
對帳因此改看所得代號而非 `basis`——依扣繳率算出來的紀錄一樣是薪資所得。

**名冊回傳底薪與三個投保級距。** 原本不回，於是全系統沒有一頁看得到某個人現在的
本薪：調薪要憑記憶，編輯抽屜也無法把「留空＝不動」的那個「原值」顯示出來。這不是
放寬權限——同一組權限底下的 `get_insurance_summary` 本來就逐人列出三個投保級距，
而持有 `view_hr` 的只有負責人與會計，兩者都已看得到薪資明細。逐月薪資單仍在
`list_payslips`，仍限負責人。

**離職與關掉存取是同一件事（`close_access`）**。`update_employee` 把狀態改成 `left`
時，同一個交易接著把他的登入帳號停用、撤銷未過期的 session，並撤銷
`api_token.owner_employee_id` 指向他的 MCP Token。三件事必須一起做：session 查詢雖然
帶 `u.status = 'active'`，但 MCP Token 不讀那一欄，而生效日之後也沒有任何排程回頭
檢查，所以留著的登入就是永久有效的登入。唯一的例外是**最後一個啟用中的系統管理員**
——沒有任何操作能把角色給出去，停用它等於誰也打不開帳號管理，因此那次更新直接拒絕並
要求先建立另一個管理員帳號。`update_employee` 以 `closed_access` 回報實際關掉了什麼，
畫面照實說出來；`list_user_accounts` 另外回連結員工的在職狀態，讓帳號管理不必跳到
名冊才看得出這個人已經離職。

這條銜接原本掛在異動單的離職流程最後一關（0008 的 `hr_flow_step`／
`employee_movement`）。0058 把整套異動單刪掉：四種異動裡，調薪與調部門結案時只寫
一個欄位，而名冊的編輯抽屜一步就寫得到同一個欄位，也沒有任何機制要求走異動單；到職
流程結案不寫任何東西；`hr_flow_step` 全 codebase 沒有寫入路徑，十五列是穿著資料表
外衣的常數。真正沒有第二條路的只有離職這段交易，於是它搬到 `update_employee`——那正是
原本繞得過去的那條路。

0008 的 `mcp_tool_grant` 名稱必須與上述五個人資操作模組在 Registry 裡的名稱集合
完全相等；`laysi-ops` 以 `include_str!` 直接讀 migration 的測試守住這條契約，0058
刪掉的三個工具在該測試裡另以一份名單扣除（`grant_names` 只讀 INSERT）。

### 公告與規章（0004、0005、0006）

| name | risk | input | output 重點 |
|---|---|---|---|
| `list_notices` | ReadOnly | `{ id?, kind?, include_ended? }` | 現行版本；帶 `id` 回該則的每一個版本 |
| `create_notice` | NeedsConfirm | `{ kind, title, body, effective_on?, expires_on? }` | `{ notice }`，記為第 1 版 |
| `update_notice` | NeedsConfirm | `{ id, title?, body?, effective_on?, expires_on? }` | `{ notice, revised }` |
| `list_my_notices` | ReadOnly | `{ id? }` | 生效中的內容＋`unread`；帶 `id` 回每一個版本 |
| `mark_my_notices_read` | NeedsConfirm | `{}` | `{ read_at }` |

公告與規章是**同一張表**：欄位一模一樣，`kind` 是唯一的差別，而狀態由
`effective_on`／`expires_on` 與當日比較現算。兩種類別對同樣三個狀態各有各的說法
（公告：未發布／公告中／已下架；規章：未生效／施行中／已廢止），`expires_on` 當天
仍然算數。

**內容只住在 `notice_version`，而且只增不改。** 標題、內文與生效日都是某一版的事實，
改動任一項就是新的一版；設定下架或廢止日不是內容，所以不進版（`revised` 回報是哪一種）。
`notice.version_no` 以一條 **deferred** 外鍵指向現行版本——建立時要先寫 `notice` 才寫得了
`notice_version`，立即檢查會擋在中間。生效日與廢止日的先後改由 ops 層驗：兩個值從此
不在同一張表，CHECK 寫不出來。

0004 原本的做法是改寫規章就刪掉 `notice_ack`。**那條路把「誰在哪一天讀過哪一段文字」
銷毀，而那是勞資雙方事後唯一拿得出來的東西**，所以 0005 把閱讀確認整套移除，改成保存
版本並讓任何看得到這一頁的人都讀得到舊版。`list_notices` 與 `list_my_notices` 帶 `id`
時回的是同一份版本清單，兩邊不許有不同的答案。

**未讀是「看到哪一刻為止」，不是簽核（0006）。** `notice_read` 一人一列只記時刻，沒有
版本，人資那一面也讀不到它，所以它不是換一個名字的閱讀確認。沒有紀錄＝全部未讀：新人
必須看得到他到職前就貼上的規章。標記掛在 `employee_id` 而不是登入帳號，因為自助操作
一律以 `ctx.employee_id` 為鍵，MCP Token 也拿得到同一個鍵；換成 `user_account` 會讓兩個
通道看到不同的未讀數。**替代方案「最近 N 天內發布或修訂」被否決**：那是每個人都一樣的
數字，讀完也消不掉，而消不掉的提醒沒有人會再看第二次。

權限沿用人資那兩組，不另立一條：`list_notices` 屬 `view_hr`，`create_notice` 與
`update_notice` 屬 `manage_hr`。員工端的兩個操作定義在 `selfservice.rs`，因此受
`no_self_service_input_can_select_another_employee` 逐字釘死——`list_my_notices` 的
`id` 指的是一則公告，而公告不屬於任何人，是那份名單裡唯一不以人縮小範圍的欄位，因為
裡面沒有人可以縮。`mark_my_notices_read` 與 `create_notice`／`update_notice` 的
`mcp_tool_grant` 一律關閉。

**內文是 Markdown。** `web/src/components/Markdown.tsx` 直接產生 React 元素，
**不經 `dangerouslySetInnerHTML`**，所以結構上沒有 HTML 注入的路，也不需要另外接一個
消毒程式庫。支援的子集是：`#`～`###` 標題、`-`／`*` 與 `1.` 清單、`>` 引述、`**粗體**`、
`*斜體*`、`` `行內程式碼` ``、`---` 分隔線、`[文字](http(s) 網址)`，空一行分段、段內
單一換行保留為換行。表格與圖片不支援。**這個子集寫在 `MARKDOWN_HINT` 一處**，編輯畫面
把那一句直接印給作者看，不另外維護一段說明——說明與實作分成兩份就會漂走。連結只收
`http`／`https`，所以 `javascript:` 進不來。

**內容一律折疊。** 卡片預設只顯示標題、日期與版次，點標題展開；一頁十則規章全文攤開
沒有人讀得下去。員工端把**還沒看過的**那幾則預設展開，因為提醒要看的東西就是要讓它
看得到。

### 員工自助的 MCP（0017、0018）

| name | risk | input | output 重點 |
|---|---|---|---|
| `issue_my_api_token` | NeedsConfirm | `{ label, scope?, rotate_days? }` | 明文只回一次；actor／role 取自 session |
| `list_my_api_tokens` | ReadOnly | `{ include_revoked? }` | 本人核發的 Token＋本人在 MCP 上可用的工具 |
| `revoke_my_api_token` | NeedsConfirm | `{ id }` | 所屬員工寫在 WHERE 條件裡 |

**「用 MCP」不是主管的特權。** `issue_api_token` 是 `AdminOnly`，而且輸入帶
`actor` 與 `actor_role`——就算給了職員權限，那個形狀也允許他指定 Token 代表誰。
自助版本兩個欄位都沒有，代表對象與角色一律取自 session，因此**這把 Token 能做的
事與本人完全相同，不會更多**：職員的 Token 就是職員。

`api_token.owner_employee_id`（0017）是所有權的鍵。`actor` 是顯示名稱、可能同名，
不能拿來判斷歸屬。只有 `issue_my_api_token` 會寫入這一欄，老闆代發的 Token 留
NULL。`revoke_my_api_token` 把員工直接寫進 `UPDATE ... WHERE` 與後續查詢，所以
**別人的 id 是「查無此列」，不是查到之後才拒絕**。

**這一欄同時補上 MCP 缺的那一半身分。** `mcp::token_identity` 原本一律回
`employee_id: None`，理由是「不能從姓名猜員工」——那個理由現在仍然成立，但
`owner_employee_id` 不是猜的，是本人在自己的 session 裡按下按鈕寫進去的。於是
0018 把五個唯讀自助工具對 AI 打開：沒有這一步，「我的 MCP」在全新安裝上會發出
一把什麼都不能呼叫的 Token。老闆代發的 Token 依然沒有 owner，自助操作對它一律
回「尚未連結員工資料」，不猜。

寫入類與薪資明細維持關閉。`punch_clock` 最明確：讓助理替你打卡的工具就是偽造出勤
紀錄的工具；`issue_my_api_token` 若對 AI 開放，Token 就能再生 Token，撤銷也追不上。

`no_self_service_input_can_select_another_employee` 的黑名單擋下了
`issue_my_api_token` 原本的 `name` 欄位——在這個模組裡 `name` 一直是人名。正確的
回應是把欄位改叫 `label`，不是替測試加例外。

### 打卡（0016）

| name | risk | input | output 重點 |
|---|---|---|---|
| `punch_clock` | NeedsConfirm | `{ kind, note? }` | 本人上／下班卡，時間取伺服器時鐘 |
| `list_my_punches` | ReadOnly | `{ month? }` | 本人逐日明細（該月每一天）與月統計 |
| `list_attendance_punches` | ReadOnly | `{ month?, emp_no? }` | 全公司逐人統計；帶 `emp_no` 另附逐日明細 |
| `correct_punch` | NeedsConfirm | `{ emp_no, punched_on, kind, at, note? }` | 補登，`source = manual` |
| `submit_my_punch_correction` | NeedsConfirm | `{ punched_on, kind, at, reason? }` | 本人送出補登申請；公司設定為自動通過時同一交易寫入打卡 |
| `list_my_punch_corrections` | ReadOnly | `{}` | 本人送出過的補登申請與狀態 |
| `list_punch_corrections` | ReadOnly | `{ include_done? }` | 全公司補登申請，逐列回 `can_decide` |
| `decide_punch_correction` | NeedsConfirm | `{ id, approve, note? }` | 核准即在同一交易寫入打卡 |

`attendance` 一直有欄位放月統計，卻沒有任何東西產生它——0016 補上被彙總的那個
原始事件，0059 刪掉那張表。從 0008 到 0059 沒有任何操作、解析器或種子寫過它，
於是 `get_attendance` 每次回空清單、對帳每次回「沒有可對照的資料」、月份選單每次
是空的，而 `run_payroll` 的加班費退路恆為 0。**打卡是這個月唯一的紀錄**，月統計
由打卡現算，月份選單改讀有打卡的月份。

**打卡記錄到秒。** `punched_at` 是 `timestamptz`，本來就有秒；操作層一律以
`%H:%M:%S` 呈現，時數與加班以 `num_seconds()` 計算，遲到判定也在秒的精度上比較
（`09:05:00` 不算、`09:05:01` 算）。`correct_punch` 接受 `HH:MM:SS`，只給到分鐘時
記為秒零——補登是憑記憶填的，秒本來就不知道，而 `source = manual` 已經說明了這
一列是誰填的。

**人資補登的時間欄是純文字，不是 `<input type="time">`。** 那個型別的顯示格式
跟著瀏覽器語系走，會出現 12 小時制的 AM／PM，而 `lang` 屬性只有部分瀏覽器理會；
它也不接受秒，而 `correct_punch` 是接受秒的。文字欄位 24 小時制是定義而不是設定，
且與 API 收的字串格式一模一樣，前端只需在送出前擋掉不合法的值。

**員工自己送出的補登用 `components/TimeField`**（時、分兩個 `<select>`）。兩個
理由在這裡都不成立：`submit_my_punch_correction` 收 `HH:MM`，而本人憑記憶填的
時間本來就沒有秒。選單只給得出 24 小時制的兩位數，所以格式由元件保證，不必再驗。
同一個元件也用在首次設定精靈與基礎設定的 `work_start`／`work_end`——那兩個欄位
的合法值同樣是 `HH:MM`，而後端的 `is_clock_time` 只收兩位數的時與分。

**日期欄則相反，一律 `<input type="date">`。** 它的佔位字樣（Chrome 的
`yyyy／月／dd`）由瀏覽器語系決定、頁面改不動，但那只是佔位字樣：值一律是
ISO 的 `YYYY-MM-DD`，畫面上顯示日期的地方也一律是 ISO，而日期挑選器是這個型別
真正帶來的東西。時間欄的兩個理由（12 小時制、收不到秒）在日期上都不成立。

順帶一提，勞基法第 30 條第 7 項寫的是出勤紀錄「應逐日記載勞工出勤情形**至分鐘
為止**」，也就是到分鐘即已足夠；記到秒是超過而不是符合。程式與畫面因此都不宣稱
這是法規要求，只是照實記錄時鐘給的值。上下班時間（`work_start`／`work_end`）仍是
`HH:MM`，那是公司政策而不是事件。

**`punch_clock` 不收時間欄位。** 打卡記錄的就是「呼叫的那一刻」，能填時間就不是
打卡了。連 `LAYSI_TODAY` 也不移動它：那個環境變數是給示範安裝挪業務日期用的，
而打卡寫的是真實瞬間。漏打或打錯要走補登：本人在打卡頁送出申請（0045），或由
人資直接 `correct_punch`。兩條路寫出來的都是 `source = manual`，畫面上標示
「補登」。整天由別人填的紀錄與本人按下的紀錄不是同一種證據，所以這一欄必須
留下來。

資料庫擋住兩件事：`UNIQUE (employee_id, punched_on, kind)` 讓一天只有一組
上下班；`CHECK (punched_on = (punched_at AT TIME ZONE INTERVAL '+08:00')::date)`
讓「哪一天」與「哪一刻」不會各說各話。用固定 `+08:00` 而不是
`AT TIME ZONE 'Asia/Taipei'`，是因為後者只有 STABLE、不能寫進 CHECK；台灣自
1979 年起沒有日光節約時間，兩者在這裡是同一件事。同一個常數是
`laysi_domain::BUSINESS_UTC_OFFSET_HOURS`。

**彈性工時（0019）與遲到寬限是兩件事。** 寬限（`late_grace_minutes`）原諒遲到，
但下班時間不動；彈性（`flex_window_minutes`）把整天往後移，長度不變。同一個
09:45 到班，在一小時寬限下不算遲到但仍要 18:00 走，在一小時彈性下不算遲到而且要
18:45 走。`WorkSchedule::expected_end()` 因此要吃「幾點到」，`overtime_seconds()`
也跟著改成 `(arrived, left)` 兩個參數——加班是超過**這個人**那天該走的時間，不是
超過一個全公司共用的時間。

早到不會把那一天縮短（`shift_minutes` 夾在 `0..=flex`）：沒有人被要求早到，讓它
生效等於把好意變成義務。留空表示不採彈性工時，也就是所有既有安裝現在的行為。

**上下班時間是公司政策，住在 `company_setting`（`work_start`／`work_end`／
`flex_window_minutes`／`late_grace_minutes`），不在 Rust 裡。** 全新安裝這三格是空的，此時
「這個人遲到了嗎」不是回答「沒有」，而是**不問**：`WorkSchedule::late()` 回
`Option<bool>`，`None` 一路傳到畫面上顯示為「未設定上班時間」。預設 09:00 等於
本系統替別人的公司編了一條規定，再拿它去判定員工。

**打卡推導的加班只算時數，不算金額。** 打卡是「這個人幾點走的」，不是「公司同意他
加班」；金額要有人同意過的時數才算得出來，那是 0057 的加班申請（見下一節）。

順帶修掉一個既有問題：`today()` 原本讀 UTC 日期，於是本地時間 00:00–08:00 之間
建立的任何單據都會記成前一天。現在走 `laysi_domain::business_today()`。

### 加班申請（0057）

加班原本散在三個互不相連的地方：打卡推導出時數，`attendance.overtime_hours` 與
`overtime_pay` 是外部差勤系統對一個月的主張（0059 已把那張沒人寫得進去的表刪掉），
薪資單上的 `overtime` 直接抄後者。只用本系統的公司因此永遠發不出加班費，而且
**沒有任何人核准過任何一次加班**——勞基法第 32 條把雇主的同意當成加班合法的前提。

申請就是那份同意，形狀與休假單相同：牆上時鐘的起訖時刻，時數等於區間長度，
待簽核到有人決定。`worked_on` 由 `from_at` 推導（generated column），所以一張單
屬於哪個月不是另一個可以填成別的值的欄位。

**加班費不存欄位。** 它是時數乘一個有生效日的倍數，`run_payroll` 依加班當日的
`overtime` ruleset 現算，理由與不存扣繳稅額相同：同一個問題有兩個答案就會漂走。
`day_type` 也刻意沒有 CHECK——它的合法值來自有生效日的規則檔，而 CHECK 沒有生效日，
修法多一種日子會讓約束拒絕法律允許的資料。驗證在 `submit_my_overtime_request`，
比對的是加班當日解析出來的那一份規則。

規則檔 `regulation/2026/overtime.toml` 收的是第 24 條：前二小時加給三分之一、
再延長加給三分之二，寫成 `1.34`／`1.67`（三分之一除不盡，**進位**寫，捨去會低於
法定下限）；`monthly_hours = 240` 是月薪換算時薪的除數（三十日乘八小時），不是公司
自己的上下班區間——那一段可能是九小時，因為它含休息時間。上限也在檔案裡：一日
四小時、每月四十六小時（第 32 條）。

三道擋下：結束要晚於開始；**平日的時段不得與上下班時間重疊**（那段時間月薪已經
給付，重疊等於同一段時間付兩次）；當月累計不得超過月上限，拒絕訊息說出還剩多少。
沒有設定上下班時間時不做重疊檢查——沒有區間就沒有東西可以重疊，而加班時數不需要
它就算得出來，這一點與休假相反。

`run_payroll` 只認核准的申請；沒有核准紀錄的加班不計入（0059 之後也沒有第二個
來源可以沿用）。**國定假日與例假不做**：第 39 條與第 40 條
按日加倍發給而不是按小時計費，而系統沒有國定假日日曆，判定不了某一天屬於哪一種。
週六與週日一律當休息日，與休假時數的假設同一套；法遵檢核把這一塊列為未檢查。

### 補登申請（0045）

0016 把 `correct_punch` 給了人資，沒有給員工任何東西。知道自己漏打的人正是唯一
不能記錄它的人，於是每一次漏打都變成一件跑腿的事：跟人資講，人資替他打字。
十五個人的公司裡，這是最常發生的出勤事件。

**申請是另一張表，不是帶狀態欄的打卡。** `attendance_punch` 是工作日的證據，
讀它的每一件事——時數、遲到、加班——都必須讀到一批同樣為真的列。提議的時間在
有權說話的人接受之前不是證據，所以它住在 `punch_correction`；核准時才經由人資
本來就在走的 `record_punch` 路徑寫成一列 `source = 'manual'` 的打卡，
`created_by` 是決定的人。

**核准與寫入在同一個交易。** `Store::punch_in_tx` 是從 `record_punch` 拆出來的
寫入半段，`decide_punch_correction` 與自動通過的 `submit_punch_correction` 都
呼叫它。少了這條，一張申請可以標成已核准而它承諾的那一格是空的，而畫面上沒有
任何地方看得出來。

**一格只能有一張未決的申請。** `UNIQUE (employee_id, punched_on, kind) WHERE
status = 'submitted'` 是部分索引，決定過的列不在裡面，所以同一格的歷史留得住。
少了它，送兩次就留給人資兩個互相矛盾的時間，而核准兩張只會後者蓋掉前者，
且看不出原本要的是哪一個。

**要不要審核是公司設定。** `company_setting.punch_correction_policy` 只有兩個值，
而且存的就是畫面上印的字：`人資審核`（種子值）與 `自動通過`。信任員工自己修的
公司不必把每個月十五張申請送給一個當時也不在場的人；要第二雙眼睛的公司留著它。
它與上下班時間是同一組政策值，所以改得動它的同樣是系統管理員。

**申請人不得決定本人送出的申請。** 與休假、報支同一條規則，鍵是
`employee_id` 不是姓名。`list_punch_corrections` 逐列回 `can_decide` 與
`decide_blocked_reason`，畫面在 `false` 的那一列不畫按鈕、改印原因。

**申請在明細的那一格提出，不在另一張表單。** 表單要人重填一次日期與上下班，
而那兩件事就寫在他正在看的那一列上；漏打、打錯時間、整天忘記三種情況也只有第一種
在表單裡是明顯的。畫面因此把逐日明細的上班與下班兩格做成可按的：空的按下去是補登，
有時間的按下去是調整（帶入原值），待審核的那一格印出申請時間且不能再按——同一格
只能有一張未決的申請，亮著一顆按下去必定被拒的按鈕等於把規則講兩次。

**所以 `list_my_punches` 回該月的每一天，不只是有打卡的那些。** `punch_days` 由
打卡列推導，整天忘記打卡的那一天因此沒有列，而那正是最需要補登的一天；
`month_days` 在它之上補齊空白日，範圍以到職日與今天為界（離職日更早時以離職日
為界），空白日不影響 `summarise` 的任何一個數字。日期範圍不再由申請表單的日期欄
決定，所以畫面改以月份前後翻——少了它，月份一換就補不了上個月最後一天。

**今天那一列不從查詢的月份推導。** `next_kind` 是打卡鈕要顯示上班還是下班，
翻到過去的月份時那個月裡沒有今天，只看查詢結果會回頭要人再打一次上班卡。
月份不是本月時另查今天所屬月份的打卡列。

**`correct_punch` 留著。** 不是每個在薪資冊上的人都有登入帳號——0028 允許一筆
只有姓名與到職日的員工主檔——而那個人一樣會漏打卡。

### 員工自助模組（0009、0015、0016、0020、0045）

| name | risk | input | output 重點 |
|---|---|---|---|
| `get_my_home` | ReadOnly | `{}` | 本人基本資料、年資、本月出勤、特休餘額、送出的單走到哪 |
| `list_my_payslips` | ReadOnly | `{}` | **只有本人的**薪資單 |
| `list_my_leave_requests` | ReadOnly | `{}` | 本人請假單、特休額度／已休／剩餘、`leave_kinds`，以及算時數用的 `schedule` |
| `request_leave` | NeedsConfirm | `{ kind, from_at, to_at, reason? }` | `{ request, schedule }`，編號 `LV{西元年月}-{三碼}`；時數與天數由後端算 |
| `list_my_overtime_requests` | ReadOnly | `{}` | 本人加班單、本月已核准時數與上限、`day_types` |
| `submit_my_overtime_request` | NeedsConfirm | `{ from_at, to_at, day_type?, reason? }` | `{ request }`，編號 `OT{西元年月}-{三碼}`；時數為區間長度 |
| `list_my_expense_claims` | ReadOnly | `{}` | 本人送出的報支與逐關進度 |
| `submit_my_expense_claim` | NeedsConfirm | `{ title, amount, spent_on?, pay_method?, memo? }` | `{ claim }`，編號 `EX{西元年月}-{三碼}` |
| `attach_my_expense_receipt` | NeedsConfirm | `{ claim_no, file_name, content_base64 }` | `{ receipt, receipts }`，收據進 RustFS |

**這個模組的授權不靠 `Risk`，靠輸入形狀。** `list_payslips` 是 `BossOnly`，
但員工本人必須看得到自己的薪資單，而他不是老闆。作法是：**自助操作一律不接受
任何指定對象的參數**，對象由已驗證 session 的 `ctx.employee_id` 決定。沒有欄位可
以填，就沒有辦法填別人的名字；這比在查詢裡加過濾條件更難寫錯。

`ops/selfservice.rs` 的 `no_self_service_input_can_select_another_employee`
測試把十四個操作的 `Input` 屬性集合**逐字釘死**（`request_leave` 只有
`kind`／`from_at`／`to_at`／`reason`，`submit_my_overtime_request` 只有
`from_at`／`to_at`／`day_type`／`reason`——`day_type` 指的是哪一種日子、
合法值由規則庫決定，不是人，`submit_my_expense_claim` 只有
`title`／`amount`／`spent_on`／`pay_method`／`memo`，`punch_clock` 只有
`kind`／`note`，`list_my_punches` 只有 `month`——那是期別不是人——其餘四個是
空集合），另加一份
`employee_id`／`emp_no`／`name`／`actor`／`claimant`／`user_id`／`dept_code`
的黑名單。**加任何一個欄位都會讓測試紅**，因為這是本模組唯一的授權保證。

`submit_my_expense_claim` 與 `submit_expense_claim` 是兩個操作而不是同一個：
後者帶 `claimant`，把它授權給職員等於允許以他人名義報支，所以它留在
`request_payment` 權限裡。自助版本的申請人姓名與部門一律取自 `ctx.employee_id`
對應的員工主檔，另外**不收費用科目也不收收據文件編號**——科目是「會計確認科目」
那一關要決定的事，而職員沒有文件權限，能填進來的編號必然是別人的，回應會連
帶洩漏該文件名稱。單據本身仍是 `expense_claim` 的普通資料列，走同一組
`approval_rule` 關卡，0015 不動任何資料表。

**收據是送位元組，不是指一份既有文件（0020）。** 上一段擋掉的是「指向已經存在
的文件」，不是「附一個檔案」——收據本來就是後者。`attach_my_expense_receipt`
因此收 `file_name` 與 `content_base64`，輸入裡沒有任何東西能指到既有文件。

擋不住的是 `claim_no`，它指的是報支單，而報支單屬於人。所以 0020 為
`expense_claim` 加上 `employee_id`：`claimant` 是顯示名稱、會同名，讀那一側沿用
它還可以接受，寫這一側不行。查詢是
`WHERE claim_no = $1 AND employee_id = $2`，**別人的單號是查無此列**，不是查到
之後才拒絕；會計代建的單 `employee_id` 為 NULL，因此在自助這一側完全看不到。
狀態限 `submitted` 與 `approving`：收據是還沒作成的決定的憑據，決定作完之後
背後的憑證不該再變。

流程是先建單、再附件——反過來的話 `link_ref` 沒有單號可寫。因此上傳失敗時單
已經存在，畫面照實說明哪幾個檔案沒附上，並要求到「我的請款」重附，**不要重送
整張單**；`accepts_receipt` 就是那顆按鈕出不出現的依據。

**檔案內容不進稽核紀錄。** `Registry::dispatch` 在寫 `ai_audit_log` 之前先過
`redact_for_audit`，超過 512 位元組的字串改記長度。判準是值的長度而不是欄位
名稱，所以之後任何帶位元組的操作都自動涵蓋。是哪一個檔案仍然查得到：
`document_version` 存著 BLAKE3，而且那是**落地的位元組**的雜湊，不是送出的。

`ctx.employee_id` 直接取自已驗證 session 所屬的 `user_account.employee_id`，
再以主鍵查 `employee`；不使用姓名比對，也沒有同名時任選一筆的問題。未連結
員工資料的外部會計師帳號會回 `BadInput`，明確說明自助功能需要員工主檔，
不回空清單；空清單容易被讀成「我沒有薪資單」。

自助的 `mcp_tool_grant` **一律 `enabled = false`**：MCP token 沒有
`user_account.employee_id` 連結，而且它代表的系統身分與坐在畫面前的人是兩回事，
預設對 AI 開放沒有道理。
上節的 migration parity 測試同時涵蓋 0008、0009 與 0015。完成 0009 後 Registry
共 **76 個操作**；0012 再加入 9 個主檔與應收應付建立操作，0014 加入作廢與
反向沖銷，0015 加入 `submit_my_expense_claim`，0016 加入四個打卡操作，0017 加入三個自助 MCP 操作，
0020 加入 `attach_my_expense_receipt`，目前共 **96 個**。

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
  **數量為零的項目不出現**：待辦清單上的 0 沒有可做的事。
- `cash_bars`：近 6 個月現金餘額 `{ month, value }`。
- `ai_recent`：`ai_audit_log` 最近 3 筆 `{ what, time, who }`。
- `ledger_started`：有沒有任何一張已過帳傳票。四個 `stats` 全部只計已過帳，
  所以全新安裝時它們是「還沒有資料」而不是「金額為零」，畫面靠這個欄位分辨——
  沒有就改成空狀態並指向記帳，不擺四個 0。

前端的現金水位圖**依最大值決定單位**（元／萬元／億元）而不是固定百萬元：
中小企業的三百八十萬在固定單位下畫成 3.8，讀數的人得自己乘回去。

## REST 契約（laysi-server）

| 方法 | 路徑 | 說明 |
|---|---|---|
| GET | `/healthz` | `{ status, brand: {ok, fingerprint, files}, db: {migration}, rustfs: {ok} }` |
| — | — | 前端也讀這一條（Caddy 與 vite 都要 proxy）：品牌指紋與 RustFS 狀態只有伺服器行程知道，`get_system_health` 從資料庫查不到 |
| POST | `/v1/auth/login` | 帳密登入並設定 session cookie；失敗訊息不區分帳號或密碼 |
| POST | `/v1/auth/logout` | 撤銷目前 session 並清除 cookie |
| GET | `/v1/auth/me` | 目前登入帳號；未登入回 401 |
| POST | `/v1/auth/change-password` | 驗證目前密碼後更新 Argon2id 雜湊並清除 `must_change_password` |
| POST | `/v1/auth/bootstrap` | 安裝完全沒有帳號時建立第一個帳號（角色固定 `admin`）並直接簽發 session |
| GET | `/v1/auth/oauth/start` | 啟用時建立 PKCE 流程並以 302 前往 discovery 的 authorization endpoint |
| GET | `/v1/auth/oauth/callback` | 後端交換授權碼、驗證 id_token、建立 session，再以 302 回 `/` |
| GET | `/v1/meta` | 公司、會計期間、身分、版本、授權、品牌出處字串 |
| GET | `/v1/brand/mark.svg?h=28` | 字標，`Content-Type: image/svg+xml` |
| GET | `/v1/brand/monogram.svg?h=64` | 組合標記 |
| GET | `/v1/ops` | registry 清單：`[{name, risk, summary, enabled, input_schema}]` |
| POST | `/v1/ops/{name}` | body 是 `{ input, prompt }`（兩者皆可省略）；回 `{ ok: true, data }` 或 `{ ok: false, error: {code, message} }` |
| POST | `/mcp` | MCP streamable HTTP，Bearer token |

`/healthz` 與 `/v1/brand/*.svg` 是公開端點。`/v1/meta`、`/v1/ops*` 必須有
有效 session；`/mcp` 必須有自己的 Bearer 憑證。認證路由是操作註冊表唯一例外：
登入若經過 dispatch，就會先被角色權限步驟要求一個尚未取得的身分，形成循環。
`/v1/auth/bootstrap` 同樣未登入即可呼叫，但守門的是「安裝目前沒有任何帳號」，
由資料庫的單列表保證，見上文〈網頁初始化〉。

`/v1/meta` 回傳：

```json
{ "company": "", "tax_id": "", "setup_required": true,
  "period": "2026 / 07", "period_state": "未結帳",
  "actor": "目前使用者", "actor_role": "acct", "employee_id": 1,
  "oauth": { "enabled": true, "provider": "https://accounts.example.com" },
  "version": "0.1.0", "licence": "BSL",
  "brand_build": "<16 碼指紋>" }
```

`setup_required` 只看 `company_setting.company_name`：空字串（包含只有空白）代表
首次設定尚未完成，回 `true`；填入公司名後回 `false`。這只是帶前端進設定精靈的
bootstrap 訊號，不會自行建立任何公司資料或示範資料。

### MCP

`POST /mcp`，JSON-RPC 2.0，實作 `initialize`、`tools/list`、`tools/call`。
protocol version `2025-06-18`。`tools/list` 的 `inputSchema` 直接取
`Operation::input_schema()`。

**`tools/list` 列的是這組 Token 真的叫得動的工具**，三個條件與 `dispatch` 相同：
工具開關開著、通得過風險底線、角色權限有它。只看工具開關的話，一般職員的 Token
會看到全部開著的工具而其中九成必定被拒——讓模型看到、呼叫、被拒絕是白跑一趟，
還會把對話變成跟權限系統吵架，而那正是不列出關閉工具的理由，對角色權限一樣成立。
查詢失敗時列空清單而不是全部：這裡不是強制點（`dispatch` 才是），但不能在資料庫
連不上時反而放寬。風險底線的判斷是 `Risk::allows_role`，`dispatch`、
`/v1/auth/me` 的 `permissions` 與這裡共用同一份，多一份就是多一份會漂走的副本。`tools/call` 走 `Registry::dispatch`，回傳
`{ content: [{ type: "text", text: <JSON 字串> }] }`；被擋下時
`isError: true`。認證：`Authorization: Bearer <api_token 明文>`；伺服器只保存
BLAKE3 雜湊，且 Token 必須是未撤銷、未過期的 `kind = mcp` 資料列。認證先於
JSON-RPC body 解析，未通過一律回 401（撤銷與過期不對外分辨，見上一節）。

對外開放之後，認證與 JSON-RPC 之間還有兩層防護（`crates/laysi-server/src/mcp.rs`）：

- **Origin 驗證**：streamable HTTP 規格要求防 DNS rebinding。有帶
  `Origin` 就要在 `MCP_ALLOWED_ORIGINS`（逗號分隔，預設空）內，沒帶則放行：正常
  的 AI connector 不會帶這個標頭，瀏覽器帶了就必須在白名單裡。
- **per-token 速率限制**：`MCP_RATE_LIMIT_PER_MINUTE`（預設 60，0＝不限制）。
  同一個 `RateLimiter` 也擋 `/v1/auth/bootstrap`（全域一桶）與 `/v1/auth/login`
  （每分鐘 10 次，以**登入名稱**分桶——反向代理後面所有請求同一個來源位址，
  以位址分桶等於讓一個攻擊者把全公司鎖在門外）
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
以文字重繪品牌名**。

**前端同樣不得自帶任何法規數字。** 稅率、級距、耐用年數一律由後端從 ruleset
帶下來，這條規則與「不得自帶 logo」同級。畫面上要即時試算稅額時，用後端
回傳的費率，不要在 TS 裡寫 `0.05`。金額運算用 `client.ts` 的
`multiplyToDollars` / `addDecimals`（BigInt），不經過 `number`。

### 示範資料：`laysi-admin seed-demo`

產品出廠是空的，這條指令是唯一的例外，而且必須手動執行。已經有傳票的安裝會被
拒絕，除非加 `--force`。

- **一律走 `Registry::dispatch`**，不直接寫 SQL。這樣種出來的資料會自己成立：
  應收明細對得上 1131、薪資對得上 6111、資產負債表平衡，且每一筆都有稽核。
- **傳票必須依日期順序建立。** `doc_sequence` 每個 prefix 只有一列，期別一變
  就把計數器歸 1，所以先開 6 月再回頭開 5 月會撞號。`demo.rs` 的 `Step` 先收集
  再 `sort_by_key(date)` 就是為了這件事——這也是**回頭補開上期傳票會撞號**的
  同一個限制。
- **有三處不是操作**：發票字軌、預算版本與歷史打卡。前兩者沒有任何操作可以
  建立，全新安裝因此無法開發票、也無法設預算，補了操作之後就該刪掉；第三者
  不一樣，`punch_clock` **刻意**只記錄呼叫當下的時刻，所以「補一個月的歷史打卡」
  本來就不該有操作，那段 SQL 會一直留著並照實說明它不是打出來的。
- 金額是編造的公司資料；**沒有任何法規數字**，投保薪資直接沿用各人薪資。

### Header 屬於當前分段，不是全站共用

`FINANCE_NAV`／`HR_NAV`／`ME_NAV` 是三個系統，不是同一個系統的三種檢視，所以
header 不能共用；`SYSTEM_NAV` 是第四棵樹但不是分頁（見下）。header 先標明現在在
哪個系統，再只帶那個系統自己的脈絡與動作：

| 分段 | 脈絡 | 動作 |
| --- | --- | --- |
| 財務 | `會計期間` + `period_state`（來自 `/v1/meta`） | ＋ 建立傳票 |
| 人資 | 無 | 無 |
| 我的 | 登入者姓名 | 無 |
| 系統 | 無 | 無 |

**系統不是分頁，是左下角帳號列旁邊的一個按鈕。** 它跟財務、人資、我的不是同一種
東西：那三個是每天要待著的地方，系統是偶爾去一次的地方，放成第四個分頁會讓它看
起來跟前三個一樣常用。按鈕只在角色開得起其中至少一個畫面時才出現，多數角色看不到。

**這四個畫面原本掛在財務樹底下。** 基礎設定、角色與核准、MCP 與 AI、安全與營運
四個畫面原本掛在 `FINANCE_NAV` 最底下，理由只是財務那棵樹最長，不是它們屬於
財務。誰可以簽核什麼、AI 能呼叫哪些工具、品牌簽章有沒有通過，講的是這套系統
本身；會計結帳時不會打開它們，而會打開它們的人當下並不在做會計。它們背後的
權限（`manage_access`、`manage_settings`）也是管理權限，所以多數角色根本不會
看到這個分頁。

**帳號管理是第五個畫面，從「角色與核准」拆出來。** 建立帳號原本是那個畫面上的一個
區塊，夾在權限矩陣與簽核流程之間。改矩陣是設定角色能做什麼，發帳號是決定誰是使用
者，兩件事的頻率與對象都不同；而且已建立的帳號清單原本無處可放，只能靠
`laysi-admin list-users` 從 shell 看。拆出來之後 `list_user_accounts` 有畫面可落，
「角色與核准」也回到只講角色與簽核。

**建立與編輯都在抽屜裡，和員工名冊同一套。** 這個畫面第一版把表單常駐在清單下方，
建立與編輯共用它：按下某一列的「編輯」要捲到頁尾才看得到表單，取消編輯的按鈕寫成
「取消，改為建立新帳號」，而刪除是表單底下的第二排按鈕。清單與表單是兩個焦點，
硬擺在同一個捲動平面上就會互相搶。改成抽屜後清單不動，建立與編輯是同一個抽屜的兩種
狀態；刪除放進抽屜頁腳，按下去整個抽屜換成一句後果與「確認刪除 {帳號}」，
確認與被刪的對象因此在同一個視野裡。

**忘記密碼由管理員重新發一組臨時密碼（0029）。** 這件事原本只有 shell 上的
`laysi-admin reset-password`：密碼只顯示一次、資料庫只有 Argon2 雜湊，弄丟就沒有別的
出口，而會弄丟的人正是最不可能有伺服器 shell 的人。`reset_user_password` 把同一段邏輯
搬到畫面上，並多做一件 CLI 沒做的事——**撤銷該帳號目前所有登入工作階段**：換密碼卻讓
舊密碼開的 session 繼續活著，等於沒換。

兩個拒絕：`password_hash IS NULL` 的帳號只用外部身分登入，發一組密碼給它等於把
SSO 帳號變成密碼帳號，繞過身分提供者的多因素；自己的帳號則是畫面上不給按——重設會
把自己登出，而密碼視窗會隨著跳回登入頁一起消失，何況真的忘記自己的密碼時根本進不
到這個畫面。

**初始密碼用一個必須按過才會消失的視窗。** 它只在建立那一次的回應裡出現，資料庫只留
Argon2 雜湊，錯過了就只能從 shell 跑 `laysi-admin reset-password`。原本它是清單上方的
一段文字，捲動或下一個動作都會讓它離開視野。改成置中視窗後，點背景不關、標題列也沒有
×，唯一的出口是「我已交付」；旁邊是「複製密碼」，寫進剪貼簿後回一則通知，
`navigator.clipboard` 在非安全來源會被瀏覽器擋下，那時通知改說請手動選取。

**暫時密碼是 8 個小寫英數字元**，字母表排除 `0 o 1 l i`。原本是 `tmp-` 加 36 個
十六進位字元共 40 個字：那是給複製貼上用的長度，而實際發生的事是人資打電話唸給
員工聽。約 39 bits 的熵配上「首次登入強制更換」與 Argon2id 的成本已經足夠，
真正的防線是那兩件事，不是長度。取樣用拒絕採樣而不是取餘數，否則前幾個符號會
比其餘的常出現。

**員工連結必須能事後補。** 員工主檔帶底薪，所以讀寫它的是人資權限而不是管理員
權限；全新安裝上管理員先發帳號、人資之後才建員工名冊，`create_user_account` 那一刻
根本還沒有員工編號可填。0026 的 `update_user_account` 因此存在：它改得動顯示名稱、
電子郵件、啟用狀態與員工連結，改不動 `role_code` 與密碼——角色走 `change_user_role`，
密碼只有本人改得動；而停用最後一個啟用中的管理員帳號會被擋下，否則沒有人打得開這個
畫面。0027 的 `delete_user_account` 擋的是同兩件事再加一件：刪掉最後一個
帳號會讓整座安裝沒有人能登入，而 `install_bootstrap` 已被認領、網頁初始化不會重開。

**薪資不是「人存在」的前提（0028）。** 上面那段講的補救原本補不完：0008 給
`employee.base_salary` 寫了 `CHECK (base_salary > 0)`，於是建人一定要先有薪資，而
薪資是人資的資料、帳號是管理員的工作，全新安裝的第一天沒有人能起頭。每一環單看都
成立，串起來讓最常見的第一件事做不到。

0028 把該 CHECK 放寬成 `>= 0`，0 表示尚未設定。真正該擋的地方改在計薪：
`run_payroll` 遇到 0 底薪回「尚未設定底薪」而不是發出 0 元薪資單——**零元薪資單看
起來像算出來的，錯誤訊息不會**。

**建帳號一定會有一筆員工主檔，沒有開關。** `link_employee()` 的規則只有一條：
既有的員工編號就連結，指不到人（含留空）就建一筆只有姓名與到職日的員工主檔，編號
留空時由 `next_employee_no()` 依 `E###` 系列配號（不是這個形狀的編號不參與計算，
自訂編號的名冊不會被重新編過），部門與金額仍留給人資。原本這件事是一個
`new_employee` 旗標，預設關閉：關著建出來的帳號打不了卡、請不了假、報不了帳，
而那是這個人隔天上班就要做的事，等於預設把帳號建壞。旗標拿掉之後
「查無員工編號」這個錯誤也一併消失——打錯編號會建出一筆新的人，改掉編號即可，
比擋下來要求先去另一個畫面建人短。

`update_user_account` 的 `employee_no` 是三態：不帶這個欄位不動連結、空字串解除
連結、給編號就連結或建立。連錯人時改成正確的編號即可，畫面上不提供解除。

- **會計期間與「＋ 建立傳票」只出現在財務。** 員工在看自己的薪資單時，會計
  期間不是他的脈絡，而建立傳票這個呼叫後端本來就會拒絕。
- **人資刻意留空。** 它的月份只會重複今天的日期，而每個人資畫面各自選期別；
  把月份放進 header 會被讀成一個系統其實沒有的篩選條件。寧可空著。
- 分段名與脈絡之間的分隔線用 `:has(+ .header__meta)` 畫，沒有脈絡就不畫，
  避免出現後面沒有東西的分隔線。
- 新增分段或新增 header 元素時，先問「這件事對這個系統是不是真的成立」，
  不成立就不要放。

### 數字用自己的字體

`--font-display` 是 Cormorant Garamond，一套筆畫極細的展示用襯線體。它拿來排標題
沒問題，拿來排 31px 的金額與 40px 的時鐘就很難認——而這是一套財務系統，數字就是
內容。`.num` 因此有自己的 `--font-numeric`（Source Serif 4），並明確指定
`lining-nums tabular-nums`，讓每一位數等寬、等高、成欄對齊。

**`.num` 的規則必須排在所有元件規則之後。** `.stat-card__value` 之類的元件也設
`font-family`，兩者都是單一 class、權重相同，勝負只由檔案順序決定。這與先前
`.entry-input` 的問題同一類。

### 頁面說明只寫使用者需要知道的事

`page-lede` 一度全部寫成「這頁有什麼」——那是規格註記，不是給使用者看的字。
判準改成：**寫使用者需要知道、而畫面上看不到的事**；只是複述頁面內容的就刪掉，
標題已經說過了。

- 留下的例如：「已過帳傳票不能修改，要更正請開立反向分錄或作廢」「到期日加註底線
  者已逾票期」「匯入一律先執行預覽」——都是能據以行動的規則或圖例。
- 刪掉的例如：「本月出勤與加班統計，以及待簽核的休假單」「各期實發金額、代扣項目
  與公司負擔部分」——畫面本來就長那樣。
- 也刪掉設計說明：「畫面與 AI 使用同一份操作註冊表，並非另行實作的介面」是架構
  文件的內容，不是 MCP 設定頁的內容。

**解釋句要掛在它解釋的那件事上，不是掛在一個相關的數字上。** 損益表底下那句
「開票傳票尚未過帳，銷貨收入尚未進入總帳」原本的條件是「收入為 0」，於是全新安裝的
第一天必定印出來——系統裡連一張開票傳票都沒有，畫面卻斷言有一張還沒過帳。收入為 0
只能推出「本期尚無已過帳的收入」，未過帳傳票有幾張是另一個問題，而那個問題頁面最上方
已經有一行在回答。同一頁不講第二次，更不能為了講第二次而編一個前提。

### 網址就是頁面狀態

沒有引入 router 套件：`nav.ts` 的 `SCREEN_PATH` 把每個 `ScreenId` 對到一個路徑，
`screenForPath` 是它的反查，`App.tsx` 用 History API 推入與監聽 `popstate`。
`web/Caddyfile` 早就有 `try_files {path} /index.html`，Vite dev server 預設也做
history fallback，所以重新整理與直接貼網址都會落在同一個畫面。

- 路徑帶分段前綴（`/me/clock`、`/finance/ledger`、`/hr/attendance`、
  `/system/security`），因為分段本來就是四個不同的系統。
- **`section` 不再是獨立 state，而是由 `screen` 推導**（`sectionOf`）。兩個 state
  可能不同步，一個不會。
- 進站時網址指到的畫面要先過 `screenAllowed`（角色權限 ＋ 自助段需要員工主檔）才
  採用，否則換成該身分的落地畫面並 `replaceState`。貼一個沒有權限的網址不會讓畫面
  停在會被後端拒絕的狀態。
- **只有一條換畫面的路：`go()`。** 側邊欄一度直接 `setScreen`，結果網址只有進站
  那一次會寫，看起來像「只有 `/me/clock` 有路由」。任何新的導覽點都要走 `go()`，
  不要自己 `setScreen`。
- `popstate` 的處理函式掛在 `[]` 相依的 effect 上，因此不能直接閉包住
  `permissions`／`landing`，會停在第一次 render 的值。判斷寫在一個每次 render
  更新的 ref 裡。
- **對不到任何畫面的網址要說「找不到這一頁」，不是靜靜換一頁**。原本
  `/me/payslips` 這種猜出來的路徑會直接落到落地畫面、網址也被改掉，書籤失效或
  連結改名時使用者只會覺得「我點的東西不見了」。`unknownPath()` 把三種情況分開：
  對得到又有權限就開；對得到但沒有權限仍換成落地畫面（權限會變，不是壞掉的網址）；
  對不到任何畫面則保留網址並顯示找不到這一頁與一顆回到落地畫面的按鈕。

### 時間戳一律換算成營業時區再顯示

後端把 `timestamptz` 原樣回成 UTC 的 RFC3339，換算是顯示層的事：`api/client.ts`
的 `businessTime()` 給「幾月幾日幾點幾分」，`businessDate()` 給日期。**取日期不能
切字串**——`created_at.slice(0, 10)` 在本地 00:00 到 08:00 之間會少一天，而同一時刻
用 `businessTime()` 印出來的登入時間、稽核時間、打卡時間全都是對的，於是同一頁上
兩種時間互相矛盾。後端的 `today()` 早就改用 `laysi_domain::business_today()`；
前端漏掉的是同一件事的另一半。

**「現在幾點」也是同一件事**：畫面上會走動的時鐘與各種預設日期原本讀
`new Date().getHours()` 與 `getFullYear()`，也就是**瀏覽器的時鐘與瀏覽器的時區**，
而記下來的時間是**伺服器的時鐘、固定換算成 +08:00**。兩個來源之間沒有任何一處
互相比對，所以瀏覽器的時區不是 +08:00（容器、遠端桌面、TZ 沒設好的工作階段）時，
打卡卡片會用一種時間往前數、記錄另一種時間，而畫面上沒有任何跡象。差幾小時
就整整差幾小時，拿去算遲到、加班與出勤全部是錯的。

正解是把第二個來源拿掉，不是加一段警告：`callOp`、`getMeta` 與 `/v1/auth/me` 會把
回應的 HTTP `Date` 標頭記成 `serverSkewMs`，`serverNow()` 因此回伺服器的時刻，
`businessClock()` 與 `businessToday()` 再把它換算到 +08:00。**讀到的那個數字就是
按下去會被記下來的那個數字**。標頭只到秒，所以差距小於兩秒時不更新，時鐘才不會
每次請求就跳一秒。用它的有打卡卡片的時鐘、記帳抽屜與請款申請的日期預設，以及
`docNumberHint` 的單據編號範例。

伺服器自己的時鐘走錯時，系統裡沒有任何東西比對得出來——那是主機要修的事。

### 記帳時區是設定，不是常數

`+08:00` 原本同時寫死在三個地方：`laysi_domain`、`api/client.ts`，以及
`attendance_punch` 與 `punch_correction` 的 CHECK。其中兩個裝機的人看不到，所以
跑在別的時區的安裝沒有辦法說出這件事，也沒有辦法發現自己說不出來。

現在住在 `company_setting.business_utc_offset`，值就是 PostgreSQL 在
`AT TIME ZONE INTERVAL` 收的那串文字，設定、兩條 CHECK 與畫面因此讀同一個字串。
`laysi_domain::business_time`／`business_today` 改成**收** offset（與計算子收
`&Ruleset` 同一個道理：沒有「當前時區」，只有「這次呼叫用的時區」），
`laysi_ops::business_offset()` 持有行程層的那一份，啟動時由
`load_business_offset` 讀出來，`OpCtx.offset` 帶著它走完一次呼叫——它就在
`today` 旁邊，因為 `today` 正是用它推導的。前端由 `/v1/meta` 的 `utc_offset`
設定 `client.ts` 的偏移量，不再自帶常數。

**改時區不是顯示層的事**。`attendance_punch.punched_on` 與
`punch_correction.punched_on` 是**由 `punched_at` 推導後存下來的日期**，而且是
`UNIQUE (employee_id, punched_on, kind)` 的一部分：+08:00 的 23:30 那筆打卡，在
+09:00 底下屬於另一天，於是同時違反 CHECK 並換到另一個鍵。`set_company_timezone`
因此在同一個交易裡改寫兩條 CHECK 再寫設定，並在兩張表任一有資料時拒絕——時區在
還沒有人打卡之前就該定案。設定列種成 `editable = false`，`update_company_setting`
因此碰不到它：只寫文字不改 CHECK，會讓資料庫檢查一個偏移量而程式用另一個。

offset 會被字串插進 DDL（bind 參數帶不了識別字與字面量），所以
`parse_business_offset` 只收六個字元的 `±HH:MM`，`set_business_timezone` 進去
之後再解析一次，不信任呼叫端解過了。

伺服器時鐘走錯**不是時區問題**，改時區也修不掉：錯的是那個瞬間本身，換一個
偏移量只是換一個一樣錯的顯示，而存進去的 `timestamptz` 照樣是錯的。

### 落地在「我的」，不是「財務」

`SECTION_ORDER` 以 `me` 開頭，所以分頁順序與落地畫面都是我的，而 `ME_NAV` 的第一
個項目是打卡——打卡是全公司每人每天用兩次的操作，會計每天用的畫面再多也只有一個
人在用。

打卡因此有兩個入口：專屬畫面，以及**我的分段 header 上的常駐按鈕**，和財務段的
「＋ 建立傳票」同一個位置。header 那顆按鈕直接呼叫 `punch_clock`，並顯示今天已打
的時間；今天打完了就變成回到打卡畫面的連結。

**打卡卡片是直向、置中、限寬的。** 一開始是橫向 flex（時鐘｜狀態｜按鈕），狀態
那格 `flex: 1` 會把按鈕推到最右，螢幕越寬推得越遠——最常用的操作被擠到最不容易
看到的角落。改成單欄之後閱讀順序是時鐘 → 按鈕 → 今日狀態，按鈕就在視線落點的
正下方，並以 `--punch-width` 限寬置中，不隨螢幕變寬而散開。

**只有一顆按鈕，不是兩顆。** 上下班的順序由資料庫強制（沒有上班卡不能打下班
卡），所以兩顆按鈕裡永遠有一顆是死的。改成一顆顯示「該打哪一種」的按鈕，兩邊
都做完則換成「今日已完成」。

備註欄原本是畫面下方另一個區塊，標題寫「下一次打卡的備註」——要先往下捲、填字、
再捲回上面按按鈕。現在它是卡片裡按鈕上方的一行「加上說明」，展開就是輸入框，
填完往下按就是那一顆。

### 響應式：桌機版是定義，小螢幕只做減法

三個斷點，全部寫在 `classical.css` 末尾的 Responsive 區塊，不散落在各元件：

| 斷點 | 改變 |
| --- | --- |
| `1180px` | 帳齡五格 `stat-grid--five` 收成三欄 |
| `900px` | 側邊欄離開版面，改為覆蓋式抽屜；所有雙欄 grid 收成單欄；表格啟用橫向捲動 |
| `560px` | 手機：統計卡單欄、觸控尺寸、欄位字級 16px、分錄列重排 |

規則：

- **媒體查詢的 px 是本檔唯一容許的裸數值**，因為 `@media` 讀不到 CSS 變數。
  三個數字集中在該區塊的註解裡，其餘尺寸（`--sidebar-drawer-width`、
  `--tap-target`、`--table-min-*`）仍然是 `:root` 變數。
- **小螢幕只移除欄位與留白，不新增元件、不換一套導覽。** 側邊欄在 900px 以下
  只換 `position` 與 `transform`，內容與四棵導覽樹
  完全相同；不得為手機另寫一份導覽。
- **每個 `<table class="table">` 都必須包在 `<div class="table-scroll">` 裡。**
  分錄列壓不進手機寬度，所以表格在自己的盒子裡橫向捲動，頁面本身永遠不橫捲。
  最小寬度依欄數由 `:has(th:nth-child(n))` 分三級。新增表格時一併加外層 div。
- 欄位字級在 560px 以下是 `--text-field-touch`（16px）：iOS 在此之下會在聚焦時
  自動放大整頁。
- `.shell` 用 `100dvh`（`100vh` 為 fallback），否則最後一列會被手機網址列蓋住。
