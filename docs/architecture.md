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
3. **專有授權防盜賣** — 授權聲明寫在 `LICENSE.txt` 與圖示 SVG 裡。
4. **Docker Compose 自架**。

### 不做加值中心

開立統一發票要向財政部申請字軌、接 MIG 介面、承擔上傳成功與否的責任，那是
加值服務中心的業務。這套系統不做，未來也不做，`einvoice`、
`einvoice_track` 兩張表與 `list_einvoices`／`issue_einvoice`／
`retry_einvoice_upload` 三個操作因此整組移除。留著一個永遠停在 `queued` 的上傳
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

**權威是 `reg_ruleset` 資料表**，不是檔案。伺服器啟動時讀出整張表交給
`laysi_regulation::install()`，計算子仍然是同步純函式、`resolve()` 仍然回
`&'static Ruleset`，所以 `cargo test` 不需要資料庫。`save_regulation` 寫完一版
之後重讀重裝，修法**不必發版也不必重新啟動**。

`backend/regulation/<年度>/<domain>.toml` 從此只是**出廠預設值**：`seed()` 由
`include_str!` 編譯期嵌入，而 `Store::seed_regulations()` **只寫還沒有任何版本的
領域**。逐列補寫會讓刪掉的舊版在下一次重新載入時自己回來，而「刪除」按下去必須
是真的刪掉。新的年度由畫面新增版本，或由 `refresh_regulations` 送來。

**判斷逐領域做，不看整張表有沒有資料**：`api` 與 `mcp` 同時對一個空資料庫開機，
後到的那個會看到前一個寫到一半的表。看整張表就會跳過，把那一半裝進記憶體，另一半的
計算一律回「查無適用的法規版本」，直到下一次重新載入——示範站每晚重建，撞上的那一天
整天都是如此。逐領域判斷時，對方還沒提交的那一筆會讓這一邊在 `reg_update`
的鍵上等它提交，兩邊讀回來都是完整的一套。`laysi-store` 的
`a_partly_seeded_table_is_completed` 守這一條：它直接擺出後到的那個看到的表，不靠兩個
連線搶時間——同一個測試程序裡的兩個連線池時序太整齊，看整張表的寫法跑二十輪也撞不上。

規則檔格式（**TOML 不是 YAML**）：

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
| `owner_insurance` | `health_employer_share=0` `health_employee_share=1.00` `health_government_share=0` `pension_employer_rate=0` |
| `leave_pay` | 每個假別一節：`ratio`（工資給付比例）；病假另有 `yearly_days=30` `beyond_ratio=0`，生理假 `separate_days=3`，產假 `min_service_months=6` `short_service_ratio=0.5`。依勞工請假規則、性平法第 14 條、勞基法第 50 條 |
| `health_dependents` | `max_counted=3`（員工健保自付額計入的眷口數上限，整數）。依健保法第 18 條第 2 項，自 2013-01-01 起 |
| `partial_month` | `labor_days_per_month=30` `pension_days_per_month=30`（到職與離職當月按日計時一個月算幾日，28～31 的整數）。依勞保條例施行細則第 28-1 條、勞退條例施行細則第 22 條，自 2005-07-01 起 |

驗證基準（**必須寫成單元測試**，數值取自原型的 `ins` / `pay` 區塊）：

- 勞保級距 45,800、非工會身分 → 公司負擔 `45800 × 0.11 × 0.70 = 3527`（四捨五入）
- 健保級距 53,000 → 公司負擔 `53000 × 0.0517 × 0.60 × 1.61 = 2647`
- 健保級距 53,000 → 員工自付 `53000 × 0.0517 × 0.30 = 822`（本人一口；每位在保眷屬再加一口，最多計三口，兩名眷屬為 `822 × 3 = 2466`）
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
/// 員工自付與政府補助逐口先取整再乘 1 + min(眷口, 上限)；雇主負擔仍乘平均眷口數。
pub fn health_insurance(rs: &Ruleset, dependants_rs: &Ruleset, insured_salary: Money,
                        covered_dependants: u32) -> Result<Contribution, RegulationError>;
pub fn dependants_charged(rs: &Ruleset, covered: u32) -> Result<u32, RegulationError>;
pub fn pension(rs: &Ruleset, contribution_wage: Money) -> Result<Money, RegulationError>;
pub fn vat(rs: &Ruleset, net_amount: Money) -> Result<Money, RegulationError>;

/// 負責人身分：費率仍取自 health_insurance，只有分擔比率取自 owner_insurance。
pub fn health_insurance_owner(health_rs: &Ruleset, owner_rs: &Ruleset, dependants_rs: &Ruleset,
                              insured_salary: Money, covered_dependants: u32)
    -> Result<Contribution, RegulationError>;
pub fn pension_owner(owner_rs: &Ruleset, contribution_wage: Money) -> Result<Money, RegulationError>;

/// basis 是 payload 鍵名（monthly／hourly），不認得的鍵回 MissingField 而不是回退。
pub fn minimum_wage_shortfall(rs: &Ruleset, wage: Money, basis: &str)
    -> Result<Option<Money>, RegulationError>;

/// 平日每小時工資額。月薪除以 monthly_hours；時薪制不經過這裡。
pub fn ordinary_hourly_wage(rs: &Ruleset, monthly_salary: Money) -> Result<Money, RegulationError>;
/// 第二個參數是平日每小時工資額，不是月薪。
pub fn overtime_pay(rs: &Ruleset, hourly_wage: Money, day_type: &str, hours: Money)
    -> Result<Money, RegulationError>;
```

### 負責人投保是獨立 domain，不是 `health_insurance` 的一個欄位

費率沒有變——健保 5.17% 對誰都一樣，所以它留在 `health_insurance`；換掉的是分擔
比率。把 `owner` 節點塞進 `health_insurance` 的 payload 會逼出一個新版本，而
`resolve()` 以 `effective_from` 取最大值，同一天生效的兩版誰勝出由資料庫回傳順序
決定——那是沒有答案的。獨立 domain 沒有這個問題，法源也真的是另一條（健保法第 27 條
第 1 款第 1 目、勞退條例第 7 條）。

**勞保沒有負責人節點**：勞保條例第 15 條第 1 款對「實際從事勞動之雇主」與受僱者是
同一組 70／20／10，抄一份到這裡只會多一個會漂走的副本。
`premiums_on` 因此只換健保與勞退兩項。

### 健保自付額依眷口數計，上限是另一個 domain

員工自付本人一口，另加每位在保眷屬一口，超過上限以上限計（健保法第 18 條第 2 項）；
雇主負擔仍乘 `avg_dependents`，與這位員工實際有幾位眷屬無關——那是法律的分攤方式。
政府補助與員工自付同一組口數。負責人身分同樣逐口計費，全額自付。

**上限是 `health_dependents` 的 `max_counted`，不是 `health_insurance` 的欄位**：理由與
`owner_insurance` 相同，塞進去要一個同日生效的新版本。`effective_from` 是 2013-01-01
（現行條文施行日）而不是出廠年度，補算較早期間時不會因為找不到上限而失敗。
`premiums_on` 不論眷口數多少都解析這個 domain，找不到就回錯誤，不當成 0 口；
`max_counted` 不是整數（`2.5`、`-1`）一樣回錯誤，`3.0` 讀成 3。前一條由
`crates/laysi-server/tests/payroll_dependant_cap.rs` 跑一次真的 `run_payroll` 證明：法規快取是
行程內的全域狀態，拿掉一個 domain 會波及同一個測試執行檔裡的其他測試，所以它單獨一個檔案。

**逐口先取整再加總**：健保署逐口計費，`38200` 級距三名眷屬是 `592 × 4 = 2368`，先乘再
取整會得到 2370。

**在保眷口數看期末日**：保費按月計，由月底在保的單位收取。`covered_dependants` 數
`health_enrolled_from <= 期末日` 且退保日為空或 `>= 期末日` 的眷屬（退保日是最後在保日，
當天仍在保），所以眷屬月中退保的那個月不收、月中加保的那個月收。眷屬掛在員工的健保底下，
**員工期末日不在職就一位都不計**（`left_on` 早於期末日，或到職日在期末日之後），名單上的
日期不論——月中離職的員工，眷屬那幾口跟著員工走，員工本人那一口也一樣（見下一段）。
**勞健保頁以 `as_of` 所在月份的月底計**，費率也在那一天解析，與該月薪資單同一天：以今天
判斷的話，月中才加保的眷屬在薪資單上計入、在勞健保頁上不計，兩邊的健保自付額就不相符。
回應的 `as_of` 是那個月底，畫面印的「適用」日期也是它，同一頁的試算
也以那一天呼叫 `compute_insurance`。

### 到職與離職當月：勞保與勞退按日計，健保看月底

三項保費在到職、離職那個月各算各的，`Coverage` 把三件事一起帶進 `premiums_on`：

- **勞保**：加保當日起算到退保當日，每月以 `partial_month.labor_days_per_month`（30）日計
  （勞保條例施行細則第 28-1 條）。10/10 到職是 `30 − 10 + 1 = 21` 日，31 日到職與 30 日到職
  同樣 1 日，2/28 到職 3 日；10/15 離職 15 日，做到月底（含 2 月底）是整月。同月到職又離職
  是兩端相減加一。
- **勞退**：同一套算法，日數讀 `pension_days_per_month`（勞退條例施行細則第 22 條）。
- **健保**：投保當月繳全月、退保當月免繳（健保法第 30 條第 2 項），等於月底在保的那個單位收
  整月。月中到職收整月，月中離職整月不收，眷屬跟著不計；做到月底最後一天算月底在保。

加保日與退保日依法等於到職日與離職日，所以由 `hired_on`／`left_on` 推導，不另設欄位。
**按日計的部分在取整之前乘**：每一個分擔比例各自算 `月投保薪資 × 費率 × 比例 × 日數 ÷ 30`
再四捨五入，不是把整月取整之後再折算。日數只在 `laysi_regulation::month_part` 算一次，
30 不在 Rust 裡；一個月短於 28 或長於 31 日的值在 `month_part` 回錯誤。

**薪資單存下投保期間**（`payslip.insured_from`／`insured_to`，本期在本公司投保的第一天與
最後一天）：`list_payslips` 重算要知道日數與健保收不收，而離職日事後可能更正——與眷口數同一個
理由，存事實、重算規則。這兩欄出現之前算的薪資單是 NULL，當時一律收整月，重算也照整月。
薪資單的備註寫出「勞保與勞退按 N 日計」與「健保退保當月免繳」；勞健保頁逐列回
`labor_days`／`pension_days`（整月為 null）與 `health_charged`，當月有人投保不足月時多一則
說明，日數讀自 ruleset。**那則說明的人數只數不足月計費的人**：1 日到職或做到月底最後一天的人
收整月，不算在內——算進去的話，後半句「按日計、退保當月免繳」對他們不成立。
**當月一天都沒投保的人**（到職日在下個月，或離職日早於月初而狀態還沒改成已離職）是 0 日，
畫面標「本月未投保」，不算進那則說明的人數，也不寫「退保當月免繳」。
反過來，**本月才離職的人，狀態改成已離職了也照列**：薪資單照收那幾天的勞保與勞退，勞健保頁
少了這一列，兩邊的合計就不相符；離職日在月初之前的才不列。

**薪資作業記下全部六個版本**：`payroll_run` 原本記勞保、健保、勞退、扣繳四個，
`dependents_version` 與 `partial_month_version` 補上眷口數上限與日數兩條規則——金額由哪一版
算出要查得到，回溯修正時才知道哪幾期要重算。兩欄出現之前的薪資作業是 NULL。薪資作業畫面與
員工的「我的薪資單」都列出這兩個版本，NULL 就不列。

**薪資單存下計入的口數**（`payslip.health_dependents`，已套用上限、不含本人）：
`list_payslips` 拿存下的級距重算保費再比對，而眷屬名單之後還會改——事後補登一位加保日
較早的眷屬，會讓每一張舊薪資單的重算都對不上，雖然每一張都照當時的名單扣了。口數因此
與級距一起存，重算用存下的口數，比對的是規則。

規則以 `include_str!` 在編譯期嵌入，另外由
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

理由是生效日：設定表只有一個當前值，`reg_ruleset` 則有有效期間。把稅率放進設定表，
初期行為一樣，差別到修法時才出現：舊期間的申報書會跟著新稅率一起變，而這正是「法規
更新空間」這個需求要避免的情況。兩者都在資料庫裡，界線在**資料模型**不在存放位置。

### 工作日曆

`work_calendar` 只存**與星期不同**的日期。預設是週一至週五工作日、週六休息日、
週日例假，而政府辦公日曆表增加的資訊剛好就是例外：國定假日、調整放假、補行上班，
一年約二十列。`WorkSchedule::load` 因此整張表帶著走，之後每一次問日期都不再查庫。

五種日別：`workday`／`rest_day`／`regular_leave`／`holiday`／`natural_disaster`。
三個消費端：

- `leave_seconds()`：非工作日不計時數。跨到春節的休假原本每一天都扣抵特休。
- `overtime_seconds()`：非工作日不扣「一天的長度」，出勤第一秒就是加班。
- `overtime_day_type()`：決定 `overtime.toml` 用哪一種日別計價。

**不內建任何日期**，日期年年不同，內建一份等於印一份沒人看得出錯在哪的假日曆。
`refresh_work_calendar` 向政府資料開放平臺（資料集 14718）取：先讀 metadata、依
`resourceDescription` 的民國年前綴挑檔案，再抓 CSV。**取最後一筆符合的**——更正版是
附加在原檔之後而不是取代它；跳過 `_Google行事曆專用`（欄位不同）與非 UTF-8 的檔案。
欄位（`西元日期`／`是否放假`／`備註`）依欄名找而不是依位置，因為年度之間換過順序，
而且檔案帶 BOM，比對欄名前要去掉。網址是 Rust 常數不收參數，所以沒有 SSRF 面。

**第一次啟動自動抓一次**（`ops::calendar::seed`）：表是空的才抓，抓當年與次年——
十二月裝機的公司否則會在一月開年時面對一張空日曆。整段是 `tokio::spawn` 出去的，
**不擋啟動也不讓啟動失敗**：新安裝不該等一個政府網站才起得來，連不出去的安裝也不該
因此起不來。失敗只寫一行 log，而表仍然空著，所以下一次啟動會再試——第一天沒有對外
連線的安裝不會就此永遠沒有日曆。

`source` 分 `government` 與 `company`：匯入只覆寫 `government` 列，自行設定過的日期
保留不動——那是公司的決定，重新匯入官方檔案不該把它抹掉。回傳的 `kept` 說出保留了幾天。
`set_work_calendar_day` 的 `kind` 留空＝刪掉該列，回到星期決定的日別。

**天災停班（颱風假）不在政府檔案裡，也永遠不會在**：辦公日曆表提前一年公布，停止上班
是前一晚由各縣市宣布的。它由人資在畫面上登記，`source` 記 `company`，所以下一次更新
不會把它蓋掉。**它不是國定假日**：勞基法對天災停班日的工資與出勤加給都沒有明文
（屬勞資協商），與國定假日的「工資照給、出勤加倍」是兩件事，所以它是第五種日別而不是
換個名字的國定假日。目前兩者共同的效果只有「不是工作日」。

**日曆事後改動不改寫已送出的休假單**。休假單在送出當下依當時的工作日曆記下每一天的
上班秒數（`leave_request_day`），之後登記停班日、更新政府日曆或把某天改成補行上班，
那張單的時數、天數與各日拆法都不動：停班日那一天仍以請假計，特休照扣、事假照扣那天的
工資；反過來，事後改成工作日的那一天不在單上。依據：已經請好的假遇到停止上班，法規
沒有明文，銷不銷假由勞資協商或工作規則決定；唯一的函釋（臺(78)勞動2字第24973號）是
婚假期間遇颱風停止工作之日計入婚假期間。天然災害出勤要點禁止的是把**沒有出勤的人**
強迫改記成請假，與事先請好假的人是兩件事。自動重算等於替每一家公司選了「一律銷假」，
婚假還會與那份函釋相反。

公司同意銷假時走既有的路：人資撤銷那張單（`revoke_leave_request`，理由必填），本人依
新日曆重送，再簽核。撤銷沿用「讀過那張單的薪資作業還在就拒絕」，所以已發薪的期別不會
被改寫。

讓人資知道要找誰的是 `set_work_calendar_day` 與 `refresh_work_calendar` 回應裡的
`leave_overlaps`：改動的日期上，待簽核或已核准的單與新日曆一邊計入上班時數、另一邊
不計，就列一列（日期、單號、員工、假別、狀態、本單計入的時數、依日曆的時數），稽核
摘要寫出幾張。更新政府日曆只看日別真的變了的日期，所以之前登記的停班日不會每次更新都
再列一次。呼叫者自己讀不到休假單時（矩陣沒有 `list_leave_requests`，或 MCP 通道上那個
工具關著）回 `null`——誰請了什麼假不是維護日曆需要知道的事。「工作日曆」畫面把清單印在
年曆上方，持有 `revoke_leave_request` 的人多一顆「前往出勤與休假」。

**每一種休息日都以休息日費率計價**（`DayKind::overtime_code`）。勞基法第 39、40 條
是按日加倍而不是時數級距，payload 沒有那個形狀，加上去要新增 day_types 的欄位與一個
日薪基數，兩者都是有生效日的法規值；天災停班日更是連法定費率都沒有。程式裡以
`ponytail:` 註記。

### 法規版本畫面

`list_regulations` 回九個領域各自的每一版，含 `payload` 與攤平後的 `fields`
（`{path, label, value, editable}`，群組節點帶 `children`）。**欄位顯示名與排序
同一份 `FIELDS` 表**：`jsonb` 的鍵是字典序回來的，照原樣印會把「員工負擔比率」排在
它所適用的「投保薪資上限」前面；識別字（`max_insured_salary`）也不得進畫面。

`save_regulation`／`delete_regulation`／`refresh_regulations` 是 `AdminOnly`，
`list_regulations` 移到新的 `manage_regulations` 權限（系統管理員、負責人、會計），
畫面掛在系統分頁。讀費率與改費率是兩件事：申報的人要查得到，改的人是裝機的人。

寫入前先跑 `probe()`——用該領域自己的計算子算一次。少一個欄位的 payload 在存檔當下
就被擋下，而不是等到發薪那天。`delete_regulation` 拒絕刪掉一個領域的最後一版：
`resolve()` 找不到就回錯誤、絕不回退到預設值，所以空的領域等於薪資、申報與折舊
全部算不出來。

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

**文件 bucket 固定為 RustFS 的 `strict` 存檔模式**。`relaxed` 回 200 時只保證物件
內容落盤，指向內容的 `xl.meta` 與目錄項還在 page cache，斷電遺失的部分靠其他節點的
erasure 副本補回；本產品是單機單碟，沒有其他節點，最近確認的版本會永久消失。文件刪
不掉、取回時比對 BLAKE3，所以遺失的物件會成為帳上登記卻取不出來的文件。RustFS
1.0.0-rc.2 起新建 bucket 預設寫入 `relaxed`，1.0.1 改成跟隨全域預設，但保留既有
bucket 的值，所以升級修不好。`ObjectStore::ensure_bucket` 在每個行程第一次呼叫時
（啟動時就會呼叫）讀 `GET /rustfs/admin/v3/bucket-durability/{bucket}`，不是 `strict`
就寫入 `strict`。寫在 bucket 上，不清掉 bucket 的值去跟隨全域預設，因為那個預設改過
兩次。請求以 SigV4 簽章，帳密就是 S3 那一組。失敗不擋啟動也不擋上傳（與物件儲存連不上
同一條理由），寫一筆 warn；之後的上傳也會呼叫 `ensure_bucket`，距上次失敗滿一分鐘就再試
一次，直到成功，所以啟動那一刻的暫時失敗不會讓 bucket 停在 `relaxed` 到下次重啟。不設
間隔的話，一個卡住的 admin API 會讓每一次上傳都多等一個逾時；嘗試在送出之前就先記下，
所以同時進來的上傳只有一個去試，其餘照常上傳。`/healthz` 的管理員詳情以
`rustfs.durability` 即時回報。compose 不設 `RUSTFS_NEW_BUCKET_DURABILITY_MODE`：
伺服器已經固定，再加一個等於同一件事有兩個機制。

上傳原本只有一條路：`upload_document` 的 `content_base64`，因為「所有操作都支援
MCP」不能有例外。那條路留著（25 MB，`MAX_OBJECT_BYTES`），但它有兩個後果：base64
膨脹三分之一，而 AI 要把整份 base64 當輸出寫出來——1 MB 的 PDF 約 140 萬字元，超過
單次輸出上限，所以 AI 實際上傳不進真實的檔案。

**上傳連結**把位元組搬出操作輸入，操作仍然是入口：

1. `request_upload_link`（`needs_confirm`，會寫一列）發一條 10 分鐘、至多 20 個檔的
   連結，回 `batch_id`、瀏覽器頁網址（`/upload/{secret}`）、`POST` 網址
   （`/v1/uploads/{secret}`）與 curl 指令。秘密只存 BLAKE3，稽核紀錄記「發了一條
   連結」不記連結。網址由 `oauth::public_base` 推導：OAuth 連接器本來就只能走主站
   網址，Caddy 在同一網域把 `/v1/*` 轉給 api。
2. 檔案 `POST` 到連結，**一個請求一個檔案**：Cloudflare Free／Pro 每個請求上限
   100 MB，所以 `MAX_UPLOAD_BYTES` 是 90 MB。串流寫進 RustFS 的 `staging/`：邊收
   邊算 BLAKE3，每 8 MiB 送一段 multipart，一個上傳只佔一段的記憶體。
3. `list_uploads` 列出收到的檔案；`upload_document { upload_id }` 在桶子裡以
   CopyObject 搬到文件的物件鍵，類型與雜湊用收件當下記下的那兩個。
4. 一天內沒人取用的暫存檔由 api 每小時的清除工作刪掉，連同桶子裡沒有資料列指著的
   `staging/` 物件與中斷的 multipart。
5. 存成文件時暫存物件已經不在（資料庫還原到歸檔或清除之前，見「主機端更新程式」），
   比照過期處理：刪掉那一列，回「暫存檔已不存在，請重新上傳。」

**先進暫存區，不直接成為文件**：文件封存後不可刪，傳錯的檔案要丟得掉；AI 也要先讀過
檔案才知道要掛哪一張單。**連結不綁定目標操作**是同一個理由。

**上傳連結只負責傳檔案**：這組操作把檔案送進私人儲存，讀內容交給 AI 自己的工具與
一起使用的其他 MCP。AI 在自己的環境裡本來就讀得了它要上傳的圖片、PDF 與試算表；伺服器
再解析一次，等於為外部送進來的檔案多養一個要隔離在無密鑰容器裡的解析器（圖片、試算表、
PDF 各一套），換來的是 AI 已經有的東西。使用者以瀏覽器上傳頁送來的檔案，AI 從
`list_uploads` 只看得到檔名、大小與類型，要掛哪一張單由使用者說明。CSV 匯入
（`preview_import`）是另一條路，它解析的是傳票內容本身。

`/v1/uploads/{secret}` 是 `/v1/auth/*` 之外第二個手寫路由：它收的是檔案本身，而
連結的用意就是讓位元組不進操作輸入。它只寫暫存區、不寫業務資料，授權就是連結：
讀 body 之前先驗連結存在、未到期、還有名額，且**持有者仍然有效**——Token 未撤銷、
未過期、擁有者合格（`token_owner_active!`），或帳號啟用中且角色沒變。收件與拒收
都以持有者的身分寫稽核（`upload_file`）。來源位址每分鐘 60 次。

**持有者是帳號或 MCP Token 其中之一**（`upload_batch` 的 CHECK），`OpCtx.user_id`／
`token_id` 由 session 或 Bearer 決定。一方暫存的檔案另一方看不到也取不走，別人的
`upload_id` 回「查無此上傳編號」。名額在讀 body 之前以單一 `UPDATE … WHERE
used_files < max_files` 佔下、失敗時退回；取用暫存檔以單一 `UPDATE … WHERE
consumed_at IS NULL RETURNING` 標記，同一個檔案存不成兩份文件，存檔失敗就放回去。

**類型由開頭的位元組決定**（`storage::detect_content_type`）：副檔名承諾的類型與
位元組不符就拒收（改過副檔名的 `.pdf`），位元組認不出的一律存成
`application/octet-stream`——HTML 與 SVG 永遠不會帶著瀏覽器會算繪的類型進桶子。

`list_documents` 的桶子用量不算 `staging/`（另列 `staged_objects`／`staged_bytes`），
不然每一個還沒存成文件的暫存檔都會讓文件對帳差一個數。`max_object_bytes` 回的是連結
的上限（畫面走連結），`max_inline_bytes` 是 base64 的上限。

**下載連結**是同一件事的反方向。`get_document` 以 base64 回內容，上限同樣是 25 MB：
`include_content` 遇到更大的文件回錯誤、請使用者到「文件管理」畫面下載；不帶內容時照常
驗證，並邊讀邊算雜湊（`ObjectStore::verify`），不把整份檔案放進記憶體。更大的文件：

1. `get_download_link { doc_no, version }`（`read_only`，與 `get_document` 同屬
   `manage_documents`）寫一列 `download_link`，回一條 5 分鐘、只能用一次的網址。**對 AI
   預設關閉**：它是為了畫面而做的——上傳上限提高到 90 MB 之後，超過 25 MB 的文件得有
   下載的路——而 AI 的上傳流程用不到它；要讓 AI 取得大檔，由系統管理員在「MCP 與 AI」
   打開。它會寫一列，卻不算寫入（`writes()` 為 false）：授權失效的安裝仍然交得出自己的
   文件，連結本身不是業務資料。秘密只存 BLAKE3，持有者的規則與上傳連結共用
   （`links::LinkHolder`、`holder_active!`）。員工附件回查無此文件。
2. `GET /v1/downloads/{secret}` 驗連結存在、未使用、未到期、持有者仍然有效，先佔一個
   下載名額（`downloads::DOWNLOAD_SLOTS`）再以單一 `UPDATE … WHERE used_at IS NULL`
   把連結標成已使用，然後把物件串流到本機暫存檔、同時算 BLAKE3（`ObjectStore::spool`），
   **相符才開始送出**；不符回 409 與錯誤訊息，body 不含任何檔案內容。回應帶
   `Content-Disposition: attachment`、`nosniff` 與 `Content-Length`，類型取自
   `document_version`。
3. 核發由 `dispatch` 寫稽核；實際下載以持有者的身分寫 `download_file`，成功、雜湊不符
   與物件遺失各一種結果。

**下載連結不是 presigned URL**。presigned URL 由 RustFS 自己驗簽，發出去之後 api 看不到它
被用了幾次、被誰用，也撤不回——撤銷 Token 或停用帳號之後照樣有效，而 RustFS 的埠本來就
沒有對外開。更根本的是它繞過雜湊比對：RustFS 不知道 `document_version.checksum`，被動過
的物件會原樣送出去。下載連結是資料庫的一列，每一次使用都經過 api、比對雜湊、寫稽核。

**先落地再送**：邊讀邊送的話，雜湊對不上時檔案已經送出大半。暫存檔建立後立刻 unlink，
只剩開著的 handle 讀得到它，所以用戶端中途斷線或程序結束都不留殘檔；名額在回應送完或
中斷時才歸還，同時佔用的磁碟空間至多 `DOWNLOAD_SLOTS` 份檔案（8 份、每份至多 90 MB，
放在 api 容器的暫存目錄，在容器的可寫層上而不是 tmpfs）。等 30 秒還拿不到名額就回 503，
這時連結還沒用掉，同一條可以再試。**連結在驗證之前就標成已使用**：驗證失敗或物件儲存
暫時連不上時重新取得一條即可，而兩個同時送出的請求只有一個拿得到內容。`HEAD` 回 405：
它要跑完整個下載才答得出標頭，還會順便把連結用掉。過期一天的連結由每小時的清除工作
刪掉，與暫存區同一個排程。複製到暫存檔時讀到超過登記的 `byte_size` 就停下並當作雜湊
不符：名額限得住同時幾份，限不住每一份多大，被換成更大檔案的物件否則會先寫滿磁碟。

**畫面用 `fetch` 而不是導向連結**（`client.ts` 的 `downloadDocumentFile`，文件管理、
傳票附件與報支收據共用）：導向的話，雜湊不符時瀏覽器只顯示「下載失敗」，說不出原因；
`fetch` 讀得到錯誤訊息，也畫得出進度。回應經 `TransformStream` 計數後直接轉成 Blob——
Blob 由瀏覽器管理，不是舊做法那樣 base64 字串、`atob` 的結果與 `Uint8Array` 三份同時
放在 JavaScript 記憶體裡。報支收據那顆按鈕因此看 `get_download_link` 而不是
`get_document` 決定畫不畫。

**沒有 OCR**。`ocr_status` 原本由上傳當下的 MIME 型別推出 `pending` 或
`not_applicable`，沒有任何判斷讀它，而畫面上那張統計卡與逐列標籤在每一套安裝上
都印同一件事。要接引擎時新增欄位，不是留一個永遠停在 `pending` 的欄位等它。

### 身分證字號完整保存

原本 `employee.id_no` 只收遮罩後的值，理由是「系統沒有功能需要完整字號」。
那個前提在人資模組上線後就不成立了：勞保投保申報與扣繳憑單（`payee_id`）都要
完整字號。擋著的結果不是資料不存在，而是人資另外維護一份 Excel——個資散到系統
外面，讀了也沒有紀錄。

因此完整保存，寫入時驗格式與檢查碼（一個英文字母加九碼數字），還沒收到
可以留空。**遮罩是顯示層的事**：名冊預設顯示 `A12*****89`，頁面上一顆切換鈕整批
顯示或整批遮蔽。稽核紀錄則由 `redact_for_audit` 的 `REDACTED_FIELDS` 依欄位名把
`id_no` 換成「[已遮蔽]」——長度規則只擋超過 512 位元組的值，十個字元的字號永遠
碰不到它，而同一批參數裡的 `emp_no` 已經足以回答那次呼叫動的是誰。

### 員工的銀行帳戶不只一個

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

舊欄位的值搬過來時只能盡力拆：第一個數字之前是銀行、之後是帳號。整串
沒有數字的原樣留在帳號欄，於是它會出現在名冊上等人更正，而不是安靜消失。

重複帳號的錯誤訊息只寫銀行名稱不寫帳號：錯誤訊息原文寫進稽核摘要，而
`REDACTED_FIELDS` 只碰輸入參數，碰不到摘要。

### 眷屬名單只記健保眷屬

`employee_dependent` 記掛在員工健保底下的眷屬：姓名、關係、身分證字號、生日、
健保加保日與退保日。健保自付額的眷口數從這份名單推導；加退保要經過投保單位辦理，
這份資料公司本來就要掌握。

**不記列報扶養**：員工報綜所稅時列誰為扶養親屬是本人的選擇，每年可能換人（兄弟姊妹
輪流扶養父母），扣繳憑單上也沒有這一欄。公司唯一用得到扶養人數的地方是按扣繳稅額表
預扣，而系統一律按 5% 扣繳；為一個沒有計算會讀的欄位逐位眷屬記一份，等於多存一份
會過時的個資。改按扣繳稅額表時，記的是員工以免稅額申報表申報的人數，不從這份名單
推導（#108）。

**投保期間記日期不記布林**：保費按月計，補算前期薪資要問的是「那個月誰在保」，
布林只回答得了「現在誰在保」。加保日必填（`NOT NULL`）——沒有加保日的一列不是健保
眷屬，而名單不收其他人。`health_enrolled_until` 是**最後在保日**（當天仍在保），
所以加保與退保可以是同一天；退保日不得早於加保日。關係只收得到全民健康保險法第 2 條
的眷屬範圍：配偶、父母、祖父母、子女、孫子女。清單在 `ops/employees.rs` 的
`RELATIONS`，由 `list_employees` 以 `relations` 帶給畫面；
`the_relations_list_agrees_with_the_table` 拿它比對 migration 的 CHECK。

`dependents` 與 `bank_accounts` 同一族的三態：不帶＝不動、空清單＝全部刪除、給清單
＝整份取代。字號存大寫（CHECK 釘住），同一員工底下不得重複（partial UNIQUE，空白
不算，還沒收到的可以有好幾個），寫入前走與本人字號相同的 `is_valid_id_no`。錯誤
訊息寫眷屬姓名、不寫字號，理由與上一段的帳號相同；稽核紀錄裡的 `dependents[].id_no`
由 `redact_for_audit` 依欄位名遮蔽（它遞迴進陣列）。

**退保填日期，刪除只給登記錯誤**：刪掉一列等於刪掉它的投保期間，回查過去日期的
勞健保頁不再計入那個人。對真的在保過的眷屬那是錯的——結束要填退保日；對登記錯的人
那正是要的結果。兩件事因此是兩個動作：存過的眷屬不畫 ×，改畫「登記錯誤，刪除」，
按下去整張卡換成一句後果加「復原」，存檔才刪。已算好的薪資單不受影響，計入的口數
存在 `payslip.health_dependents`。加保日可以改、不能清空。

三個管道看到的不一樣：名冊（人資）回完整資料；個人專區回姓名、關係與投保期間，
**不回字號與生日**（與本人字號同一條規則）；MCP 通道的名冊整份拿掉
`dependents`，與底薪、銀行帳戶同一個 `redact_for_ai`。

## REST 登入只接受伺服器端 session

`user_account`、`user_identity` 與 `user_session` 接上 REST 登入。
`POST /v1/auth/login` 以 Argon2id 驗證密碼，簽發 32-byte CSPRNG session token；
瀏覽器只收到明文 cookie，資料庫只保存 BLAKE3 雜湊。Cookie 固定為
`HttpOnly; SameSite=Strict; Path=/`，`Secure` 由 `SESSION_COOKIE_SECURE`
控制，本機 HTTP 可關閉，正式 HTTPS 部署必須開啟。

**Argon2id 驗證同時最多核心數個**（`auth.rs` 的 `VERIFY_SLOTS`）：查無帳號也對固定
雜湊驗一次，所以任何登入名稱都走得到這一步，而以名稱分桶的限流擋不到每次換一個名稱的
灌量。沒有上限時每次驗證在一條 blocking thread 上佔 19 MiB，灌 100 個就到 1.9 GB；
有上限之後排隊的只有登入。

session 同時受兩個可調上限限制：`SESSION_IDLE_MINUTES` 以 `last_seen_at` 判斷
滑動閒置時間，每次成功使用都更新；`SESSION_ABSOLUTE_HOURS` 在建立時寫入
`expires_at`，即使持續使用也不能越過。撤銷、任一種到期或帳號停權都回 401。

`x-laysi-actor` 與 `x-laysi-role` 不再參與 REST 身分解析；`OpCtx.actor`、
`actor_role` 與 `employee_id` 都來自 session 查回的 `user_account`。登入失敗
不區分帳號不存在、沒有本機密碼、停權或密碼錯誤，避免提供帳號列舉訊號。

OIDC 是選用入口，**設定在畫面上而不在環境變數裡**：安全與營運頁的「外部
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
同一個判斷因此在按下去之前就看得見。停用的開關本身也放在精靈第四步，而不是留到事後的
安全與營運頁：它與第五步是同一個決定，分開問就會出現「關掉密碼登入，然後發一組密碼給
負責人」這種走得完、但結果沒有人登得進去的路徑。

帳號管理（`screens/Accounts.tsx`）的建立抽屜用的是同一個欄位與同一組判斷，因為精靈之後
建的會計、人資走的是同一條路。它另外多一件事：免密碼的帳號沒有初始密碼卡可以顯示，
所以改發一則 toast——沒有東西要交付不等於什麼都不必說，靜靜關掉抽屜與「按下去看起來
成功、其實什麼都沒發生」是同一個毛病。`get_oidc_setting` 是 `AdminOnly`，與這個畫面
本身的權限相同，讀不到時就退回只有密碼的行為。

### 外部登入會清掉 `must_change_password`

系統管理員用 `create_user_account` 建的帳號帶著一組產生的密碼與這個旗標。從 IdP 進來
的人沒拿過那組密碼，也不需要密碼，旗標卻會在一次成功的登入後再擺一張密碼表單。
`oauth_callback` 因此在發 session 之前清掉旗標。

只清旗標，不動 `password_hash`：那組密碼仍在，本人之後真想改用密碼登入時
`reset_user_password` 還叫得動。清掉雜湊會讓帳號變成只有外部身分進得來，而
`unlink_user_identity` 拒絕沒有密碼的帳號，等於把一個可逆的狀態換成不可逆的。

### 帳號密碼登入可關閉：`password_login_disabled`

`oidc_setting` 多一欄 `password_login_disabled`。開啟後除系統管理員角色外，
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

`unlink_user_identity`（`AdminOnly`）在同一個交易裡刪掉 `user_identity` 並
撤銷該帳號未過期的 session——OIDC 開出來的 session 不會因為連結消失而失效，不撤等於
沒解。它拒絕 `password_hash IS NULL` 的帳號：解完就沒有人登得進去，而畫面上沒有任何
操作能補一組密碼回去（`reset_user_password` 同樣拒絕外部登入專用的帳號），那條路只剩
主機上的 `laysi-admin reset-password`。畫面因此在那種帳號上請人改用停用。

`delete_user_account` 現在拒絕還連著外部身分的帳號。理由是自動建帳號讓刪除自己撤銷
自己：人還在網域裡，下一次登入又建一個新的。先解除連結是那個刻意的步驟。仍要注意
解除連結本身不等於擋住登入——自動建帳號開啟時，同一個 email 再登入就會由第 2 步重新
連上，真正擋住登入的是停用帳號，或在 Cloud Identity 那側停用該帳號。離職流程照舊：
結案那一步停用登入帳號並撤銷 session 與 MCP Token，那才是本站這側的關門動作。

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

`install_bootstrap` 是單列表（`id int PRIMARY KEY CHECK (id = 1)`）。
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

`SETUP_TOKEN` 存在的理由是那段時間差：容器起來到負責人打開瀏覽器之間，先到的人
就是負責人。內網無所謂，公開位址不是。**它由 compose 的 `secrets` 服務隨機產生**，
程式從 `SETUP_TOKEN_FILE` 讀。不必人自己想一段亂數——那條路的實際結果是留空。

代碼要送到的是**瀏覽器前面那個人**，不是終端機前面那個人，所以它寫進
`setup/初始化代碼.txt`（bind mount，SELinux 用 `:Z`），初始化畫面的欄位提示直接指那個
路徑。**刪掉就不再產生**（`.written` 標記），因為第一個帳號建立之後那段字串沒有任何
用途——`install_bootstrap` 是單列表，事後清空 `user_account` 也打不開那個端點。容器日誌
仍留一行當退路。錯誤的代碼寫 tracing
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
**每個 Registry 操作恰好屬於一條 permission**，由 `laysi-ops` 的純字串測試釘住：它
`include_str!` 讀 `0001_initial.sql` 的 `INSERT INTO permission`，缺漏與重複都不准
通過。

### 人資是一個角色，不是會計的別名

人資相關的 permission 原本只掛在負責人與會計身上，所以一位專職人資只能設成
`acct`，連帶拿到開立傳票、過帳、應收應付、銀行核銷與稅務申報的完整財務權限——
為了讓他核一張休假單。

`hr` 持有五組：`view_hr`、`manage_hr`、`view_payroll`、`manage_payroll`、
`staff_self_service`。這一組剛好打開人資分頁的八個畫面，財務分頁與系統按鈕
一個都不出現。

**逐人薪資明細不在裡面**，也不是漏掉的：`list_payslips` 與
`list_withholding_statements` 掛在 `BossOnly` 底線上，權限矩陣拉不動它，給
`hr` 一列 `view_payroll_details` 只會多一列不生效的資料列。人資算得出一期薪資
（`run_payroll`）也對得起總帳（`get_payroll_summary`），逐人金額仍是負責人的事。

**名冊的部門下拉因此改由 `list_employees` 帶回來。** 那個下拉原本呼叫
`list_dimensions`，而它屬於 `view_masters`——同一組權限也開著「基礎設定」，所以
為了一個部門清單就得把系統分頁交出去。呼叫端還把失敗吞掉（`.catch(() => null)`），
於是少了那組權限的角色會看到一個空的下拉，畫面上沒有任何跡象。`list_employees`
現在回一份 `departments`，與 `list_fixed_assets` 回 `categories` 同一個做法：
畫面要的值由它讀的那個操作帶著走。

### 角色只在帳號上，員工主檔沒有角色

`employee.role_code` 曾經存在，但決定權限的是 `user_account.role_code`。
員工主檔卻給了一個
可編輯的下拉、七個角色任選，連沒有登入帳號的人都存得進去，名冊還把它跟職稱、
僱用型態排在同一串點號裡：「業務專員 · 正職 · 一般職員」，三個不同的東西一個
語氣。除了顯示它的畫面之外沒有任何程式讀它，所以那一欄直接拿掉，而不是
換一個比較好的標籤。

一個東西一個名字：角色在帳號上。

### 角色可以變更，但不能變更自己的

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
留著就是舊角色永久有效，所以 session 全部撤銷、`owner_user_id` 指向這個帳號或
`owner_employee_id` 指向本人的 Token 一併撤銷，操作回報各撤了幾個。只看員工編號會漏掉
沒有員工主檔的帳號（`admin`）授權的連接器：角色不符時它被擋下，改回原角色它又能用。

### 兩道風險底線：負責人與系統管理員

`Risk` 有兩個「限某角色」的層級，它們保護的是不同的東西：

| 底線 | 常數 | 保護什麼 | 操作 |
|---|---|---|---|
| `BossOnly` | `ROLE_BOSS = "boss"` | 業務機密與權威 | `list_payslips`、`list_withholding_statements`、`set_budget`、`create_budget_version`、`set_approval_rule` |
| `AdminOnly` | `ROLE_ADMIN = "admin"` | 系統本身的控制項 | `update_company_setting`、`set_role_permission`、`issue_api_token`、`create_user_account`、`update_user_account`、`change_user_role`、`reset_user_password`、`delete_user_account`、`list_user_accounts` |

兩者原本是同一個：`boss` 既改權限也看薪資，於是**裝機的人自動看得到每個人的
薪水**。分開之後，系統管理員管得了帳號、權限、Token 與公司設定，卻讀不到任何一張
薪資單；負責人看得到薪資、定得了預算與簽核門檻，卻改不了權限矩陣。

這條分離之所以成立而不只是慣例，靠的是兩件事：底線在權限矩陣之外，`set_role_permission`
無法把 `admin` 提升到 `BossOnly`；而 `change_user_role`**拒絕變更呼叫者
自己的帳號**。系統管理員改得動別人的角色——建帳號時本來就指定得了任何角色，這個
方向沒有多出權限——但升不了自己。

系統管理員另外持有 `manage_masters`，因為首次設定要建銀行帳戶與部門；該組裡的
`set_approval_rule` 仍擋在負責人底線之後。`revoke_api_token` 刻意只是 `HighRisk`：
撤銷往安全的方向走。

`create_user_account` 是唯一不需要 shell 就能建立登入的路徑。它不收密碼——密碼由
系統產生、只在該次回應出現一次、`must_change_password = true`，所以建立帳號的人
無法長期使用別人的帳號。連結員工用 `employee_no` 而非姓名，姓名會重複。

**強制變更密碼那一步不再問目前的密碼**：那張表單接在剛才用臨時密碼登入成功的下一
步，session 已經證明過同一件事，再問一次只是把剛打完的字再打一次。
`change_password` 因此只在 `must_change_password` 為 false 時驗 `current_password`
（輸入欄位改為選填），其餘呼叫仍要證明。

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

**工具開關由 `set_mcp_tool_grant` 在畫面上改**。`mcp_tool_grant` 決定 AI 叫得動
什麼，而在此之前改一列的唯一方法是打開資料庫：新工具永遠停在種子說的那個值，而「MCP 與
AI」那一頁只報得出現況。操作是 `AdminOnly`，它自己那一列種成關閉，並且**拒絕把自己打開**
——否則系統管理員的一把 MCP Token 就能自行開啟其餘每一項。畫面上那一列因此不畫開關，改印
「無法對 AI 開啟」。

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

**自助與連接器 Token 跟著擁有者的帳號走。** `token_by_hash` 同一道查詢回傳
`owner_active`，條件寫在 `token_owner_active!` 一處，`touch_api_token_used` 接受那一刻
再判斷一次：

- `owner_user_id` 不為 NULL（OAuth 連接器）：那一個帳號要啟用中、且 `role_code` 等於
  Token 的 `actor_role`。刪掉的帳號 id 對不到任何列，同名重建也接不回來。
- 否則 `owner_employee_id` 不為 NULL（自助核發）：該員工底下要有一個啟用中、角色相同的
  帳號。
- 兩者都是 NULL（系統管理員以安裝本身名義核發）：不綁定到人，不受任何帳號影響。

停用與刪除帳號都只寫 `user_account`、不碰 `api_token`，所以判斷放在讀取端，與
`session_by_hash` 帶 `u.status = 'active'` 同一個做法——下一條會停用帳號的路不必自己
記得撤銷。角色也要相符，因為 `/mcp` 以 `actor_role` 這份核發當下的快照行事，而
`user_account.employee_id` 不唯一：同一個人之後以另一個角色重建帳號，不能讓舊 Token
帶著原角色回來。**連接器要另記帳號**，因為 OAuth 可以從沒有員工主檔的帳號出發——
初始化建的 `admin` 就是——只記員工編號的話，它的連接器看起來與安裝本身的 Token 一樣，
帳號停用了它還在。`owner_user_id` 刻意不設外鍵：`ON DELETE SET NULL` 會把 Token 變成
安裝本身的，`CASCADE` 會刪掉清單上還看得到的那一列。這一欄加上之前授權的連接器推不回
是哪個帳號（列上只有會同名的顯示名稱，以及可能有多個帳號的員工），留在員工那條規則上
會跟著同一個人之後的任何帳號回來，所以升級時依 `oauth_token` 稽核認出來、全部撤銷，
每個連接器重新授權一次。稽核的保存天數調短之後會被清掉，寫入也不保證成功，所以長得像
連接器的 Token（MCP、沒有員工編號、`scope` 為「與本人相同」、沒有輪替天數、建立者等於
代表對象——那正是會被當成安裝本身 Token 的那一種）另外處理：稽核裡找得到核發它的那一筆
`issue_api_token`（摘要寫著名稱與前綴，格式從那個操作存在起沒變過）就留著，找不到就撤銷。
預設保存五年，所以在預設設定下不會撤到任何一組公司 Token；自助核發與安全與營運頁核發
的形狀本來就不同。帶員工編號而稽核已經不在的舊連接器留在員工那條規則上，權限不會超過
那個人現在的角色。

在帳號管理停用後再啟用，Token 跟著恢復，與 session 相同。離職（`close_access`）與
變更角色（`change_user_role`）不靠這條判斷，兩者在同一個交易裡直接撤銷，回任或改回
原角色都不會恢復。

**Token 會過期，不只能撤銷。** `rotate_days` 有值時，`created_at +
rotate_days` 之後即強制失效（`ApiToken::is_expired`），不必等人手動撤銷。這與清單
畫面上「距離下次輪替還有幾天」的提醒（以 `last_used_at` 估算）是兩件事：一個決定
Token 還能不能用，一個只是提醒該換了。撤銷、過期與擁有者帳號不再相符三種原因會
分別寫進稽核摘要（`mcp_auth` 這筆），但**對外的 HTTP 回應一律是同一句 401**，
不讓探測者從回應內容反推出這把 token 是哪一種。
`list_api_tokens` 同時回傳 `expires_at`／`expired` 與既有的
`rotate_due_on`／`overdue`，兩組數字不要混用。

## 資料庫 schema（PostgreSQL 18）

Schema 的起點是**一個檔案**：`backend/migrations/0001_initial.sql`，以
`sqlx::migrate!()` 在啟動時執行。上線前的 migration 已經三度合併進去，因為當時還沒有
任何一個安裝需要沿用，而新裝的人讀一份 SQL 比讀一串差異合理。合併只在還沒有安裝要
沿用時做得了；有了第一個正式安裝，這條路就關上，之後每一次變更都是新的一份檔案。

**本文不寫 migration 編號。** 合併過三輪之後，同一個四位數指過三份不同的 migration，
而指出去的檔案一份都不存在。會影響跨層契約的結論寫在本文，不靠那些檔案保存；單一
決定的來由用 `git log -S` 找那段程式或那個欄位名。

合併之後**每一次 schema 變更都是新的一份 migration**，0001 不再更動——它一旦被套用過，
改一個位元組就會讓現有的資料庫拒絕啟動。

**「已套用過的檔案一個位元組都不能改」是實測過的**：有一份 migration 曾經在套用之後
被補寫一段 `UPDATE`，於是那個安裝的伺服器每次啟動都停在
`migration 3 was previously applied but has been modified`。修法是補跑漏掉的那段、
再把 `_sqlx_migrations.checksum`（檔案的 SHA-384）對齊現在的檔案，不是改檔案。

**種子資料的最終狀態是每一份 migration 的 INSERT 疊起來**：`laysi-ops` 三個比對測試
以 `include_str!` 讀這些檔案（`SCHEMA` 是它們的串接），算出 permission 與
mcp_tool_grant 的名稱集合再與 Registry 對照，並比對每一列 grant 的風險分級。後面的
檔案要接在前面的後面，因為測試把後出現的資料列當成勝出的那一列，與資料庫裡後一道
敘述蓋掉前一道相同；動到 `permission.op_names` 的 migration 因此整列重寫
（`ON CONFLICT (code) DO UPDATE`），不用 `array_append`——字串比對讀不出後者。分級也要比，因為 `scripts/mcp-role-matrix.ts` 的
INV1 讀的是這一欄：寫低了就會放過它本來要抓的那個工具。**下一份 migration 若改動
permission 或 mcp_tool_grant，要把它接到 `SCHEMA` 後面**：那三個測試把 migration 當
文字讀、只看得見 INSERT，所以改動要重寫整列而不是附加一個名字，而新增的操作少了種子
會讓測試變紅，拿掉一個種子不會。

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
            unique(domain, version))

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
company_setting             公司與帳務設定。**法規決定的值進 reg_ruleset，不進這張表**
dimension / dimension_value 部門。專案、成本中心與付款條件三個沒有消費者的分類
                            已刪掉；department 以複合外鍵掛到分錄與員工
role / permission / role_permission
                            人的權限矩陣。與 mcp_tool_grant 是兩件事：前者管人，
                            後者管 AI。dispatch 每次呼叫即時強制 deny-by-default
approval_rule               費用簽核流程與金額門檻（公司政策，不是法規）
api_token                   **不存明文**，只有 BLAKE3 雜湊與前八碼

-- 公告與規章
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
                            結果。沒有解析器的五列來源已刪掉

-- 登入、權限、主檔操作與 MCP 憑證
user_account                登入帳號；可選擇連到 employee，密碼雜湊可為 NULL
user_identity               以 IdP 的 provider + subject 唯一識別；email 只供稽核
user_session                只存 token 雜湊、到期與撤銷狀態，明文不落地
permission.op_names         每個 Registry 操作恰好屬於一條 permission
account.is_active           停用阻止新分錄，但保留歷史帳
voucher.reverses            沖銷傳票指回原傳票，UNIQUE 防止重複沖銷

-- 員工的銀行帳戶
employee_bank_account       一人可有多筆；`is_payroll` 標出薪轉的那一筆，
                            以 partial UNIQUE 索引限制至多一筆。
                            原本是 `employee.bank_account` 單一自由文字欄

-- 員工的健保眷屬
employee_dependent          一人可有多筆；`health_enrolled_from`（必填）／
                            `health_enrolled_until`（最後在保日）。關係限健保法
                            第 2 條的眷屬範圍（CHECK）；
                            `id_no` 存大寫，同一員工底下不重複（partial UNIQUE）

ai_audit_log(id bigserial PK, at timestamptz, actor text, actor_role text,
             tool_name text, risk text, prompt text null,
             params jsonb, result_summary text, outcome text,
             voucher_no text null, reverted bool default false,
             client_ip text null)
    outcome ∈ ok | blocked | error
    client_ip  來源位址，取自前置代理的 X-Forwarded-For（第一段）或 X-Real-IP，
               由伺服器解析成位址才寫入，不是位址就留空——那個標頭沒有代理改寫時
               由呼叫端說了算，照收等於在稽核紀錄裡開一格任人填的欄位。
               只記錄，不參與任何判斷。
```

**登入本身也是稽核事件**：`/v1/auth/*` 不經 `dispatch`，所以登入成功、登入
失敗、登出、變更密碼由 `auth::write_auth_audit` 各寫一列（`tool_name` 為 `login`／
`logout`／`change_password`，初始化是 `bootstrap_install`）。少了這條，一次帳號遭盜用
在系統裡只留下 stdout 的一行，而容器重啟就沒了。**成功與失敗的訊息分得比回應細**：
回應對「查無此帳號」與「密碼不正確」一律回同一句，稽核紀錄則分開寫——分辨得出來才
查得下去，而讀得到稽核紀錄的只有系統管理員。**被速率限制擋下的嘗試不寫**：那一段還
沒驗任何憑證，逐次寫等於讓外面的人決定這張表長多快。

**保存期限是設定，不是常數**：`audit_keep_days` 預設 1825 天（勞基法要求
出勤紀錄保存五年，而關於出勤的稽核紀錄不該比它短命），留空表示永久保存。伺服器啟動
時 spawn 一個每日迴圈，每一輪重讀設定再刪，所以改了不必重啟；值為空或解析不出來就
整批留著——保存期限要是公司決定的，不能由一個打錯的字決定刪掉什麼。`Store::purge_audit`
是全 codebase 唯一刪 `ai_audit_log` 的地方，那正是這個設定答得出「你們留多久」的原因。
清除跑在伺服器而不是備份容器：沒開備份的安裝一樣要守自己的保存期限。

**匯出走 `list_ai_audit` 本身**：輸入多了 `from`／`to`（營業日）與最高 10000 筆的
`limit`，畫面把取回的列組成 CSV 由瀏覽器下載，不另建操作、不另開權限。日期在後端轉成
兩個瞬間（`to` 取隔日零時，所以最後一天整天都在內），因為 `at` 是時間戳而人挑的是
營業日。

必要的不變量，以 SQL 強制（不能只靠 Rust）：

1. `voucher_line`：`debit >= 0 AND credit >= 0 AND NOT (debit > 0 AND credit > 0)`
2. 每張傳票借貸相等：**deferred constraint trigger**，於交易結束時檢查
   `SUM(debit) = SUM(credit)`，且行數 ≥ 2。
3. `status = 'posted'` 的傳票不可 UPDATE／DELETE（trigger 擋下），唯一狀態例外是
   `posted → void`；確實發生但記錯的交易則另開反向分錄。
4. 同一位員工的休假單彼此不得在時間上重疊，加班申請亦然：
   `EXCLUDE USING gist (employee_id WITH =, tsrange(from_at, to_at, '[)') WITH &&)
   WHERE (status IN ('pending', 'approved'))`。

### 一個人不能在同一段時間裡休兩次假

兩張重疊的休假單各自扣一次特休、各自進一次薪資；兩張重疊的加班申請由 `run_payroll`
各付一次。這件事原本沒有任何一層在管——`submit_my_overtime_request` 只比對申請時段與
上下班時間有沒有重疊，兩張申請之間從來沒有比過。

**時段是半開區間 `[)`**，所以前一張到 18:00、後一張從 18:00 開始並存；真正重疊的那
一秒才擋。**只有待簽核與已核准的單在約束範圍內**：被駁回、撤回或撤銷之後，同一段日期
要能重送。條件列舉佔住時數的兩種狀態，而不是排除不佔的那幾種——再多一種狀態時，要先
決定它站在哪一邊才寫得進 CHECK。PostgreSQL 18 的 `WITHOUT OVERLAPS` 是
`UNIQUE`／`PRIMARY KEY` 的一種，**不收 `WHERE`**，會把重送一併擋掉，所以這裡用排除
約束而不是它。相等欄位（`employee_id`）與範圍要進同一個 GiST
索引，`btree_gist` 因此在 0001 的開頭啟用。

`create_leave_request` 與 `create_overtime_request` 在寫入前各查一次擋路的那張單，
**那一步只決定錯誤訊息說什麼**——兩個同時送出的申請都查得過，把它擋下來的是約束。
測試是 `requests_cannot_overlap_for_one_employee`。

### 單據編號

格式 `{前綴}{西元年末兩碼}{月份兩碼}-{三碼流水}`，例：2026/07 第 18 張傳票 →
`V2607-018`。以 `doc_sequence` 取號，同一交易內遞增。傳票 `V`、付款批次 `P`、
費用報支 `EX`、文件 `D`、匯入批次 `IM`、休假單 `LV` 共用這一套。

**休假單原本用民國年**（`LV11507-001`），於是同一天建立的四張單上並排著兩種
曆法。年份的寫法是編號格式的一部分，不是各模組自己的選擇。

**計數器的鍵是（前綴, 期間），不是前綴。** 原本一個前綴一列，另存一個
`period` 欄，換期間就把計數器歸 1 並覆蓋期間。往前走是對的——新的月份從 001
開始；往回走就不是。提列 9 月折舊（傳票日期 09-30）之後再算 8 月薪資（傳票日期
08-31），計數器歸 1 而發出 `V2608-001`，那個號碼已經存在，`voucher.no` 是 UNIQUE，
於是薪資作業以資料庫約束錯誤收場，而畫面上繞不過去。鍵因此是兩欄，並用各
單據表已發出的號碼重建每一期的計數器。

### 主檔編號：`E###` 與 `FA###`

員工編號與資產編號**不走 `doc_sequence`**，而是在寫入的同一個交易內取 advisory lock
再掃現有最大值。配號由 `create_employee`（`emp_no` 留空時）與 `create_fixed_asset`
（`code` 留空時）自己做，呼叫端拿不到一個「已經配好的號碼」。

**為什麼不用計數器。** 計數器只往前、不回收刪掉的號碼；掃描讀的是名冊或登記簿本身。
決定性的差別在匯入：把既有名冊 `E001`–`E050` 匯進來之後，計數器仍停在 0，下一個自動
配號發 `E001` 並當場撞上 UNIQUE，而且每匯入一次撞一次；掃描匯完就接上 `E051`。掃描的
代價是回收刪掉的號碼，但固定資產一定指向傳票（外鍵擋著刪除）、員工離職是改狀態不刪列，
兩張表實際上刪不掉，那個代價收不到。單據相反——一期之內順序發號、跨期歸零正是它要的
行為，所以 `V`／`P`／`EX`／`D`／`IM`／`LV` 維持 `doc_sequence`。

**鎖要涵蓋寫入才有意義。** 只鎖配號那一段沒有用：掃描不改變任何一列，第一個交易放掉
鎖之後，第二個交易讀到的仍是同一個最大值。公開方法 `next_employee_no()` 因此移除——
它跑在連線池上，算完把字串交還給呼叫端，呼叫端再另開交易寫入，中間的窗比鎖本來要關的
那個還大。三條配號的路（`create_employee` 的兩個呼叫端，以及 `provision_external_user`）
現在共用 `EMPLOYEE_NO_LOCK_KEY`：**一把只有三分之一的寫入者拿的鎖不是鎖**。資產編號用
另一把 `ASSET_CODE_LOCK_KEY`，分開是為了登記資產不必排在建帳號後面。

**撞號的訊息分兩種。** 人自己填的編號撞到，`DUPLICATE_MESSAGES` 照舊說「請換一個」；
系統配的號碼撞到，`generated_number_collision` 改寫成請重新送出——那個號碼沒有出現在
畫面上，欄位是空的，沒有東西可以換。鎖讓後者變罕見而不是不可能，還原與匯入都插得進
一個不經這條路配出來的編號。

### 設定值必須做得動

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
對 `punch_correction_policy`、`owner_self_decide` 與 `payroll_timing` 只收各自的兩個值。驗在後端而不只在精靈裡，因為 MCP 與
未來的匯入程式走的是同一個操作。這些值不是法規數值，是公司自己的登記事實與決定，
所以留在 `company_setting` 而不進規則檔。

**只有幾個答案的設定由後端列出來，畫面照著畫**：`get_company_settings` 每一列多回
一個 `options`，有值就是下拉、沒值就是文字欄。`punch_correction_policy` 原本是文字
欄，於是政策只有兩個答案，畫面卻要人把其中一個逐字打對，打錯了就靜靜當成「人資
審核」。清單放在後端，是因為判斷這個值合不合法的也是後端。

公司地址與電話（`company_address`／`company_phone`）與公司名稱同一族：**不在
`CLEARABLE_SETTINGS` 裡**，空字串的意思是還沒填，不是這家公司沒有地址。

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
- `import_source`：保留匯入來源。
- `reg_ruleset`：法規的權威資料，不是投影。
- `work_calendar`：與星期不同的日期，只存例外。

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
    pub fn new() -> Self;             // 註冊全部操作
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
5. 寫 `ai_audit_log`（`ReadOnly` 也要記，對應原型的唯讀稽核列），連同
   `OpCtx.client_ip`。

### 核心操作

| name | risk | input | output |
|---|---|---|---|
| `get_dashboard` | ReadOnly | `{}` | `{ stats[], todos[], cash_bars[], ai_recent[] }` |
| `get_trial_balance` | ReadOnly | `{ as_of?: date }` | `{ rows: TrialBalanceRow[], total_debit, total_credit }` |
| `get_cash_position` | ReadOnly | `{}` | `{ accounts: [{name, account_no, balance}], total }` |
| `search_vouchers` | ReadOnly | `{ status?, q?, limit? }` | `{ vouchers: Voucher[] }` |
| `list_ai_audit` | ReadOnly | `{ limit? }` | `{ entries: [...] }` |
| `list_work_calendar` | ReadOnly | `{ year?: int }` | `{ year, months[], workdays, holidays, kinds[] }` |
| `set_work_calendar_day` | NeedsConfirm | `{ on_date, kind, name? }` | `{ on_date, kind, kind_label, default, changed, leave_overlaps }` |
| `refresh_work_calendar` | NeedsConfirm | `{ year?: int }` | `{ year, source, read, written, kept, leave_overlaps }` |
| `list_regulations` | ReadOnly | `{ as_of?: date }` | `{ as_of, domains: [{domain, label, applicable, version, versions: [{version, effective_from, effective_to, source_note, in_force, payload, fields}]}] }` |
| `save_regulation` | AdminOnly | `{ domain, version, effective_from, effective_to?, source_note, payload }` | `{ domain, label, version, effective_from, rulesets }` |
| `delete_regulation` | AdminOnly | `{ domain, version }` | `{ domain, label, version, rulesets }` |
| `refresh_regulations` | AdminOnly | `{}` | `{ read, applied: [{domain, label, version, effective_from}], rulesets }` |
| `draft_voucher` | NeedsConfirm | `{ date, memo, lines: LineInput[] }` | `{ voucher }` 狀態 `draft` |
| `update_voucher` | NeedsConfirm | `{ no, date, memo, lines: LineInput[] }` | `{ voucher }`；分錄整份取代，跨月時 `no` 換號 |
| `post_voucher` | HighRisk | `{ no: string }` | `{ voucher }` 狀態 `posted` |
| `void_voucher` | HighRisk | `{ no, reason }` | `{ voucher }` 狀態 `void`；分錄不動，理由進稽核摘要 |
| `reverse_voucher` | HighRisk | `{ no, date }` | `{ voucher, original }`；新傳票狀態 `posted` |

### 未過帳傳票的修改

`update_voucher` 收整份分錄取代原有的，不做逐行比對：一張傳票只有整組才平衡，
而一行分錄沒有自己的識別，也沒有任何東西指著它。守門條件與 `discard_voucher`
共用 `ensure_unposted_and_unowned`——狀態是 `draft` 或 `pending`，且來源是 `manual`
或 `ai_draft`。少了這個操作，「待確認」是條死路：AI 草擬的科目要嘛照原樣過帳，
要嘛整張刪掉重打，而總覽上那句「N 張 AI 草稿待確認會計科目」正是要人去改它。

分錄的寫入與檢查由 `write_lines` 與 `insert_voucher` 共用，所以改過的傳票與新開的
一樣要通過 `validate_entry`、科目可入帳檢查與資料庫的遞延約束。`hydrate` 因此補回
`dept_code`：不回這一欄，畫面帶著讀到的內容送回來就會把部門清成未指定。

**日期改到別的月份就換號碼**：號碼的 `V{yy}{mm}` 段說的是它屬於哪一期，留著舊號碼
等於讓一張八月的傳票掛著九月的號。換號取該月份 `doc_sequence` 的下一個，舊號碼空著
不再使用，與刪掉一張傳票留下的缺號是同一件事。回應把新號碼帶回去，畫面照實說。

MCP 這一項開著（`needs_confirm`）：草稿本來就是 AI 寫的，讓它接著把科目改對是同一段
對話裡的事，而確認由用戶端那一關把關。

### 傳票更正

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

### 報表與設定模組

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
| `reset_user_password` | AdminOnly | `{ username, confirm: true }` | `{ user, temporary_password, revoked_sessions }`；舊密碼與該帳號所有登入立刻失效；拒絕呼叫者自己的帳號與只用外部帳號登入的帳號 |
| `delete_user_account` | AdminOnly | `{ username, confirm: true }` | `{ user, remaining }`；session 與外部身分連帶刪除，最後一個帳號與最後一個啟用中的管理員擋下 |
| `list_user_accounts` | AdminOnly | `{}` | `accounts[]`：登入名稱、角色、連結員工與其在職狀態、帳號狀態、`must_change_password`、`has_password`、最後登入；**無密碼雜湊** |
| `list_approval_rules` | ReadOnly | `{}` | `steps[]`、`enforced: true` |
| `get_backup_setting` | AdminOnly | `{}` | 目的地、保留天數、上次結果、`blocked_reason`、`restore_key_to_save`（只回一次）；**無存取密鑰** |
| `update_backup_setting` | AdminOnly | `{ enabled?, endpoint?, region?, bucket?, prefix?, access_key?, secret_key?, keep_days? }` | `{ bucket, enabled, blocked_reason }`；`secret_key` 不帶＝沿用 |
| `confirm_backup_key` | AdminOnly | `{}` | `{ blocked_reason }`；清掉待保存的私鑰，備份隨即開始 |
| `reset_backup_key` | AdminOnly | `{}` | `{ blocked_reason }`；清掉兩半金鑰，下一次備份重新產生一組 |
| `list_api_tokens` | ReadOnly | `{ include_revoked? }` | `tokens[]`（**無明文**）、`storage_note`、`wiring_note` |
| `issue_api_token` | AdminOnly | `{ name, kind, scope, actor, actor_role, rotate_days?, expiry_note? }` | `{ token, secret }`，`secret` 只顯示這一次 |
| `revoke_api_token` | HighRisk | `{ id }` | `{ token }` |
| `get_system_health` | ReadOnly | `{}` | `migration`、`stats`、`rulesets[]` |

報表三個操作、讀取類全部 `ReadOnly` 並對 AI 開放；`update_company_setting`、
`set_role_permission`、`issue_api_token` 在 `mcp_tool_grant` 中**預設關閉**：能自己
開通自己權限的 AI 等於沒有權限系統。

### 主檔與應收應付建立操作

| name | risk | input／行為重點 |
|---|---|---|
| `create_account` | NeedsConfirm | `report_group` 與 `cash_flow_section` 都必填；損益科目明確送 `null`。`normal_side` 依 kind 採慣例值，但可覆寫 |
| `update_account` | NeedsConfirm | 完整更新或以 `is_active = false` 停用；不提供刪除，既有分錄仍保留 |
| `create_employee` | NeedsConfirm | `id_no` 驗檢查碼後完整保存；三個投保級距照申報事實保存；`bank_accounts` 與 `dependents` 一次收整份清單；`pay_basis`（`monthly`／`hourly`）與 `insured_as`（`employee`／`owner`）不帶時取前者；`birthday` 必須早於到職日；不建立登入帳號（另有 `create_employee_login`） |
| `update_employee` | NeedsConfirm | 除四個金額外完整更新（`birthday` 也是，不帶＝清空）；`base_salary` 與三個投保級距是三態（不帶＝不動、`0`＝清為尚未設定、給數字＝更新）；`bank_accounts` 與 `dependents` 不帶＝不動、給清單＝整份取代；`leaving`／`left` 必須有 `left_on`；不修改登入帳號 |
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

### 固定資產：登記

`list_fixed_assets`、`preview_depreciation`、`run_depreciation` 原本就在，但沒有
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

### 費用與文件模組

| name | risk | input | output 重點 |
|---|---|---|---|
| `list_expense_claims` | ReadOnly | `{ include_done?, limit? }` | `claims[]`（含 `required_steps`、每一關的 `reason` 與 `receipts`）、`flow[]`、`thresholds_source` |
| `submit_expense_claim` | NeedsConfirm | `{ title, amount, spent_on?, dept_code?, suggested_account_code?, … }` | `{ claim }` |
| `approve_expense_claim` | HighRisk | `{ claim_no, account_code? }` | `{ claim, voucher, step_taken, finished }`；月底彙總匯款的單據同一交易寫一筆 `ap_bill`，帳單號碼即單號 |
| `reject_expense_claim` | NeedsConfirm | `{ claim_no, note? }` | `{ claim }` |
| `get_budget_status` | ReadOnly | `{ version?, from?, to? }` | `rows[]`、`budget_total`、`actual_total`、`alert_percent`、`actual_source`；一版都還沒建時 `version` 為 `null`，回空狀態而不是錯誤 |
| `get_department_pl` | ReadOnly | `{ from?, to? }` | `rows[]`（含 `unallocated`）、`total_profit`、`statement_profit`、`reconciled` |
| `create_budget_version` | BossOnly | `{ code, label?, fiscal_year?, is_current?, note? }` | `{ version }` |
| `set_budget` | BossOnly | `{ account_code, amount, version?, dept_code? }` | `{ budget }` |
| `list_documents` | ReadOnly | `{ q?, limit? }` | `documents[]`、`stats`、`bucket`、`bucket_reconciled` |
| `upload_document` | NeedsConfirm | `{ upload_id \| (file_name, content_base64), kind?, link_type?, link_ref?, doc_no?, name? }` | `{ document, new_version }`；兩種來源擇一，帶 `upload_id` 時 `file_name` 可省 |
| `get_document` | ReadOnly | `{ doc_no, version?, include_content? }` | `{ document, verified, checksum_recomputed }` |
| `request_upload_link` | NeedsConfirm | `{}` | `{ batch_id, upload_page, upload_endpoint, upload_path, curl, expires_at, max_files, max_file_bytes }` |
| `list_uploads` | ReadOnly | `{ batch_id }` | `{ batch, files[] }`；`files[].filed_as` 是存成的文件編號 |
| `list_imports` | ReadOnly | `{}` | `sources[]`、`batches[]`、`csv_columns` |
| `preview_import` | NeedsConfirm | `{ source_code, file_name, content }` | `{ batch, drafts, can_commit, blocked_note }` |
| `commit_import` | HighRisk | `{ batch_no }` | `{ batch, posted: false }` |
| `rollback_import` | HighRisk | `{ batch_no }` | `{ batch, removed }` |

`approve_expense_claim`、`commit_import`、`set_budget`、`create_budget_version` 在
`mcp_tool_grant` 中**預設關閉**：自己送自己簽等於沒有簽核流程；一次寫進幾百張傳票，
而復原只救得了未過帳的；預算是老闆定的，AI 不該改掉自己的比較基準。
`rollback_import` 維持開啟，理由與 `revoke_api_token` 相同。

**`create_budget_version` 補的是空狀態的出口**：一版都還沒建時
`get_budget_status` 回的是一句「請先建立一版」，而畫面上沒有任何按鈕可以建——
那句話指的動作做不到。現在該操作與 `set_budget` 同一條 permission、同一道底線，
理由也一樣：版本是每一個執行率的分母。

**月底彙總匯款的報支單同時是一張應付帳單**。最後一關產生的傳票貸 2101，
而應付帳款頁比對的是 `ap_bill` 明細合計與 2101 的總帳餘額，所以只動總帳的那一筆
會讓兩邊永遠差一個報支金額；畫面上也修不掉，因為「新增應付帳單」一定會自己再開一張
傳票，補一張只會把差額變成兩倍。核准因此在同一個交易裡寫 `ap_bill`，帳單號碼就是
報支單號（`EX2608-001`），供應商記申請人，到期日為傳票月份的月底。這也是這筆錢
唯一的匯出路徑——付款單只讀 `ap_bill`。零用金支付的單據貸 1101，錢已經付掉，不建帳單。

**能不能簽由後端回答，不由畫面推**。`list_expense_claims` 逐列附上 `can_approve`
與 `approve_blocked_reason`，`list_leave_requests` 與 `list_overtime_requests` 附上
`can_decide`／`decide_blocked_reason`、`can_approve`／`approve_blocked_reason` 與
`can_revoke`／`revoke_blocked_reason`；`false` 的那一列畫面**畫不出按鈕，改印那一句原因**。原本
按鈕一律是亮的，按下去才回一句拒絕，而頁面上還寫著「無法簽核時，下方會顯示原因」
——原因其實是螢幕底部一閃即逝的浮動訊息。規則只有一份（`may_decide` 與
`decide_leave_request` 本身），畫面讀的是它的結論。

**核准與駁回不是同一個問題**。`list_punch_corrections` 另回 `can_approve` 與
`approve_blocked_reason`：一張下班補登，當天沒有上班卡時核准不了——寫入端
（`punch_in_tx`）拒絕沒有上班卡的下班卡，而人資原本要按下去才知道。駁回仍然開著，
那張單很可能正是該退的一張，所以擋的是核准鈕而不是整列。

**申請人不得核准本人送出的單據，休假與報支同一條規則**。休假原本沒有這道限制，
於是同一套「職責分離」在兩條流程上給出相反的答案。休假比對的是
`leave_request.employee_id` 與 `ctx.employee_id`（`decided_by` 是顯示名稱，會同名，
不能當鍵）。一人公司因此兩條都簽不動，這是正確答案而不是缺口：拒絕訊息直接說出唯一
的出路——請系統管理員在「帳號管理」建立第二個帳號，因為建帳號是 `AdminOnly`，
負責人自己做不到。

**負責人自行核決是公司設定，只放行負責人**：`company_setting.owner_self_decide`
（`由他人簽核`／`可自行核決`，種子值是前者）。負責人持有休假、加班、補登與報支的
全部核准權限，所以其他角色送出的單一定有人簽得動，**卡住的只會是負責人本人的單**；
全域放行等於讓主管簽掉本人的報帳、人資准掉本人的假，而那三種角色本來就不會被卡。
判斷只有一處：`ops/settings.rs` 的 `owner_decides_own(role, setting_on)`。休假、加班、
補登由 `deciding_as(ctx)` 交給 store 一個 `Option<i32>`，放行時是 `None`——store 那三道
檢查不動，仍是最後一道防線；報支由 `may_decide` 自己比對。拒絕訊息依角色分兩種
（`own_request_refusal`）：負責人指向「基礎設定」，其他人仍指向「帳號管理」，store
回的那一句在 ops 換掉。不另存「自行核決」欄位：單子上申請人與決定者相同就是。
**不做「偵測沒有第二個可簽核帳號就自動放行」**：行為會自己變，而且「沒有第二個帳號」
與「第二個人在休假」是兩件事，後者更常見而且照樣卡住。

**報支的「本人」有兩把鑰匙**：`expense_claim.employee_id` 相同，或 `claimant` 等於
呼叫者的顯示名稱，任一成立就是本人。自助送出的單帶員工編號；`submit_expense_claim`
代他人建單只有名字（`employee_id` 是 NULL），少了名字那一條，負責人在財務畫面以自己
的名字代建一張就簽得掉，開關形同虛設。名字那一條擋不住打別人的名字，但那筆錢就記在
別人名下的應付帳款，看得出來。

**休假是一段時間，時數與天數由它算出來**。原本的請假單是兩個日期加一個
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
程式卻是先讀再無條件更新。現在在交易裡以 `FOR UPDATE` 讀那一列再更新，兩位主管同時按
下去只有一位會成功（見「撤回與撤銷」）。

### 計薪基準與投保身分

員工主檔有兩個獨立的欄位，兩者都不是 `employment_type`：

- **`pay_basis`（`monthly`／`hourly`）決定 `base_salary` 是什麼**。月薪制照曆日
  比例計薪（在職日 ÷ 當月日數），時薪制以打卡工時計薪。**它與 `employment_type`
  是兩個軸**：兼職可以領月薪，正職也可能按時計酬，所以計薪不看僱用型態。
  `part_time` 原本只印在名冊上，沒有任何計算讀它，於是兼職員工領的是一份
  按曆日比例攤開的月薪。
- **`insured_as`（`employee`／`owner`）決定套哪一組分擔比率**。見上文
  「負責人投保是獨立 domain」。

時薪制的四個後果：

1. **工時只採完整的打卡日**（`PunchDay::worked_seconds`，休息時間已扣除）。只打上班
   沒打下班的那一天長度不明，猜一個等於在上面掛錢；那是漏打卡，補登流程負責。
2. **本期沒有完整打卡的時薪員工不產生薪資單**，列進 `skipped` 並說明原因。一張 0 元
   薪資單看起來像算出來的金額，而不是還沒有資料。
3. **加班費以本人的時薪計價**：`ordinary_hourly_wage` 對月薪制除以 `monthly_hours`，
   對時薪制直接回傳本人時薪。少了這一步，時薪員工的加班費會被除以 240，一小時加班
   反而比一小時正常工時便宜。
4. **本薪扣掉核准加班與打卡時段重疊的部分**（`ordinary_hours`）：第 24 條的
   1.34／1.67 已經含那一倍本薪，而打卡涵蓋加班時段，不扣就付兩次。只扣重疊，不扣
   加班時數：打了下班卡才留下來加班、或那天漏打卡，加班都不在打卡工時裡，照時數扣
   會把別的時段的本薪扣掉。重疊裡的休息時間也不扣，它本來就不在打卡工時裡。另一種
   做法是加班費只付加給（倍率減 1），合計相同，但薪資單的加班費欄會對不上法定倍率，
   漏打卡的那天加班只拿到加給、低於法定。

**最低工資的下限跟著 `pay_basis` 走**：`minimum_wage` 的 `monthly` 與 `hourly`
是兩個並列的下限，互不涵蓋。時薪拿去比月薪下限必定不合格，月薪拿去比時薪下限則永遠
合格——兩種寫法都讓那個檢核項目說不出任何事。

**特休年度從到職日起算（週年制），額度與已休算同一段區間**。原本額度依到今天為止的
年資取級距、已休卻依 `from_on` 的曆年加總，一過到職週年，上一年度用掉的天數就從新年度
的額度裡扣，而員工照那個數字排假、人資照它核假。
- **區間**：`laysi_regulation::annual_leave_year` 以年資月數回答 `[起, 迄)`——`annual_leave`
  每個級距的月數與每個滿週年都是起點，所以未滿六個月是一段（額度 0）、六個月到一年是一段
  （3 日），之後每滿一年一段。`ops/attendance.rs` 的 `annual_leave_year()` 換成日期，起日
  與 `service_months()` 同一套曆法：月底到職而某個月沒有那一天時，從次月 1 日起算，名冊印的
  年資與區間不差一天。額度仍以查詢當日的 `annual_leave` ruleset 計算；離職員工停在離職那天
  所在的年度。
- **已休**：只加總 `status = 'approved'` 的特休落在區間內的日期，撤回、撤銷與待簽核都不算。
  跨區間的單由 `leave_by_date()` 拆開，與計薪攤日是同一個函式；天數存到兩位小數，所以每一段
  取「區間結束時的累計值」減「區間開始前的累計值」（各自取兩位），相鄰兩段加起來恆等於單上
  存的天數。
- **拆法在送出當下定案**：`leave_request_day` 一張單一個日期一列，存那一天的上班秒數，與
  `hours`／`days` 在同一個交易寫入（`WorkSchedule::leave_seconds_by_date`）。`leave_by_date()`
  只讀這張表，不讀現行的上下班時間與工作日曆——原本每次讀取都依現行設定重拆，清掉上班時間
  之後整張單落到第一天，跨週年的單就有一天從一個特休年度搬到另一個，計薪的期別也一樣會搬。
  表建立之前的單由建表的那份 migration 依當時的上下班時間與工作日曆以 SQL 補寫，算法與
  `leave_seconds_by_date` 相同（`tests/annual_leave_year.rs` 比對兩者）；那一刻每一天都沒有
  上班時數的單沒有列，讀成整張落在第一天。工作日曆事後改動不改寫這張表（見「工作日曆」）。
- **四處同一個答案**：休假頁、名冊、個人專區／我的休假／休假申請、法遵檢核都走
  `annual_balances()`／`annual_balance()`，同一人同一天回同一組數字，回應帶區間起訖
  （`from_on`／`to_on`，名冊是 `annual_leave_from`／`annual_leave_to`），畫面印出區間。
  休假頁上每張特休單附的是**該單開始那天**所在年度的餘額——週年之後才休的單比的是新年度。
- **不在系統裡**：曆年制（公司選擇以曆年計算並按比例給予）與年度終結未休日數的折發或遞延；
  `annual_leave` 的 `settlement` 只是說明。

**特休不檢核兼職與時薪員工**：部分工時的特休按約定工時比例給予，而本系統沒有記錄
約定工時。以全時額度去比，等於每一個兼職員工都通過。檢核項目照實把他們排除並在
`scope_note` 說有幾位。

**負責人的健保投保金額不得低於受僱員工最高者**（健保法第 20 條），列為
`get_hr_compliance` 的一個檢核項目而不是寫入端的驗證：名冊存的是申報事實，
在鍵盤上擋下一個數字不是這一層的事，但那個數字要有人說出來。名冊上沒有負責人時
該項回 `checked = false`。

### 人資模組

| name | risk | input | output 重點 |
|---|---|---|---|
| `get_hr_dashboard` | ReadOnly | `{}` | 在職人數、待辦（休假單、加班單、補登申請）、應計期別尚未計算薪資的提醒與人資／財務銜接狀態 |
| `list_employees` | ReadOnly | `{ q?, include_left? }` | 名冊、生日、特休額度與所在特休年度的起訖、約定的底薪與三個投保級距，另回 `departments` 供編輯抽屜的部門下拉、`relations` 供眷屬的關係下拉、`has_login` 決定畫不畫核發帳號、`insured_suggestion` 供抽屜帶入級距；**不含系統角色**；MCP 通道另行遮蔽（見下） |
| `list_employee_documents` | ReadOnly | `{ emp_no }` | 該員工的附件清單與可用類型；內容要另外取回 |
| `upload_employee_document` | NeedsConfirm | `{ emp_no, file_name, content_base64, name?, kind?, doc_no?, note? }` | `{ document, new_version }`；`doc_no` 只收這位員工自己的附件 |
| `get_employee_document` | ReadOnly | `{ emp_no, doc_no, version?, include_content? }` | 重算雜湊比對；不屬於這位員工的編號回查無此附件 |
| `create_employee_login` | NeedsConfirm | `{ emp_no, email }` | 角色固定一般職員；`{ username, role_name, temporary_password }` |
| `list_leave_requests` | ReadOnly | `{ include_done?, limit?, month?, emp_no? }` | 請假單與依 `annual_leave` ruleset 現算的逐人額度（含特休年度起訖），每張特休單另附該單開始那天所在年度的餘額；帶 `month`／`emp_no` 只回碰到該月或該員的單，`other_pending` 是沒列出的待簽核張數 |
| `decide_leave_request` | NeedsConfirm | `{ leave_no, approve, note? }` | `{ request, action }`；單據所在月份到當年年底之間，申請人在任一期已有薪資單時不能准假，駁回不受影響 |
| `revoke_leave_request` | NeedsConfirm | `{ leave_no, reason }` | `{ request }`；只收已核准的單，理由必填；單據所在月份到當年年底之間，申請人在任一期已有薪資單時拒絕（見「撤回與撤銷」） |
| `list_overtime_requests` | ReadOnly | `{ month?, include_done? }` | 加班申請、逐人本月已核准時數與加班費，加班費現算 |
| `decide_overtime_request` | NeedsConfirm | `{ request_no, approve, note? }` | `{ request, action }`；申請人在加班日所在期別已有薪資單時不能核准，駁回不受影響 |
| `revoke_overtime_request` | NeedsConfirm | `{ request_no, reason }` | `{ request }`；只收已核准的單，理由必填；申請人在加班日所在期別已有薪資單時拒絕 |
| `get_payroll_summary` | ReadOnly | `{ period? }` | 薪資彙總與總帳對帳；不含個人金額；`next_period` 是尚未計算的應計期別，算過就是 `null` |
| `list_payslips` | BossOnly | `{ period? }` | 逐人薪資與保費重算對帳；重算用薪資單存下的 `health_dependents` 與投保期間（`insured_from`／`insured_to`），不用現在的眷屬名單與離職日 |
| `run_payroll` | HighRisk | `{ period?, pay_on?, bank_account_id?, note? }` | 薪資單與待過帳傳票；`period` 不帶時為應計期別；已核准的休假依 `leave_pay` 扣減或計入本薪，`attendance_checks` 逐人列出曠職、未打下班、待簽核休假與時薪員工的國定假日（不改金額）；健保自付額依期末日在保的眷屬逐口加計，口數存進 `payslip.health_dependents`；到職與離職當月勞保、勞退按日計，月中離職健保不收，投保期間存進 `payslip.insured_from`／`insured_to` |
| `void_payroll_run` | HighRisk | `{ period, reason }` | 作廢整期：`{ period, headcount, voucher_nos }` |
| `get_insurance_summary` | ReadOnly | `{ as_of? }` | 逐人投保級距與三項費額；費率、`health_dependents`（計入的眷口）與 `health_dependents_covered`（在保的眷屬）一律以 `as_of` 所在月份的月底計，回應的 `as_of` 是那一天；`labor_days`／`pension_days`（整月為 null）與 `health_charged` 是該月按日計的結果；`dependents_cap` 供試算的眷屬下拉 |
| `compute_insurance` | ReadOnly | `{ insured_salary, health_insured_salary?, pension_wage?, insured_as?, dependents?, as_of? }` | 單一級距試算與三份 ruleset；`dependents` 是在保眷屬人數，回 `health.dependents_charged`；`insured_as = "owner"` 另回 `owner_ruleset` |
| `get_hr_compliance` | ReadOnly | `{ as_of? }` | `checks[]`；逐項帶 `checked`、`passed`、ruleset 與差異 |
| `list_withholding_statements` | BossOnly | `{ year? }` | 逐給付對象年度所得；`table` 稅額缺值保持 `null` |

法遵檢核的 `checked_all_passed` **只涵蓋實際算過的項目**。完整投保薪資級距、
國定假日與例假出勤工資尚未收錄，會以 `checked = false`、`passed = null` 與
`not_checked_reason` 回傳，畫面不得把它改寫成「全部法規皆符合」。每月延長工時
上限是**實際檢查**的項目：逐人加總當月已核准的加班時數，與 `overtime`
ruleset 的上限比較。

畫面依 `checked`／`passed` 分成待確認、未檢查、通過三段，**待確認排在最前面**，
所以檢核項目的先後由狀態決定而不是由 Registry 的排列決定。每一個算得出來的項目
帶 `screen`（`hr` 或 `hr_att`，與人資總覽同一組代碼），畫面據以畫出前往修正的
按鈕；**因此 `failures[].message` 不寫「請到某某畫面」**，路徑由按鈕承擔，訊息
只講哪裡不對。未檢查的項目沒有 `screen`：沒有東西可以修，一顆按鈕反而是假出口。

「勞健保與勞退費額可依規則重算」不是檢核項目。它的 `passed` 恆為 `true`——級距為
0 時重算出 0 也回報沒有差異，而規則缺漏會讓整個操作先失敗——它唯一的內容
（三項公司負擔合計）就是「勞健保與勞退」那一頁本身。永遠亮著的綠燈比沒有燈更糟，
所以連同 `evidence` 一起移除。同一條判準也移除了 `coverage_note`（說明版面配置）、
`coverage_complete` 與 `available_domains`（沒有讀取端）、`missing_bracket_count`
（等於某一項的 `failures` 筆數）。

年度扣繳憑單以 `withholding_record` 的 `payee`／`payee_id` 分組。`basis = rate`
依給付日 ruleset 計算，`basis = table` 只採來源系統帶入的
`withheld_amount`；來源沒帶就保留 `null`。來源若只有多人合併總額，回應會標成
`person_specific = false`，不自行拆出一份不存在的逐人資料。薪資模組的
`payroll_year_totals` 另列為 `employee_income`，並與**薪資所得代號（50）**的紀錄
對帳；兩邊若不同就回報差額，不用其中一邊覆蓋另一邊。

**這一段是負責人才叫得動的操作，所以畫面靠 `permissions` 決定叫不叫**，不再送出
一個註定被拒的請求再把錯誤印出來。對帳結果畫成一張兩列的比較表（薪資模組／扣繳
紀錄／差額），`employee_income` 收在「逐人明細」開關後面——它是差額的佐證，不是
這一頁的主體。`payroll_reconciliation.note`、`table_note`、`source_note` 與逐列的
`source_note` 一併移除：對帳結論由差額欄位自己說，稅額缺值與「可能為多人合併」
都已經是該列上的標記。

**應計期別由發薪時點決定。** `company_setting.payroll_timing` 是「當月」或「次月」，
`payroll::due_period` 依它與今天算出應計期別：當月是今天所在的月份，次月是前一個月
（2026-01 的前一個月是 2025-12）。薪資作業的期別預設（`get_payroll_summary` 的
`next_period`）、`run_payroll` 不帶期別時的預設，以及人資總覽的「尚未計算薪資」待辦，
三處都經 `unpaid_due_period`，不各自寫一份。原本三處都問「本月算過沒有」，次月發薪
（10 月初發 9 月薪）的公司打開薪資作業看到的是 `2026-10`；期別決定保費的期末日與出勤、
加班、休假的讀取範圍，沒改到就算成另一個月，而算完 9 月之後人資總覽整個 10 月掛著一則
消不掉的提醒。**不由計算紀錄推導**：第一次計薪時沒有紀錄可看，而且次月發薪的公司照樣
整月被提醒本月尚未計算。種子是「當月」，與原本的行為相同。期別欄空著時計算按鈕不能按
——原本空著按下去由後端補成本月，按鈕上寫的卻是「計算薪資並產生傳票」而不是補算。

**一期不只一次計算。** `payroll_run.period` 沒有 UNIQUE，讓計薪之後才到職
的人仍領得到那幾天的錢，`run_payroll` 改成跳過已經持有薪資單的人。但
`record_payroll_run` 裡那條「這一期已經有作業了就回錯」留著沒刪，等於在 Rust 裡
重建剛拿掉的約束：畫面上那句「這次只算後來到職或當時漏算的人」與它的按鈕，按下去
必定失敗。同一次移除的還有 `PeriodAlreadyPaid`。**已過帳的期間補算是唯一的路**——
作廢在那裡被擋下，所以兩個機制不是二選一。連帶的三件事：`payroll_runs` 的排序加上
`p.id DESC`，一期之內才有定序；`record_payroll_run` 改回傳它剛寫的那一列（依 id
取，不是依期別再查一次，否則答案是該期排在最前面的那一列）；`get_payroll_summary`
與 `list_payslips` 的 `run` 改成**該期各次計算的合計**（`period_json`），各次的傳票、
規則版本與計算人員另以 `period_runs` 逐次回傳，期別選單則逐期只列一次。畫面因此
一期一張總計卡，底下一段傳票一段規則版本，而作廢的按鈕要該期**每一張**傳票都還沒
過帳才畫。

**改主檔之後重算，只到總帳為止。** 到職日改了，那一期的薪資單仍然照舊的
天數按曆日比例計薪，而回頭的路一條也沒有：`run_payroll` 跳過已經持有薪資單的人，
`discard_voucher` 拒絕不是手開的傳票並指向產生它的那一頁，而那一頁沒有這個動作。
`void_payroll_run` 就是它，作廢一整期：薪資單（`payroll_run` 級聯）、扣繳紀錄與
傳票在同一個交易裡刪掉，之後重新計算。刪除順序由外鍵決定——`withholding_record`
與 `payroll_run` 都指向傳票，所以兩者先走。**界線在總帳**：只要該期任何一張傳票
不是待過帳就整個拒絕，已過帳的金額報表已經算進去了，那裡的更正是下一期的差額而
不是刪除。理由必填並寫進稽核摘要，與 `void_voucher` 同一個 `require_void_reason`。
畫面畫不畫這顆按鈕看 `permissions` 與傳票狀態，兩個條件缺一就不畫。

**`run_payroll` 在同一個交易裡寫入這些給付紀錄。** 沒有它，扣繳頁永遠寫著「對不上，
差一整年的薪資」，而畫面上沒有任何動作可以消除——那不是對帳，是一個做不完的待辦。
`basis` 記 `rate`：稅額本來就是由給付日的 ruleset 以扣繳率現算，存一份等於同一個問題
有兩個答案，而 `withholding_record` 的 CHECK 也正是為此只在 `table` 時要求稅額。
對帳因此改看所得代號而非 `basis`——依扣繳率算出來的紀錄一樣是薪資所得。

**計薪讀已核准的休假單，給付比例在 `leave_pay`。** 原本 `compute_one` 只按曆日比例與
打卡工時算，事假、病假照發全薪，而時薪員工的特休與病假一毛不給。現在每張已准的單
依送出當下記下的各日上班時數比例（`leave_request_day`）分攤到它碰到的每一天——分攤的
是單上存下的時數與天數，兩者與拆法都在送出當下定了——再由 `laysi_regulation::leave_paid_days` 依各日適用的 `leave_pay` 回答那一天
付了幾天的工資。月薪制扣掉沒付的那一份，一日是月薪除以當月曆日，與到職當月按曆日
比例計薪同一套；時薪制加上付了的那一份時數。每個假別各自四捨五入成一筆，寫進薪資單
備註，所以備註加得起來等於本薪的變動。
**病假的每年 30 日折半與生理假的併入按曆年算**（使用者決定），從 1/1 起逐日累計，所以
`run_payroll` 讀當年 1/1 起的休假單，不只是本期的。產假看請假當天的年資。
**`attendance_checks` 只列不扣**：曠職可能是漏打卡、待簽核的單可能被駁回、兼職排班日
遇國定假日要有約定排班才知道（#3），三者都要有人看過，而自動扣錯人比晚一期補發更糟。
**新領域要到得了既有安裝**：`seed_regulations()` 補寫沒有任何版本的領域，並以 feed 的
同一個名稱記進 `reg_update`；不然升級後第一次計薪就停在「查無 leave_pay」，要等管理員
按「檢查法規更新」。

**名冊回傳底薪與三個投保級距。** 原本不回，於是全系統沒有一頁看得到某個人現在的
本薪：調薪要憑記憶，編輯抽屜也無法把「留空＝不動」的那個「原值」顯示出來。這不是
放寬權限——同一組權限底下的 `get_insurance_summary` 本來就逐人列出三個投保級距，
而持有 `view_hr` 的只有負責人與會計，兩者都已看得到薪資明細。逐月薪資單仍在
`list_payslips`，仍限負責人。

**離職與關掉存取是同一件事（`close_access`）**。`update_employee` 把狀態改成 `left`
時，同一個交易接著把他的登入帳號停用、撤銷未過期的 session，並撤銷
`api_token.owner_employee_id` 指向他的 MCP Token。三件事必須一起做：session 與自助
Token 雖然都要求帳號啟用中，但那只在帳號停用期間成立——日後以同一角色回任，兩者就一起
回來，而只是被擋著的憑證也不會留下存取在哪一刻結束的紀錄。唯一的例外是**最後一個啟用中的系統管理員**
——沒有任何操作能把角色給出去，停用它等於誰也打不開帳號管理，因此那次更新直接拒絕並
要求先建立另一個管理員帳號。`update_employee` 以 `closed_access` 回報實際關掉了什麼，
畫面照實說出來；`list_user_accounts` 另外回連結員工的在職狀態，讓帳號管理不必跳到
名冊才看得出這個人已經離職。

這條銜接原本掛在異動單的離職流程最後一關（`hr_flow_step`／
`employee_movement`）。整套異動單已經刪掉：四種異動裡，調薪與調部門結案時只寫
一個欄位，而名冊的編輯抽屜一步就寫得到同一個欄位，也沒有任何機制要求走異動單；到職
流程結案不寫任何東西；`hr_flow_step` 全 codebase 沒有寫入路徑，十五列是穿著資料表
外衣的常數。真正沒有第二條路的只有離職這段交易，於是它搬到 `update_employee`——那正是
原本繞得過去的那條路。

`mcp_tool_grant` 的名稱必須與上述五個人資操作模組在 Registry 裡的名稱集合完全
相等；`laysi-ops` 以 `include_str!` 直接讀 migration 的測試守住這條契約。

**名冊在 MCP 通道回得少**：`list_employees` 在畫面上回完整的身分證字號、底薪、
三個投保級距與銀行帳戶，因為編輯抽屜要顯示它即將取代的值，而全系統沒有第二頁
說得出某人現在領多少。這幾項後來才加進去，於是那個工具的 grant 備註「不含薪資」
從此是假的，而一把 MCP Token 就把整份名冊倒進對話裡。`ctx.channel == Channel::Mcp`
時因此把四個金額、銀行帳戶與眷屬名單整個拿掉，身分證字號改回遮罩過的形狀——遮罩而不是刪掉，
是為了名冊仍答得出某人有沒有登錄字號。這條分流不需要新的模型或新的操作：`Channel`
本來就是「同一個能力，人與 AI 兩種答案」的分界。

**員工附件走三個自己的操作，不是把 `manage_documents` 給人資**：證件掃描檔、簽好的
合約與人事文件用的是同兩張表、同一個桶子與同一次 BLAKE3 比對，差別只在入口。
`manage_documents` 一併打開全公司的收據、供應商帳單與銀行文件，那不是人資該持有的
集合；而一個操作只能屬於一條 permission，所以「同樣三個操作、換一組權限」這條路不
存在。`list_employee_documents`／`upload_employee_document`／`get_employee_document`
因此一律收員工編號、不收自由的文件編號，並在讀取時比對 `link_type = 'employee'` 與
`link_ref`——**別人的附件編號回的是查無此附件，不是拒絕**，拒絕會確認那個編號存在
而且說出它屬於誰。同一條檢查也擋住用 `doc_no` 往別人的附件加一版。

**人資核發得了帳號，但只發一般職員**：新人要打卡、請假、報帳就得有登入，而
`create_user_account` 是 `AdminOnly`，於是每收一個人都要等系統管理員。放寬的方式不是
把那個操作交出去——它收 `role_code`，交出去等於人資自己指派得了任何角色。
`create_employee_login` 把角色寫死成 `staff`、只認名冊上既有的員工編號、已經有帳號的
人一律拒絕，其餘每一項檢查（地址格式、登入名稱有沒有被佔用、密碼登入停用時的例外、
免密碼帳號的三個前提）與 `create_user_account` 共用 `check_new_login`／`write_login`，
所以兩條路不會長出兩套規則。檢查與寫入分成兩個函式，是因為 `create_user_account` 在
兩者之間還要建員工主檔，先寫再被拒會留下一筆沒人要的名冊資料。
**要不要密碼由伺服器判斷**：外部登入開著、設為自動建帳號、地址落在允許的網域內——三個
條件齊了就發免密碼帳號，否則給臨時密碼。人資不必知道這條規則，也不該替它做決定。
`has_login` 因此進了名冊的回應：畫面靠它決定畫不畫那顆按鈕，而 `list_user_accounts`
是 `AdminOnly`，人資問不到。

**投保級距是建議值，不是分級表**：抽屜在填入本薪時把三個投保欄位一起帶上，帶的是
`minimum_wage.monthly` 與 `labor_insurance.max_insured_salary` 夾出來的本薪——那是規則庫
現在僅有的兩個界線。**沒有把整份投保薪資分級表搬進來**：那是三份逐年更動的表格
（勞保投保薪資、健保投保金額、勞退月提繳工資），抄一份進來就是抄一份沒人看得出錯在哪
的數字，而抄錯的代價要到申報那天才出現。所以 `list_employees` 回的是
`insured_suggestion`（`floor`／`labor_ceiling` 加一句說明），畫面照實說「尚未對照分級表，
請依申報值調整」。**時薪制不帶入**：時薪換算成月投保薪資要知道約定工時，而系統沒有記錄
它。帶入只覆寫空的欄位、或還停在上一次建議值的欄位，人資改過的數字不會被下一次輸入蓋掉。

### 公告與規章

| name | risk | input | output 重點 |
|---|---|---|---|
| `list_notices` | ReadOnly | `{ id?, kind?, include_ended? }` | 現行版本；帶 `id` 回該則的每一個版本 |
| `create_notice` | NeedsConfirm | `{ kind, title, body, effective_on?, expires_on? }` | `{ notice, push }`，記為第 1 版 |
| `update_notice` | NeedsConfirm | `{ id, title?, body?, effective_on?, expires_on? }` | `{ notice, revised, push }` |
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

原本的做法是改寫規章就刪掉 `notice_ack`。**那條路把「誰在哪一天讀過哪一段文字」
銷毀，而那是勞資雙方事後唯一拿得出來的東西**，所以閱讀確認整套移除，改成保存
版本並讓任何看得到這一頁的人都讀得到舊版。`list_notices` 與 `list_my_notices` 帶 `id`
時回的是同一份版本清單，兩邊不許有不同的答案。

**未讀是「看到哪一刻為止」，不是簽核。** `notice_read` 一人一列只記時刻，沒有
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

### 佈告欄推送到聊天室

| name | risk | input | output 重點 |
|---|---|---|---|
| `test_notice_push` | AdminOnly | `{}` | `{ push }`，網址沒填就回錯誤 |

`company_setting` 的 `notice_webhook_url` 填了，`create_notice` 與改動內容的
`update_notice` 就把該則的標題、日期與內文 POST 出去；留空不推送，那是每個安裝的起始
狀態。**設定下架或廢止日不推**——那不是新的一版，收到的人也沒有新的東西可看。

**訊息形狀由網址的主機決定，不另設一個「平台」欄位。** Slack、Google Chat 與退場中的
Teams connector 讀 `text`，Discord 讀 `content`，Lark 與飛書讀 `msg_type` 底下的
`content.text`，Teams 走 Power Automate 收的是 Adaptive Card。Discord 的 `content` 是
字串而 Lark 的是物件，所以一份同時塞滿每個欄位的 payload 不存在。網址本來就說得出那是
哪一套工具，多一個要與它保持一致的欄位只是多一個填錯的地方；認不得的主機送 `text`，
中繼與自動化工具讀的也是那一個。

**推送與呼叫同步進行，結果回報給發布的人，不排隊也不重試。** 佈告欄本身就是紀錄，沒送
出去的訊息只值畫面上一句話，而排隊要一張表、一支背景工作與一組重試規則。內文超過 1800
字截斷並說明——Discord 的 2000 字上限是這幾套工具裡最低的。

**未生效的公告在發布當下就推**：系統裡沒有排程器，而訊息第一行就寫著生效日。排到生效日
當天才推，要在 `notice_version` 多一個欄位與一支每日工作，等有人真的需要再說。

**畫面是「基礎設定」底下自己一張卡，不是那一串設定裡的一列。** 網址是那一頁最長的
值，擠在 `fact` 那一行讀不完也編不動；卡片給它整行的輸入框、一顆儲存、一顆測試推送與
一顆清除。**「送往 Slack」由後端的清單決定**：`get_company_settings` 回
`push_targets`（`generic` 加 `[{domain, label}]`），畫面拿它比對正在輸入的網址主機，
所以說出來的那個名字與真的送出去的形狀是同一張表，不是 TypeScript 裡的一份抄本。

**`test_notice_push` 是獨立操作，不是重存一次設定。** 重存沒有變更的值會留下一筆
說「網址已更換」的稽核紀錄，而那句話是錯的；測試推送寫的是「測試公告推送 · 已推送到
Slack」。它與 `update_company_setting` 同屬 `manage_access`，對 AI 預設關閉。
上次推送的結果只活在畫面的 state 裡——存一欄要多一次寫入，而下一次按下去就會知道。

網址只收 `https://`（`ops::webhook::validate_url`），而它本身是憑證——拿到的人就能往公司
的聊天室貼文，所以稽核摘要與 `brand_logo` 一樣只寫「已更換」，不複製那個字串。存檔當下
送一則測試訊息，成敗直接回在畫面上：沒有人按過的網址就是沒有人知道錯在哪的網址，而下一
個送到那裡的是全公司的公告。

### 員工自助的 MCP

| name | risk | input | output 重點 |
|---|---|---|---|
| `issue_my_api_token` | NeedsConfirm | `{ label, scope?, rotate_days? }` | 明文只回一次；actor／role 取自 session |
| `list_my_api_tokens` | ReadOnly | `{ include_revoked? }` | 本人核發的 Token＋本人在 MCP 上可用的工具 |
| `revoke_my_api_token` | NeedsConfirm | `{ id }` | 所屬員工寫在 WHERE 條件裡 |

**「用 MCP」不是主管的特權。** `issue_api_token` 是 `AdminOnly`，而且輸入帶
`actor` 與 `actor_role`——就算給了職員權限，那個形狀也允許他指定 Token 代表誰。
自助版本兩個欄位都沒有，代表對象與角色一律取自 session，因此**這把 Token 能做的
事與本人完全相同，不會更多**：職員的 Token 就是職員。

`api_token.owner_employee_id`是所有權的鍵。`actor` 是顯示名稱、可能同名，
不能拿來判斷歸屬。只有 `issue_my_api_token` 會寫入這一欄，老闆代發的 Token 留
NULL。`revoke_my_api_token` 把員工直接寫進 `UPDATE ... WHERE` 與後續查詢，所以
**別人的 id 是「查無此列」，不是查到之後才拒絕**。

**這一欄同時補上 MCP 缺的那一半身分。** `mcp::token_identity` 原本一律回
`employee_id: None`，理由是「不能從姓名猜員工」——那個理由現在仍然成立，但
`owner_employee_id` 不是猜的，是本人在自己的 session 裡按下按鈕寫進去的。於是
五個唯讀自助工具因此對 AI 打開：沒有這一步，「我的 MCP」在全新安裝上會發出
一把什麼都不能呼叫的 Token。老闆代發的 Token 依然沒有 owner，自助操作對它一律
回「尚未連結員工編號」，不猜。

寫入類與薪資明細維持關閉。`punch_clock` 最明確：讓助理替你打卡的工具就是偽造出勤
紀錄的工具；`issue_my_api_token` 若對 AI 開放，Token 就能再生 Token，撤銷也追不上。

`no_self_service_input_can_select_another_employee` 的黑名單擋下了
`issue_my_api_token` 原本的 `name` 欄位——在這個模組裡 `name` 一直是人名。正確的
回應是把欄位改叫 `label`，不是替測試加例外。

### 打卡

| name | risk | input | output 重點 |
|---|---|---|---|
| `punch_clock` | NeedsConfirm | `{ kind, note? }` | 本人上／下班卡，時間取伺服器時鐘 |
| `list_my_punches` | ReadOnly | `{ month? }` | 本人逐日明細（該月每一天，帶日別、休假與曠職）與月統計 |
| `list_attendance_punches` | ReadOnly | `{ month?, emp_no? }` | 該月在職每一人的統計（含曠職天數）；帶 `emp_no` 另附該月每一天的明細 |
| `correct_punch` | NeedsConfirm | `{ emp_no, punched_on, kind, at, note? }` | 補登，`source = manual`；同一交易在 `punch_correction` 留一列已核准的紀錄 |
| `submit_my_punch_correction` | NeedsConfirm | `{ punched_on, kind, at, reason? }` | 本人送出補登申請；公司設定為自動通過時同一交易寫入打卡 |
| `list_my_punch_corrections` | ReadOnly | `{}` | 本人送出過的補登申請與狀態 |
| `list_punch_corrections` | ReadOnly | `{ include_done? }` | 全公司補登紀錄，逐列回 `can_decide`、`can_approve`、`replaced_at` 與 `hr_entered` |
| `decide_punch_correction` | NeedsConfirm | `{ id, approve, note? }` | 核准即在同一交易寫入打卡 |

`attendance` 一直有欄位放月統計，卻沒有任何東西產生它——打卡補上了被彙總的那個
原始事件，那張表則已刪掉。從頭到尾沒有任何操作、解析器或種子寫過它，
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

**月份切換一律用 `components/PeriodPicker`**（薪資作業、出勤與休假、我的薪資單）。
原本是一個月一顆 chip 排成一列且不換行，薪資算過兩年就是 24 顆，整頁被撐出水平
捲軸。元件把清單拆成「年份」與「該年的月份」上下兩列，一年最多十二顆，每一列都
可以換行；只有一個年份時不畫年份那一列，點年份跳到該年最近的一期。年份超過五個時
只畫最近三年的 chip，更早的收進同一列最前面的「更早」下拉選單（由新到舊），選到的
年份由選單本身亮起——不收的話，用到第十年時手機上光是年份與月份就有六列，內容要
往下捲才看得到。**目前顯示的
期別一定在清單上**：出勤的 `months` 只列有打卡的月份，預設顯示的本月還沒有打卡時
不在其中，於是沒有任何一顆亮著，切到別的月份之後也回不到本月。`Attendance.tsx`
因此記住第一次載入時後端給的本月，一併交給元件。

順帶一提，勞基法第 30 條第 7 項寫的是出勤紀錄「應逐日記載勞工出勤情形**至分鐘
為止**」，也就是到分鐘即已足夠；記到秒是超過而不是符合。程式與畫面因此都不宣稱
這是法規要求，只是照實記錄時鐘給的值。上下班時間（`work_start`／`work_end`）仍是
`HH:MM`，那是公司政策而不是事件。

**`punch_clock` 不收時間欄位。** 打卡記錄的就是「呼叫的那一刻」，能填時間就不是
打卡了。連 `LAYSI_TODAY` 也不移動它：那個環境變數是給示範安裝挪業務日期用的，
而打卡寫的是真實瞬間。漏打或打錯要走補登：本人在打卡頁送出申請，或由
人資直接 `correct_punch`。兩條路寫出來的都是 `source = manual`，畫面上標示
「補登」。整天由別人填的紀錄與本人按下的紀錄不是同一種證據，所以這一欄必須
留下來。

資料庫擋住兩件事：`UNIQUE (employee_id, punched_on, kind)` 讓一天只有一組
上下班；`CHECK (punched_on = (punched_at AT TIME ZONE INTERVAL '+08:00')::date)`
讓「哪一天」與「哪一刻」不會各說各話。用固定 `+08:00` 而不是
`AT TIME ZONE 'Asia/Taipei'`，是因為後者只有 STABLE、不能寫進 CHECK；台灣自
1979 年起沒有日光節約時間，兩者在這裡是同一件事。同一個常數是
`laysi_domain::BUSINESS_UTC_OFFSET_HOURS`。

**彈性工時與遲到寬限是兩件事。** 寬限（`late_grace_minutes`）原諒遲到，
但下班時間不動；彈性（`flex_window_minutes`）把整天往後移，長度不變。同一個
09:45 到班，在一小時寬限下不算遲到但仍要 18:00 走，在一小時彈性下不算遲到而且要
18:45 走。`WorkSchedule::expected_end()` 因此要吃「幾點到」。

早到不會把那一天縮短（`shift_minutes` 夾在 `0..=flex`）：沒有人被要求早到，讓它
生效等於把好意變成義務。留空表示不採彈性工時，也就是所有既有安裝現在的行為。

**上下班時間是公司政策，住在 `company_setting`（`work_start`／`work_end`／
`break_start`／`break_end`／`flex_window_minutes`／`late_grace_minutes`），不在 Rust
裡。** 全新安裝這幾格是空的，此時
「這個人遲到了嗎」不是回答「沒有」，而是**不問**：`WorkSchedule::late()` 回
`Option<bool>`，`None` 一路傳到畫面上顯示為「未設定上班時間」。預設 09:00 等於
本系統替別人的公司編了一條規定，再拿它去判定員工。

**打卡推導的加班只算時數，不算金額。** 打卡是「這個人幾點走的」，不是「公司同意他
加班」；金額要有人同意過的時數才算得出來，那是加班申請（見下一節）。

順帶修掉一個既有問題：`today()` 原本讀 UTC 日期，於是本地時間 00:00–08:00 之間
建立的任何單據都會記成前一天。現在走 `laysi_domain::business_today()`。

### 休息時間與加班時數

**加班時數＝出勤時數 − 休息時間 − 一天的正常工時**，下限為 0。原本的算法是
「比 `expected_end` 晚多久」，兩者只在有人早到時分岔，而分岔得很難看：上班時間
12:00 的人 04:00 打卡、19:31 打下班，畫面同時印出 15.5 小時工時與 1.5 小時加班，
中間那 8 小時哪一頁都不承認。出勤紀錄是工時唯一的來源，一個從它算不出來的加班數字
在每一張印出它的畫面上都是錯的。

代價是早到現在會累積加班時數。那不是漏洞而是分工：這個數字**沒有金額**，錢由
加班申請與核准決定，畫面上那句「加班費請送出加班申請」講的就是這件事。

**所以休息時間必須是一個設定。** 上班到下班之間扣掉 `break_start`–`break_end`
才是一天的正常工時；沒有它，09:00–18:00 含午休的公司每個人每天都會多出一小時加班，
而那一小時誰也沒工作。同一個區間同時從三個地方扣掉：`daily_seconds()`（正常工時）、
`worked_seconds()`（工作時數）與 `leave_seconds()`（請假時數），所以照上下班時間
出勤的一天恆為 0 加班，請整天假恆為一天。

兩個欄位分開存、一次存一個，所以**只填一端等於沒填**（`break_window()` 回 `None`）：
存檔的中間狀態是正常的，不該讓畫面在那一刻報錯。休息時間跨不到下班時間之外的部分
不扣（`daily_seconds()` 先把區間夾進上下班之間），加班時段落在休息時間裡則是要算的
工作——`submit_my_overtime_request` 的重疊檢查走的正是 `leave_seconds()`。

一天的正常工時取公司自己的上下班區間，不是法定的 8 小時。窗口開得比 8 小時長的公司
會少算加班，那個上限是有生效日的法規數值，要進 `overtime.toml` 才能用（`ponytail:`
註記在 `overtime_seconds()` 上）。

**算得出加班時數，就要說出它合不合法。** 打卡是勞檢讀的那份紀錄，而第 32 條的單日
上限（平日 4 小時、休息日 12 小時）與每月 46 小時都在 `overtime.toml` 裡，
`OvertimeLimits` 只是把它們取出來比對——`ops/attendance.rs` 沒有寫下任何一個數字，
換一份規則檔，同一天的判定就跟著換。日的種類與加班申請同一套判斷
（`WorkSchedule::overtime_day_type()`，由工作日曆決定）。

三處同時說：逐日明細標「加班超過 N 小時」、當月摘要接一句引一次第 32 條再列事實，
法遵檢核表多一項「打卡推算的延長工時」。**違法與「還沒對上」不是同一種輕重**，所以
摘要與標籤各有一級之差：`recon--alarm` 把左標尺換成 `--color-accent-deep`，`tag--alarm`
整塊填滿並用反白字——全站只有 toast 這樣做過，留給超過法定上限的那一種。**它與既有的
「每月延長工時上限」不是同一件事**：後者加總已核准的申請——公司同意過的時數，前者讀
打卡——實際出勤的時數，而違法的那一天正是沒有人申請的那一天。上下班時間沒填時這一項
`checked = false`，不是通過。

### 加班申請

加班原本散在三個互不相連的地方：打卡推導出時數，`attendance.overtime_hours` 與
`overtime_pay` 是外部差勤系統對一個月的主張（那張沒人寫得進去的表已經刪掉），
薪資單上的 `overtime` 直接抄後者。只用本系統的公司因此永遠發不出加班費，而且
**沒有任何人核准過任何一次加班**——勞基法第 32 條把雇主的同意當成加班合法的前提。

申請就是那份同意，形狀與休假單相同：牆上時鐘的起訖時刻，時數等於區間長度扣掉休息時間，
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

**月上限比對的是「仍佔住時數」的時數，不是「已核准」的時數**：等簽核的那幾小時已經
被佔住，核准與否不是申請人決定得了的；被駁回、撤回或撤銷的單把時數還回來。兩邊因此共用 `committed_overtime_hours()`，
而 `list_my_overtime_requests` 回 `month_hours_left`——畫面原本自己拿上限減去已核准
時數，於是有單在等簽核時它宣告的可申請時數比 `submit_my_overtime_request` 願意收的
多，按下送出才被擋。申請畫面在超過該數字時不畫送出鈕，改印還剩多少。

`run_payroll` 只認核准的申請；沒有核准紀錄的加班不計入（也沒有第二個
來源可以沿用）。**國定假日與例假不做**：第 39 條與第 40 條
按日加倍發給而不是按小時計費，而系統沒有國定假日日曆，判定不了某一天屬於哪一種。
週六與週日一律當休息日，與休假時數的假設同一套；法遵檢核把這一塊列為未檢查。

### 撤回與撤銷

休假單與加班申請原本只有一條路：待簽核到核准或駁回。改期的人只能請簽核者「駁回」當作
取消，本人的紀錄上留下一張已駁回；核准錯了（例如特休超過額度）則沒有任何辦法，那張單
照樣進薪資。現在的狀態轉移：

| 從 | 到 | 操作 | 誰 |
|---|---|---|---|
| `pending` | `approved`／`rejected` | `decide_*` | 持有 `manage_hr` 的人 |
| `pending` | `withdrawn` | `withdraw_my_*` | 申請人本人 |
| `approved` | `revoked` | `revoke_*` | 持有 `manage_hr` 的人，理由必填 |

**兩個狀態，不是一個 `cancelled`**：撤回的是還沒人決定過的單，`decided_at` 維持
NULL；撤銷的是一個核准，原本的核准留在 `decided_*`，誰、何時、為什麼撤銷寫在
`cancelled_by`／`cancelled_role`／`cancelled_at`／`cancel_reason`。兩條 CHECK 把欄位
與狀態綁在一起：`decided_at` 為 NULL 恰好是待簽核或已撤回，`cancelled_at` 有值恰好是
已撤回或已撤銷。回應裡四欄合成一個 `cancelled` 物件，沒撤的單是 `null`。**撤回與撤銷
都不能復原**，同一段時間要重新申請，拿到新的單號；排除約束只看待簽核與已核准，所以
重送不會被原本那張擋住。

**撤回是自助操作**，輸入只有單號與選填的理由，所屬員工由 `ctx.employee_id` 決定並寫進
`UPDATE` 的 `WHERE`，別人的單號回「查無休假單」——拒絕會確認那個編號存在。
**已核准的單本人撤不回**，訊息指向人資：核准之後那張單已經是薪資的輸入。

**撤銷沿用核准的職責分離**：申請人不得撤銷本人的單（撤銷一張事假等於不扣那天的工資），
`deciding_as(ctx)` 交給 store，公司設定 `owner_self_decide` 開著時負責人例外。拒絕訊息
由 `own_revoke_refusal` 依角色組，負責人指向「基礎設定」；其他角色只說要由另一個帳號
撤銷——那張單是別人核准的，第二個帳號一定存在。

**計薪之後不改**：核准與撤銷都會改變薪資讀到的東西，所以讀過這張單的薪資作業只要有
一期還在，兩者都拒絕。駁回不改變任何薪資讀到的東西，照常可以做——列表因此另回
`can_approve`／`approve_blocked_reason`，畫面在已計算的期別只畫駁回鈕，核准的位置印原因。

- 加班：加班日所在的那一期（`overtime_payroll_span`）。
- 休假：單據碰到的每一個月，**一直到最後一天所在年度的 12 月**（`leave_payroll_span`）。
  病假的 30 日與生理假的 3 日按曆年從 1/1 起算，`run_payroll` 讀當年 1/1 起的休假單，
  所以 11 月的薪資也讀過 3 月的病假；只看 3 月會讓已經算好的 11 月與它讀過的資料不一致。
  期別通常依序計算，前一期沒算、後一期已算的情形少見，這條範圍在那時才多擋一些。

**「讀過」看的是申請人本人在那一期有沒有薪資單**（`PayrollPeriodState.paid`，該期每一次
計算的薪資單合起來），不是那一期有沒有算過。計薪之後才建檔的人、第一次計算被跳過的人（例如
時薪制當時還沒有完整打卡），那一期沒有任何一次計算讀過這些人的單；同一期的下一次計算補發時
才讀，所以這些人的單照常核准與撤銷，已過帳的期別也一樣——補算是那裡唯一的路。補發之後，
同一條規則對這些人生效。只看期別有沒有算過的話，補發出來的薪資單會少掉加班費、不扣請假的
工資，而唯一的修法是把整期連同其他人一起作廢，已過帳的期別連作廢都不行。

擋下的訊息分兩種（`payroll_blocker`，動作是「核准」或「撤銷」）：該期傳票都還是待過帳
時，說「{期別} 已計算薪資，{動作}前請先到「薪資作業」作廢」——作廢、核准或撤銷、重新計算；
有任何一張已過帳時，說「{期別} 的薪資已過帳，無法{動作}；差額請在下一期補發或補扣」，與
`void_payroll_run` 的拒絕同一句出路，不把人送去另一個必定拒絕的畫面。過帳看的是整期而不是
申請人那一次計算：作廢一次拿掉該期每一次計算，補發那一次的傳票還是待過帳，前一次已過帳就作廢
不了。列表的
`approve_blocked_reason`／`revoke_blocked_reason` 由同一個函式產生，畫面印的就是按下去
會收到的那一句。

**決定、撤銷與計薪互斥**：`run_payroll` 在寫入的交易之外讀休假與加班，核准或撤銷可能剛好
落在讀與寫之間。三方因此共用一把交易層級的 advisory lock（`PAYROLL_INPUT_LOCK_KEY`）：
`decide_*`（核准時）與 `revoke_*` 先以 `FOR UPDATE` 鎖住單據、再拿這把鎖才去找已計算的
期別；`record_payroll_run` 在寫入的交易開頭拿這把鎖，比對當下已核准的單號與這次算進去的
（`leave_span`／`leave_nos`、`overtime_span`／`overtime_nos`，`confirm_priced_requests`）。
先提交的那一方，後者一定看得到：決定或撤銷先提交，計薪回「{單號} 在計算期間被核准或撤銷，
請重新計算本期薪資」且什麼都不寫；計薪先提交，核准與撤銷照常被擋下。比的是整個集合而不是
鎖住讀到的那幾列：計薪讀完之後才送出又被核准的單，它根本沒讀到，鎖不住。測試是
`requests_are_taken_back_until_payroll_reads_them` 的最後兩段，用另一條連線拿著同一把鎖把
撤銷與核准停在未提交，等計薪卡在鎖上再放行。

兩位主管同時決定同一張單，第二位等第一位提交，再讀到已決定的狀態被拒；拒絕訊息說出
目前的狀態（「已為「已駁回」，只有等簽核的單可以簽核」），不印內部代碼。

**「還佔著時數」只有一個判斷**：`holds_hours(status)`（待簽核或已核准）。逐日明細掛假
（`mark_days`）、每月加班上限的累計（`committed_overtime_hours`）與送出前的重疊預查
都用這一條，與排除約束的 `WHERE` 是同一組狀態。計薪與特休已休天數本來就只認已核准。

撤回與撤銷四個操作對 AI 預設關閉。

### 補登申請

`correct_punch` 原本只給了人資，沒有給員工任何東西。知道自己漏打的人正是唯一
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

**申請人不得決定本人送出的申請。** 與休假、報支同一條規則（含負責人自行核決的
例外），鍵是 `employee_id` 不是姓名。`list_punch_corrections` 逐列回 `can_decide` 與
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

**每一天都說得出它是什麼：出勤、請假、非工作日或曠職**（`mark_days`）。原本員工端的
曠職日、端午節與已准事假是三列一模一樣的空白，人資端的明細則直接沒有那一天。
`mark_days` 在 `month_days` 之上掛上碰到該日的休假單（待簽核與已核准，駁回、撤回與撤銷的不算）與
該日的請假時數，並標出曠職：工作日曆說是工作日、已經過完、沒有任何打卡、也沒有休假單。
**哪一天掛哪張單、掛幾小時讀 `leave_by_date()`**，與計薪攤日、特休已休同一個函式，不依
現行的上下班時間與工作日曆重算：事後登記的停班日照樣掛著那天的假，三處講同一個天數；
事後改上下班時間也不會讓上午的假從那天消失、把那天判成曠職。唯一的例外是沒有拆法記錄的單
（只會來自補寫 `leave_request_day` 那一刻上下班時間是空的）：照計薪的讀法整張落在第一天，
上下班時間設回來之後其餘幾天會變成曠職，所以逐日明細改依現行上下班時間與工作日曆拆
（`shown_by_date`），與這張表建立之前的顯示相同；計薪與特休仍放在第一天，總天數不變。
**待簽核的單與審核中的補登也擋掉曠職**——准不准是簽核的人要回答的問題，先標曠職等於
替他回答；送了補登就是本人說那天有到。
**「今天」是真的時鐘**，與打卡同一條理由（`LAYSI_TODAY` 不移動時鐘）。三個情況不判定：
沒有上班時間（與遲到同一條前提）；兼職——系統沒有約定排班（#3），非排班日會全部
被算成曠職；以負責人身分投保的人（`insured_as = owner`）——那是雇主，不是出勤管理的對象。
**非工作日的空白列仍然按得動補登**：週六加班卻上下班都忘了打卡，本人唯一的路就是那一格。`list_attendance_punches` 因此改成列出該月每一位在職員工，不只是有打卡的
人：整個月都沒打卡的人正是這一頁要看到的那一列。
`day_type_label` 是日別（例假、國定假日、天災停班），不是加班計價的分類——後者把
所有非工作日都算休息日，印在明細上就是把端午節寫成休息日。
人資頁的休假單清單跟著上方的月份與員工走，縮小範圍時用 `other_pending` 說出沒列出的
待簽核張數：那張清單是簽核的地方，篩掉一張等簽的單等於讓它沒有人簽。

**今天那一列不從查詢的月份推導。** `next_kind` 是打卡鈕要顯示上班還是下班，
翻到過去的月份時那個月裡沒有今天，只看查詢結果會回頭要人再打一次上班卡。
月份不是本月時另查今天所屬月份的打卡列。

**`correct_punch` 留著，兩條流程並存。** 本人補登由本人送出，對象由登入身分決定，
依 `punch_correction_policy` 審核或自動通過；人資補登由人資填員工編號，寫入當下即為
已核准。兩條路都經 `punch_in_tx` 寫成 `source = 'manual'` 的打卡，也都在
`punch_correction` 留一列，哪一條路寫的記在 `hr_entered`（見下一節）。持有補登權限的人
以 `correct_punch` 改自己的打卡是產品接受的行為，不另加本人檢查。

### 補登留下歷史紀錄

`punch_in_tx` 的寫入是 `ON CONFLICT DO UPDATE`，把原本那一列的 `punched_at`、
`source`、`note` 與 `created_at` 一起覆蓋掉。補登申請之前這條路只有人資走，而
`correct_punch` 不寫任何申請列，於是一次更正之後，被改掉的那個時刻在系統裡不存在
於任何一個地方。勞基法第 30 條第 6 項要求出勤紀錄保存五年；一份改得動又看不出改過
的紀錄，正是勞檢會問的那一份。這與公告閱讀確認的判斷是同一條：把「誰在
哪一天做了什麼」銷毀的路要先堵起來。

**每一次更正都在 `punch_correction` 留一列，兩個方向都是。** 員工送出的申請本來
就有列；人資的 `correct_punch` 現在也寫一列，寫入當下即為 `approved`——沒有人會回頭
審核人資剛剛打的字，但那次更動事後要讀得出來。`Store::record_punch` 的第二個參數
因此由 `replace: bool` 改成 `correcting: Option<&str>`，帶的是更正者的角色代碼：
會覆蓋的那一次，就是要記錄的那一次，兩件事由同一個值決定，不會各記各的。

**`replaced_at` 是被覆蓋的原時刻，`NULL` 表示該時段原本是空的。** 空格補上一個時間
與把一個時間改成另一個時間是兩件事，畫面上也只有後者要印出「原 08:05」。
`decide_punch_correction` 與自動通過的 `submit_punch_correction` 同樣由
`punch_in_tx` 的回傳值填它。

**人資直接補登與員工申請經核准，在畫面上是兩個標籤。** 判準是
`punch_correction.hr_entered`，只有 `correct_punch` 的寫入路徑寫 `true`。原本由
`decided_by = created_by` 推導，而兩欄存的都是顯示名稱：`owner_self_decide` 開著時
負責人核准本人的申請，或核准的人與申請人同名，兩欄都相等，那一列就被標成人資補登。
哪一條路寫的是要保存五年的紀錄本身，所以寫入時記下，不事後推導；既有列由 migration
依「兩欄相等且 `decided_at = created_at`」回填——人資補登在同一道 INSERT 寫兩個時刻，
核准一定在另一個交易。ops 以 `hr_entered` 回給畫面，「出勤與休假」的
那一段因此叫**補登紀錄**而不是補登申請。

**整條鏈掛在它改的那一天上，不只掛在一張全公司的清單上。** 一格可以更正很多次，
而看某個人某一天出勤的人不會先去翻另一張表。逐日明細的狀態欄因此把「補登」那個
標籤換成可按的「補登紀錄 N 筆」，按下去在同一列底下展開 `components/PunchTrail`：
由舊到新，每一筆印「原時刻 → 新時刻」、是誰以哪一種身分寫的、哪一天、事由。
駁回與待審核的那幾筆沒有箭頭——它們沒有改動任何東西，但它們是紀錄的一部分。
人資與員工兩邊用同一個元件，因為那是同一份紀錄。

**畫面一律以 `include_done: true` 取全部，「含已決定」的 chip 在前端過濾。**
逐日的鏈與待審核的清單讀的是同一份資料，分兩次取會讓其中一邊看到半條鏈；
`waiting_count` 由後端數 `submitted`，與傳什麼過濾條件無關。

### 員工自助模組

| name | risk | input | output 重點 |
|---|---|---|---|
| `get_my_home` | ReadOnly | `{}` | 本人基本資料（含眷屬的姓名、關係與投保期間，不含字號與生日）、年資、本月出勤、特休餘額與特休年度起訖、送出的單走到哪 |
| `list_my_payslips` | ReadOnly | `{}` | **只有本人的**薪資單 |
| `list_my_leave_requests` | ReadOnly | `{}` | 本人請假單、特休額度／已休／剩餘與特休年度起訖、`leave_kinds`、`schedule`，以及表單預設日期 `next_workday`（今天起第一個工作日，依工作日曆） |
| `preview_leave` | ReadOnly | `{ from_at, to_at }` | `{ hours, days, off_days }`；與 `request_leave` 共用 `leave_worth`，送出前顯示的就是會存下的數字 |
| `request_leave` | NeedsConfirm | `{ kind, from_at, to_at, reason? }` | `{ request, schedule }`，編號 `LV{西元年月}-{三碼}`；時數與天數由後端算 |
| `withdraw_my_leave_request` | NeedsConfirm | `{ leave_no, reason? }` | `{ request }`；只收本人仍在等簽核的單，別人的單號回查無此單 |
| `list_my_overtime_requests` | ReadOnly | `{}` | 本人加班單、本月已核准時數、上限與尚可申請時數、`day_types`、`calendar`（日別與星期推得的不同的日期，畫面據以顯示加班日） |
| `submit_my_overtime_request` | NeedsConfirm | `{ from_at, to_at, reason? }` | `{ request }`，編號 `OT{西元年月}-{三碼}`；時數為區間長度扣掉休息時間，日別由工作日曆決定 |
| `withdraw_my_overtime_request` | NeedsConfirm | `{ request_no, reason? }` | `{ request }`；只收本人仍在等簽核的單，別人的單號回查無此單 |
| `list_my_expense_claims` | ReadOnly | `{}` | 本人送出的報支與逐關進度 |
| `submit_my_expense_claim` | NeedsConfirm | `{ title, amount, spent_on?, pay_method?, memo? }` | `{ claim }`，編號 `EX{西元年月}-{三碼}` |
| `attach_my_expense_receipt` | NeedsConfirm | `{ claim_no, file_name, content_base64 }` | `{ receipt, receipts }`，收據進 RustFS |

**休假時數畫面不自己算。** 休假申請原本在 TS 裡重寫一份 `leave_seconds()`：沒扣
休息時間、週六週日寫死，於是有午休的公司每一張整天假都是畫面 9 小時、存下 8 小時，
跨到國定假日的假畫面多算一整天。補齊那兩項等於在瀏覽器裡維護第二份工作日曆的解讀，
所以改成欄位一變就叫 `preview_leave`。代價是每次試算都留一筆稽核；它只在起訖都填好
且結束晚於開始時才叫。

**這個模組的授權不靠 `Risk`，靠輸入形狀。** `list_payslips` 是 `BossOnly`，
但員工本人必須看得到自己的薪資單，而他不是老闆。作法是：**自助操作一律不接受
任何指定對象的參數**，對象由已驗證 session 的 `ctx.employee_id` 決定。沒有欄位可
以填，就沒有辦法填別人的名字；這比在查詢裡加過濾條件更難寫錯。

`ops/selfservice.rs` 的 `no_self_service_input_can_select_another_employee`
測試把每一個自助操作的 `Input` 屬性集合**逐字釘死**（`request_leave` 只有
`kind`／`from_at`／`to_at`／`reason`，`submit_my_overtime_request` 只有
`from_at`／`to_at`／`reason`——日別由工作日曆決定，申請人填不進來，
`withdraw_my_leave_request`／`withdraw_my_overtime_request` 只有單號與 `reason`——
單號指向的單屬於某個人，所以員工寫進 `WHERE`，`submit_my_expense_claim` 只有
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
`approval_rule` 關卡，自助報支不動任何資料表。

**收據是送位元組，不是指一份既有文件。** 上一段擋掉的是「指向已經存在
的文件」，不是「附一個檔案」——收據本來就是後者。`attach_my_expense_receipt`
因此收 `file_name` 與 `content_base64`，輸入裡沒有任何東西能指到既有文件。

擋不住的是 `claim_no`，它指的是報支單，而報支單屬於人。所以
`expense_claim` 帶 `employee_id`：`claimant` 是顯示名稱、會同名，讀那一側沿用
它還可以接受，寫這一側不行。查詢是
`WHERE claim_no = $1 AND employee_id = $2`，**別人的單號是查無此列**，不是查到
之後才拒絕；會計代建的單 `employee_id` 為 NULL，因此在自助這一側完全看不到。
狀態限 `submitted` 與 `approving`：收據是還沒作成的決定的憑據，決定作完之後
背後的憑證不該再變。

流程是先建單、再附件——反過來的話 `link_ref` 沒有單號可寫。因此上傳失敗時單
已經存在，畫面照實說明哪幾個檔案沒附上，並要求到「我的請款」重附，**不要重送
整張單**；`accepts_receipt` 就是那顆按鈕出不出現的依據。

**簽核的人要看得到那份收據。** `list_expense_claims` 因此逐單以
`documents_linked_to('expense_claim', claim_no)` 回 `receipts`，形狀與
`list_my_expense_claims` 的同一個 `receipt_json`。少了它，收據只有申請人自己
看得到，而要用到它的是下一關——費用報支頁展開一列時列出收據，沒有收據就直接
說沒有。取回檔案要 `get_document`，那屬於 `manage_documents`，而簽核屬於
`approve_expense`：主管只有後者，所以檔名在有 `get_document` 的人手上是按鈕、
在沒有的人手上是純文字。

**核准產生的傳票不另存一份收據。** 最後一關開出傳票時，收據仍然掛在
`expense_claim` 上——搬過去申請人就看不到自己的單據，複製一份則同一個檔案有兩個
編號。傳票那一側改成問得到：`search_vouchers` 與 `voucher_by_no` 的 `docs` 同時
數傳票自己的附件與該傳票所屬報支單的收據（共用 `voucher_docs!`，`document_stats`
的「沒有附件的傳票」用同一個判準），並回 `claim_no`，憑證抽屜據以一併列出，
每一列標明來自哪一張報支單。

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
自助操作與其他操作一樣受上節那個 migration parity 測試涵蓋。

`Voucher` 的 JSON 形狀：

```json
{ "no": "V2607-018", "date": "2026-07-24", "memo": "支付本月電費",
  "status": "pending", "reverses": null, "reversed_by": null, "amount": "8420.00",
  "accounts_label": "6118 水電費 / 1102 銀行存款",
  "lines": [ { "account_code": "6118", "account_name": "水電費",
               "debit": "8420.00", "credit": "0.00", "memo": null } ] }
```

`docs` 是這張傳票的憑證影本份數，`claim_no` 是產生它的費用報支單號（沒有就是
`null`）。份數同時數兩種：掛在傳票上的（`document.link_type = 'voucher'`、
`link_ref` 為傳票號、未封存），以及掛在那張報支單上的收據——`search_vouchers`
與 `voucher_by_no` 共用 `voucher_docs!` 這段子查詢。**紙本傳票夾發票影本，系統這一側要看得到有沒有夾**：傳票總帳的「憑證」
欄印這個數字，按下去是 `components/VoucherDocs` 抽屜（`list_documents` 以傳票號
查，有 `claim_no` 時另以報支單號查一次併入、每一列標明來源、`upload_document`
帶 `link_type: 'voucher'` 附上、`get_document` 取回）。三個
操作同屬 `manage_documents`，畫不畫按鈕看 `list_documents` 在不在 `permissions` 裡。
附件一律先建傳票再附——`upload_document` 會確認那張傳票存在，所以記帳抽屜當下還沒有
傳票號，附不了。文件管理頁那張「沒有附件的傳票」統計數的就是這一件事。

`get_dashboard` 的欄位對應原型「總覽」畫面，全部由總帳推導：

- `stats`：銀行＋現金（1101+1102 餘額）、應收帳款（1131 餘額）、
  應付帳款（2101 餘額）、本月損益（本月收益−費用）。
  形狀 `{ kicker, value, note }`，`value` 為金額字串。
- `todos`：`{ count, label, due, screen }`，`screen` 是前端路由 key。
  **數量為零的項目不出現**：待辦清單上的 0 沒有可做的事。
  待簽核的費用報支也算一項——它是財務這一側唯一等人動作的單據，漏掉它，
  總覽會在還有人等著簽的時候宣告沒有待處理事項。畫面**逐項比對
  `/v1/auth/me` 的 `permissions`**（`screenOp(screen)`）才畫得動，
  開不了那個畫面的角色看得到數字、按不下去，與人資總覽同一套做法。
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
| GET | `/healthz` | 未登入：`{ status, db: {ok}, rustfs: {ok} }` |
| — | — | 帶系統管理員 session cookie 時另加 `db.migration`、完整的 `rustfs`（含 endpoint、bucket，以及即時讀取的存檔模式 `durability: {mode, pinned, error?}`）與 `operations` |
| — | — | 前端也讀這一條（Caddy 與 vite 都要 proxy）：RustFS 狀態只有伺服器行程知道，`get_system_health` 從資料庫查不到 |
| POST | `/v1/auth/login` | 帳密登入並設定 session cookie；失敗訊息不區分帳號或密碼 |
| POST | `/v1/auth/logout` | 撤銷目前 session 並清除 cookie |
| GET | `/v1/auth/me` | 目前登入帳號；未登入回 401 |
| POST | `/v1/auth/change-password` | 更新 Argon2id 雜湊並清除 `must_change_password`，同一交易撤銷該帳號其他登入工作階段（呼叫者自己的保留）；`current_password` 只在強制變更以外的呼叫才驗 |
| POST | `/v1/auth/bootstrap` | 安裝完全沒有帳號時建立第一個帳號（角色固定 `admin`）並直接簽發 session |
| GET | `/v1/auth/oauth/start` | 啟用時建立 PKCE 流程並以 302 前往 discovery 的 authorization endpoint；`return_to` 只收同意畫面的網址 |
| GET | `/v1/auth/oauth/callback` | 後端交換授權碼、驗證 id_token、建立 session，再以 302 回 `return_to` 或 `/` |
| GET | `/v1/meta` | 公司、會計期間、身分、版本、授權 |
| GET | `/v1/ops` | registry 清單：`[{name, risk, summary, enabled, input_schema}]` |
| POST | `/v1/ops/{name}` | body 是 `{ input, prompt }`（兩者皆可省略）；回 `{ ok: true, data }` 或 `{ ok: false, error: {code, message} }` |
| POST | `/mcp` | MCP streamable HTTP，Bearer token |
| GET | `/.well-known/oauth-protected-resource` | 指出 `/mcp` 由哪個授權伺服器守著（RFC 9728）；`/mcp` 後綴的寫法回同一份 |
| GET | `/.well-known/oauth-authorization-server` | 授權伺服器 metadata（RFC 8414） |
| POST | `/v1/oauth/register` | 連接器自行註冊（RFC 7591），回 `client_id`，無密鑰 |
| GET | `/v1/oauth/authorize` | 同意畫面要顯示的連接器名稱、回傳位址與代表身分；需要 session |
| POST | `/v1/oauth/authorize` | 同意或拒絕，回一個瀏覽器要前往的網址；需要 session |
| POST | `/v1/oauth/token` | form-encoded，授權碼換 MCP Token |

**`/healthz` 分兩層回答**：這條路徑未登入就打得到（compose 的自檢在有帳號之前
就要跑，Caddy 也直接放行），所以匿名的回答只有各部分起來了沒有。物件儲存的
endpoint 與 bucket、migration 版本描述的是這套安裝的內部，只在請求帶著
系統管理員的 session cookie 時附上——那正是「安全與營運」畫面的讀者。

**request body 的上限依路由分開設**（`AUTH_BODY_LIMIT`／`OPS_BODY_LIMIT`）。
axum 對每個 `Json`／`Bytes` 擷取器有自己的 2 MB 預設，對登入太寬鬆，對
`upload_document` 又太小：25 MB 的檔案（`storage::MAX_OBJECT_BYTES`）以 base64
放在操作輸入裡，body 是它的 4/3。原本那個檔案在擷取器就被擋掉，操作自己的容量
檢查與訊息永遠輪不到。所以 `/v1/auth/*` 收到 64 KB，`/v1/ops/{name}` 與 `/mcp`
收到 40 MB，兩者的關係由 `const _: () = assert!(...)` 在編譯期釘住。
`/v1/uploads/{secret}` 收 `MAX_UPLOAD_BYTES` 加 1 MB 的 multipart 框架
（`uploads::UPLOAD_BODY_LIMIT`），而且只有這一條：宣告的長度超過就在讀 body 之前回
413，串流中超過 90 MB 由每檔的檢查回一句說出上限的 413。

`/healthz` 是公開端點。`/v1/meta`、`/v1/ops*` 必須有
有效 session；`/mcp` 必須有自己的 Bearer 憑證。認證路由是操作註冊表第一個例外：
登入若經過 dispatch，就會先被角色權限步驟要求一個尚未取得的身分，形成循環。
第二個是 `/v1/uploads/{secret}`，授權是上傳連結本身；第三個是
`/v1/downloads/{secret}`，授權是下載連結本身。兩者見〈文件〉一節。
`/v1/auth/bootstrap` 同樣未登入即可呼叫，但守門的是「安裝目前沒有任何帳號」，
由資料庫的單列表保證，見上文〈網頁初始化〉。

`/v1/meta` 回傳：

```json
{ "company": "", "tax_id": "", "setup_required": true,
  "period": "2026 / 07", "period_state": "未結帳",
  "actor": "目前使用者", "actor_role": "acct", "employee_id": 1,
  "oauth": { "enabled": true, "provider": "https://accounts.example.com" },
  "brand": { "name": "", "display": "名稱", "logo": "" },
  "version": "v0.1.0", "licence": "商業授權" }
```

`version` 是 `LAYSI_BUILD`，由 `release.yml` 把正在建的 tag 當 build arg 傳進
`backend/Dockerfile`，沒有它就退回 crate 版本（工作樹裡建出來的就是這一種）。
`crates/laysi-server/build.rs` 只有一行 `rerun-if-env-changed`：`option_env!` 自己
不登記相依，而發行映像把 cargo 的 target 放在 cache mount 上，少了它下一個 tag 會沿用
上一個 tag 編進去的那份目的檔。畫面在「安全與營運」的「系統健康」第一列。

`brand` 也在 `/v1/auth/me` 上，未登入同樣讀得到：登入頁是畫它的兩個地方之一。
`logo` 是 `brand_logo` 的 data URI，顯示方式不用標誌時回空字串；沒有標誌時
`display` 降回「名稱」。

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
  **那張表有上限**：最多 10,000 個桶，鍵超過 256 位元組就在字元邊界截斷（開頭相同的
  兩個長名稱併成一桶，結果比較嚴）。滿了先清掉前幾分鐘的桶；同一分鐘就灌滿時，
  **換掉次數最少的那一桶**。拒絕新的鍵會讓這一分鐘還沒登入過的人全部被擋在門外；
  灌進來的名稱每桶只有一次，被猜的帳號已經累積到上限，所以先被換掉的是灌進來的那些。

### 遠端連接器的 OAuth（`crates/laysi-server/src/oauth.rs`）

Claude 這類雲端連接器只收得下一個網址，沒有貼 Token 的欄位。加入連接器的人在
別人的網頁上，所以取得憑證的路只有一條：連接器自己找到這裡、把人送過來登入、
再拿授權碼換一組 Token。少了這一段，`one.laysi.io/mcp` 對它就是「連不上」——
不是「要登入」，因為 401 沒有附上任何指路的東西。

**access token 就是 `api_token`**，`kind = 'mcp'`。`mcp.rs` 的認證一行不用改，
「安全與營運」照樣列得出、撤銷得掉；第二套 Token 儲存等於第二個要撤銷的地方，
而其中一個一定會被漏掉。因此沒有 refresh token 也不回 `expires_in`：這組 Token
的生死由那一頁回答，在這裡再宣告一次就是同一個問題有兩個答案。

發現這條路的順序：

1. `/mcp` 對沒帶憑證的呼叫回 401，並附
   `WWW-Authenticate: Bearer resource_metadata="…"`。
2. 連接器讀那份 metadata，得到授權伺服器的位置。
3. 讀 `/.well-known/oauth-authorization-server`，得到三個端點。
4. `POST /v1/oauth/register` 自行註冊，拿到 `client_id`。
5. 瀏覽器前往 `/oauth/authorize`（同意畫面），登入後按下同意。
6. `POST /v1/oauth/token` 用授權碼換 Token。

**絕對網址由請求本身推導**（`public_base`），與 session cookie 的 `Secure` 同一條
理由：要人記得在搬站時更新的設定值，就是會指向舊網址的設定值。`X-Forwarded-Host`
與 `Host` 各自先過一次字元檢查，壞掉的那個往下落而不是兩個一起丟掉。主機名是呼叫端
說了算，但偽造的值只改變它自己收到的回答，換不到任何東西。

**用戶端是 public client，沒有密鑰**：跑在別人雲端的連接器保管不了密鑰，改以
PKCE S256 證明持有請求，`plain` 一律拒絕。四件事守住這條流程：授權碼換發時同一
道敘述把該列刪掉，所以只用得掉一次；`redirect_uri` 在核發與換發兩端都要與註冊清單
逐字相符（前綴比對會讓授權碼被帶到同一個網域下的任何路徑）；身分只來自瀏覽器
session，請求裡沒有任何欄位說得出代表誰；角色在鑄造 Token 的當下重讀，不由授權碼
帶著走。

**`code_verifier` 的格式先驗再用**：`from_code_verifier_sha256` 對 43–128 字元的
長度是 `assert!` 而不是回傳錯誤，交給它一個沒檢查過的值會讓工作執行緒 panic，
呼叫端收到的是空回應。這道檢查排在贖回授權碼之前，格式錯的請求因此不會燒掉一張
還能用的授權碼。

**註冊是開放的**，那正是連接器不必先請管理員準備任何東西的原因，所以它的界線是
速率（每個來源位址每分鐘 5 次）而不是憑證。`redirect_uri` 只收 HTTPS，或桌面
用戶端會監聽的 loopback；帶片段的一律拒絕，那會把授權碼移出用戶端讀的那一段。

**同意畫面是 SPA 的 `/oauth/authorize`**，這個模組只回它要顯示的內容。在 Rust
裡另外描出一頁 HTML，等於把整套視覺語彙再抄一份，而那一份不會跟著 CSS 變數走。
`App.tsx` 在 `Workspace` 之前就攔下這個路徑——`Workspace` 掛載時會把網址
`replaceState` 成當前畫面的路徑，查詢字串會在那一步整段消失。還沒登入時照常畫
登入頁，網址不動，登入成功後這一頁自己就出現了。

**還沒登入就按下連接器時，登入完要回到同意畫面**：整份授權請求只存在於同意畫面的
查詢字串裡，登入之後落在首頁就是把連接器等著的那份請求丟掉了，而畫面上沒有任何跡象。
兩條登入路各自會丟掉它：帳號密碼登入的 `landOnStart()` 把網址 `replaceState` 成 `/`，
外部登入則是 `oidc::callback` 直接 302 回 `/`。前者加一個條件，後者由 `start` 收
`return_to`、與 PKCE 一起存在**伺服器端**的待決流程裡，回來時才讀出來——放在 callback
的查詢參數上，離開與回來之間就換得掉。`return_target()` 只認 `/oauth/authorize?` 開頭
的字串，所以這不是開放轉址：目標路徑是固定的，開頭必定是單一個斜線（`//evil.example`
是另一個站）。**沒有查詢字串的 `/oauth/authorize` 也不收**，那裡沒有可以續下去的授權
請求，只是有人借用了前綴。

**`/.well-known/oauth-*` 在 Caddy 要自己一條 route**：SPA 的 fallback 會用 200 與
首頁 HTML 回答它，而連接器解不出 JSON 之後報的是「這個網址不是 MCP 伺服器」，
不是「discovery 失敗」。授權端點與 `/oauth/authorize` 相反，它就是要落到 SPA。
**直接對外發布 `mcp` 服務的埠會繞過這條 route**，那條路上沒有 discovery，連接器
一樣連不上；要讓外部 AI 連進來就走 `web` 這個容器。

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

產品字標「Laysi One」是 `components/BrandMark` 排出來的文字，字體走
`--font-sans`（Inter）；`mark.svg` 與 `monogram.svg` 已刪除，`favicon.svg` 留著。
公司自己的標誌由 `components/CompanyBrand` 畫在登入面板與側邊欄左上角，來源是
`company_setting` 的 `brand_logo`（data URI）與 `brand_display`（名稱／標誌／
標誌與名稱），經 `/v1/auth/me` 與 `/v1/meta` 的 `brand` 送到畫面。

**前端同樣不得自帶任何法規數字。** 稅率、級距、耐用年數一律由後端從 ruleset
帶下來。畫面上要即時試算稅額時，用後端
回傳的費率，不要在 TS 裡寫 `0.05`。金額運算用 `client.ts` 的
`multiplyToDollars` / `addDecimals`（BigInt），不經過 `number`。

**回應的形狀由 zod 在邊界檢查**。後端的輸出沒有型別可以生成——操作回的是
`serde_json::Value`——所以 `client.ts` 的形狀宣告本來就是手寫的，改成 zod schema
並不會讓它自己跟上後端。它換掉的是失敗的方式：欄位被改名、被拿掉或型別變了
（金額從字串變成數字是最貴的一種），原本要等到某張表格印出 `undefined` 才看得出來，
現在停在 `callOp` 並回一則錯誤。宣告的寫法是
`export const employeeRow = z.object({...})` 配一行
`export type EmployeeRow = z.infer<typeof employeeRow>`，型別名不變，畫面的
`import type` 不用動。`z.object` 預設會**丟掉**不認得的欄位而不是報錯，所以後端
新增欄位不會弄壞還沒跟上的畫面。

**schema 是 `callOp` 的第四個參數**：`callOp(name, input, prompt, schema)` 傳了就驗，
沒傳就照舊。放在呼叫端而不是一張「操作對 schema」的表，是因為表容許型別參數與驗證
對象指到兩個不同的東西；第四個參數同時決定回傳型別，兩者對不上編譯就過不了。

**三個檢查**。`scripts/function-test.ts` 的 `must()` 在每一次成功呼叫之後照 schema 解析
一次——**寫入型操作只有這條路驗得到**，因為它們要真的參數，而那正是那個檔案花掉的長度。`scripts/schema-test.ts` 是純資料的，不必起伺服器，守住三件事：多出來的
欄位要放行、少一個欄位要擋下、金額變成數字要擋下。`scripts/schema-live-test.ts` 是對著
真的伺服器問——手寫的 schema 只有這條路證得出它與後端相符——它從呼叫端本身讀出
「哪個操作配哪個 schema」（不另存一份清單），以負責人與系統管理員兩種身分逐一呼叫再解析。
接不上參數或該角色叫不動的操作算略過，不算通過。要先跑 `scripts/function-test.ts`，
它留下的資料庫就是這條檢查用的那一個。兩者共用 `scripts/response-schemas.ts`：操作與 schema
的對應從呼叫端讀出來，不另存一份清單。一個操作可以有多個宣告形狀（`list_notices` 帶 `id`
回的是版本歷程），對上任何一個就算通過——呼叫端本身沒有這個模糊，那裡 schema 與回傳型別
是同一個參數。

**一個操作的回應只有一個名字**。`list_accounts` 與 `list_chart_of_accounts` 是兩個
操作、兩種形狀：前者是科目本身，後者另帶顯示名稱、餘額與使用筆數。「費用報支」原本
把 `list_accounts` 的回應宣告成 `ChartOfAccounts`，讀得到的 `kind_label` 等四個欄位
其實從來沒有送來過；畫面只用到 `code` 與 `name`，所以那個謊安靜地成立了很久，是上面
那條 live 檢查把它抓出來的。另外五個畫面各自宣告了 `Account`／`GlAccount`／
`PostableAccount`／`FullAccount` 四個名字給同一份回應。現在統一成 `client.ts` 的
`account`／`accountList`。同一條 live 檢查另外抓到 `company_setting` 的 `source`：後端送
`setting`／`derived`／`regulation` 三種，前端只宣告了兩種。

**註冊表以外的三條路由也走同一套**：`/v1/auth/me`、`/v1/meta` 與 `/v1/ops` 不是操作，
沒有呼叫端可以讀出配對，schema 一樣住在 `client.ts`（`authEnvelope`／`meta`／`opList`），
live 檢查以 `mismatchAgainst` 逐一核對。`/v1/auth/me` 是其中最要緊的一條：`permissions`
決定每一個入口畫不畫，而它原本是一次 `body as Envelope<SessionUser>` 的轉型，前端對它
一無所知。形狀不符時 `readEnvelope` 回一則 `SHAPE_MISMATCH`（與 `callOp` 同一則訊息，
`client.ts` 匯出），走既有的 `failure()` 出去，未登入那條路不受影響——401 的 body 仍然
解析得出 `oauth` 與 `setup`。`SessionUser`／`SetupState`／`OauthState` 因此從 `auth.ts`
搬進 `client.ts`：`auth.ts` 留 React hooks，形狀集中在同一個檔案，測試腳本也才 import
得到（`auth.ts` 會把 React 一起拉進來）。

**風險分級的字串由產生器發**：`generated.ts` 現在出 `export const RISKS = [...] as const`
與 `export type Risk = (typeof RISKS)[number]`，`opInfo` 用 `z.enum(RISKS)`。原本只有型別
沒有值，zod 需要一份執行期的清單，而手抄第二份就是等著漂走的那一份。

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
- **日期對著工作日曆挑**：休假、加班與打卡跳過工作日曆上每一種非工作日，不只週末。
  `request_leave` 拒絕沒有任何工作時數的區間，而只跳週末的寫法在週五是國定假日時
  （2026-09-25 中秋節）會整段落在非工作日上，種到一半就停——示範站每晚重建跑的就是
  這條指令，停在那裡，`demo` 帳號與凍結共用帳號的 trigger 都不會建立。`today` 是
  記帳日期，與伺服器的 `today()` 同一套；UTC 日期在台北凌晨重建時是前一天。
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
財務。誰可以簽核什麼、AI 能呼叫哪些工具、備份要送去哪裡，講的是這套系統
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

**忘記密碼由管理員重新發一組臨時密碼。** 這件事原本只有 shell 上的
`laysi-admin reset-password`：密碼只顯示一次、資料庫只有 Argon2 雜湊，弄丟就沒有別的
出口，而會弄丟的人正是最不可能有伺服器 shell 的人。`reset_user_password` 把同一段邏輯
搬到畫面上，並多做一件 CLI 沒做的事——**撤銷該帳號目前所有登入工作階段**：換密碼卻讓
舊密碼開的 session 繼續活著，等於沒換。

兩個拒絕：`password_hash IS NULL` 的帳號只用外部身分登入，發一組密碼給它等於把
SSO 帳號變成密碼帳號，繞過身分提供者的多因素；自己的帳號由伺服器拒絕，比對的鍵與
`change_user_role` 相同（`OpCtx.username`，MCP Token 一律拒絕）——拿到別人 session 的
人對那個帳號本身重設，就跳過了自行變更密碼要求的目前密碼，還順手把本人的每一個
session 登出。畫面上同樣不給按：重設會把自己登出，而密碼視窗會隨著跳回登入頁一起
消失，何況真的忘記自己的密碼時根本進不到這個畫面。

**自行變更密碼同樣結束其他登入工作階段，但留下自己這一個。** 自己換密碼最常見的理由
正是懷疑密碼被別人知道，不撤就讓對方繼續登入到閒置逾時為止，而對方只要持續點擊就續得
下去。與重設的分界在執行的人是不是帳號本人：本人按下變更，把自己也登出沒有意義。
MCP Token 不在範圍內——它有自己的撤銷畫面，日常換一次密碼就弄壞每一個連接器。稽核摘要
寫出結束了幾個工作階段。

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
根本還沒有員工編號可填。`update_user_account` 因此存在：它改得動顯示名稱、
電子郵件、啟用狀態與員工連結，改不動 `role_code` 與密碼——角色走 `change_user_role`，
密碼只有本人改得動；而停用最後一個啟用中的管理員帳號會被擋下，否則沒有人打得開這個
畫面。`delete_user_account` 擋的是同兩件事再加一件：刪掉最後一個
帳號會讓整座安裝沒有人能登入，而 `install_bootstrap` 已被認領、網頁初始化不會重開。

**薪資不是「人存在」的前提。** 上面那段講的補救原本補不完：`employee.base_salary`
原本寫著 `CHECK (base_salary > 0)`，於是建人一定要先有薪資，而
薪資是人資的資料、帳號是管理員的工作，全新安裝的第一天沒有人能起頭。每一環單看都
成立，串起來讓最常見的第一件事做不到。

該 CHECK 已放寬成 `>= 0`，0 表示尚未設定。真正該擋的地方改在計薪：
`run_payroll` 遇到 0 底薪回「尚未設定底薪」而不是發出 0 元薪資單——**零元薪資單看
起來像算出來的，錯誤訊息不會**。

**建帳號一定會有一筆員工主檔，沒有開關。** `link_employee()` 的規則只有一條：
既有的員工編號就連結，指不到人（含留空）就建一筆只有姓名與到職日的員工主檔，編號
留空時由 `create_employee` 在寫入的交易內依 `E###` 系列配號（見「主檔編號」；不是
這個形狀的編號不參與計算，自訂編號的名冊不會被重新編過），部門與金額仍留給人資。原本這件事是一個
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

## 法規更新的兩條路

`reg_ruleset` 有兩個寫入來源，兩者都不會覆寫既有的 `(domain, version)`。

**管理員自己建**：「法規版本」畫面的 `save_regulation`，存檔前跑 `probe()`。修法當天
就能上線，不必等發版。

**向廠商取**：「法規版本」的 `refresh_regulations`（`AdminOnly`）向
`https://raw.githubusercontent.com/Laysi/laysi-one-regulation-feed/main/manifest.json` 取修法後的版本，套用過的識別字
記在 `reg_update`。**修法不必換映像**：費率是資料，換一版映像只為了改一個數字，等於
把上線時程綁在發版時程上。

**送的是 payload，不是 SQL**。發布端供應值，寫入的敘述由伺服器自己組，所以那個網址
拿不到 `reg_ruleset` 以外的任何一張表。每一份 payload 進資料庫前都走
`probe()`，與手打進去的那一份同一道檢查——形狀不對就擋在寫入當下，不是等到發薪那天。

**它不放在 `migrations/`**：費率是資料不是 schema。0001 把六十三份 migration 合併成
一份，下一次合併時一則費率修正會擋在路上，而合併保不住「這個安裝已經收過那一版並且
把它刪掉了」。兩套各記各的，版本號永遠不會撞。

三個約束：

- **`ON CONFLICT (domain, version) DO NOTHING`**。管理員搶先自己建過同一版就不動它。
- **一個資料庫只套用一次**。記錄列與 ruleset 同一個交易，先寫記錄再寫版本，所以兩台
  伺服器同時更新時第二台會擋在主鍵上、然後看到零列。刪掉的版本不會在下一次更新回來——
  這正是 `seed_regulations()` 的「只寫還沒有任何版本的領域」要守的性質；它現在也以同一個
  名稱記錄寫過的出廠版本。**唯一的缺口是請假工資之前的出廠版本**：當時的
  `seed_regulations()` 寫入時不留記錄，所以在第一次更新之前刪掉的那些會回來一次。
- **`file` 一旦發布就不能改**。它是 `reg_update` 的鍵；換一個名字等於要每個安裝再套用
  一次，包括刻意把那一版刪掉的那些。它帶著 `.sql` 尾綴，那是更新走 `include_str!` 那
  幾版留下的檔名，留著才不會對已經收過的安裝再送一次。

`manifest.json` 由 `scripts/regulation-manifest.ts` 從 `backend/regulation/**/*.toml`
產出，`.github/workflows/regulation-feed.yml` 在那些檔案有變動時推到發布用的公開
儲存庫。**只有 TOML 一份來源**：它是人編輯的地方，也是 `laysi-regulation` 測試與
`probe()` 唯一涵蓋得到的形式。同一份 TOML 對新安裝是出廠預設值，對既有安裝是更新，
兩條路寫進去的是同一組數字。

`reg_ruleset` 沒有 `checksum` 欄位。它曾經是寫入時算的 FNV-1a，而 `regulations()` 的
SELECT 不取它、`resolve()` 也看不到它，一個沒人讀的 `NOT NULL` 欄位正好擋著手寫的
INSERT。

## 產品授權

授權伺服器在 `license-server/`：同一個 repo，另一個 cargo workspace，另一個資料庫，
另一個部署（主機上 `docker compose up -d --build`，不走 GHCR 也不走 release 標籤）。
經銷商與代理商在那裡開立啟動金鑰、續約、解除綁定與月結對帳；本產品是單租戶，資料表不帶
`company_id`，把多租戶的通路管理塞進來等於要為經銷商在每一張表上加一個欄位。
執行期兩邊只靠一個端點相接：`POST /v1/license/check`。

**放同一個 repo 的理由是那份 payload**：`backend/crates/laysi-license-token` 定義
`LicensePayload`，簽的一方與驗的一方都 `use` 它。兩個 repo 的時候它是兩份手抄的複本，
改一個欄位名不會讓任何一邊的建置失敗——症狀是客戶端解不出授權檔，而畫面只說得出
「授權檔無法驗證」。合併之後那是編譯錯誤。附帶的是授權伺服器從此有 CI（`ci.yml` 的
`license-server` job），它自己那個 repo 一條 workflow 都沒有。

**兩個 workspace 不是一個**：授權伺服器不需要 `aws-sdk-s3` 與 `openidconnect`，併成一個
會讓每一次產品建置都拖著它們。它的映像 build context 因此是 repo 根目錄而不是
`license-server/`（要拿得到那個共用 crate），`.dockerignore` 只排掉
`license-server/target/`、`secrets/` 與 `.dev/`——**`secrets/` 裡是簽章私鑰，那個目錄
一定要排掉**。

**啟用與定期檢查是同一個呼叫**。第一次呼叫綁定主機並把授權推到使用中，之後每一次都是續
租。沒有第二個端點，因為沒有第二個決定。

### 簽章，不是資料庫裡的一列

授權伺服器以 Ed25519 簽出一份 payload，本產品只帶公鑰（`ops/license.rs` 的
`PUBLIC_KEY_HEX`）。三件事因此成立：

- **斷線不是授權問題**。每次成功的檢查續 45 天的租期，防火牆改設定、連假、授權伺服器
  自己維護，帳都照常做。
- **客戶自己的資料庫偽造不出授權**。畫面上每一個授權欄位都由 `evaluate()` 從簽章
  payload 讀出，不從欄位讀，所以改一列改不動任何人看得到的東西。
- **失效是唯讀，不是鎖死**。帳是客戶的：報表、查詢、匯出與備份照常，只有寫入停下來。

`ed25519-dalek` 早就在依賴樹裡（`openidconnect` 帶進來的），所以驗證這一半不增加任何
crate。

### 租期只有一個日期

`not_after` 決定整套狀態機：距今超過 30 天是使用中，0 到 30 天是即將到期，過期 30 天內
是寬限（功能不變），更早就是唯讀。`expires_on` 是經銷商賣的合約到期日，只給人看，不做
任何判斷。

**合約到期時伺服器仍然發出授權檔**，`not_after` 是過去的日期。回 403 帶不了到期日，也帶
不了該找哪一家經銷商，而那正是讀的人接下來要用的兩件事。真正回 403 的只有三種：註銷、
暫停，以及已經綁在另一台主機。

租期 45 天配上寬限 30 天，是一個完全不對外連線的安裝從最後一次成功檢查算起還能寫入的
時間。距離夠遠，失效是一通電話而不是一次中斷。

### `dispatch` 的第 0 步問的是 `writes()`，不是 `risk()`

兩個問題不同：`risk()` 說誰可以叫，`writes()` 說改了什麼。`list_payslips` 帶著負責人底線
卻什麼都不寫，用 `risk() != ReadOnly` 當判準，會讓失效的安裝連自己的薪資單都印不出來——
而那正是要離開的客戶最需要拿走的東西。

預設由 `risk()` 推導，所以只有少數「帶身分底線的讀取」需要覆寫 `writes()`：
`list_payslips`、`list_withholding_statements`、`get_backup_setting`、`get_oidc_setting`、
`list_user_accounts`、`get_license`、`get_update_ticket`、`get_update_result`。`reads_are_not_writes_whatever_floor_they_carry` 依
命名動詞把關，新增一個 `get_*` 而忘了覆寫就會紅。

`activate_license` 是唯一豁免的寫入，它是離開唯讀的那條路。被擋下的呼叫照樣寫稽核。
判斷讀行程內的 atomic 而不是資料表，所以常見路徑不多一次查詢；`refresh()` 在啟動時與每
一次檢查後重新發布，背景工作每 12 小時也重算一次——狀態跟著日期走，而日期自己會動。

### 主機識別碼綁的是主機，不是資料庫

`laysi_store::license::fingerprint()` 取 `/etc/machine-id`（取不到才退回 `install_id`）。
**不把兩者混在一起**：那會讓同一台伺服器重裝資料庫變成第二台主機，於是每一次重建都要先
請經銷商解除綁定，而重建是管理員最常做的事。退路存在是因為沒有主機識別碼的部署會全部
雜湊到同一個值，第一個綁走，其餘都被告知自己是別人。

compose 把 `/etc/machine-id` 唯讀掛進 `api` 與 `mcp`，**兩個服務都要掛**：它們對同一個
資料庫算出兩個識別碼的話，其中一個會認為自己未授權。已經在跑的安裝加上這個掛載會換一次
識別碼，要由經銷商解除綁定一次。

授權伺服器對指紋不同一律拒絕並寫進 `license_denial`，**看到就重綁等於沒有綁**。換主機是
經銷商在後台按一下「解除綁定」，而原本那台在租期內照常運作，所以一次真正的搬機有 45 天
可以處理。另有一個滾動的 `nonce`：每次回應發一個、下次必須帶回來，兩份複本會互相沖銷。
**只計數不擋**——從昨天的備份還原與整份複製長得一模一樣，用啟發式把客戶鎖在自己的帳本外
面是更嚴重的錯，所以那是後台上的一個數字，由人決定。

**同一個資料庫一次只有一個檢查在途**。`api` 與 `mcp` 都跑每日檢查迴圈，同時建立的兩個
容器每 24 小時同一刻到期；兩邊各自讀 nonce 再送出，後到的那一次帶的就是已經被換掉的
nonce，授權後台因此每天記一筆「疑似重複安裝」（#135）。兩份回應存回的先後顛倒時，資料庫
留下的也不是授權伺服器最後發出的那一個，下一次單獨檢查同樣對不上。所以 `check_now` 與
`check_if_due` 都經過 `Store::lock_license`：讀 nonce、送出、存回在同一個交易裡，
`license_state` 那一列 `FOR UPDATE`，後到的程序等前一個存完才讀；每日檢查在鎖內重判是否
到期，等到的那一個看到剛檢查過就不送。持鎖期間包含一次最長 20 秒的請求，而
`license_state` 只有這條路會寫，`FOR UPDATE` 不擋一般讀取。只讓 `api` 跑迴圈擋不住
「立即驗證」與迴圈相撞，也擋不住存回順序顛倒。

### 啟用在建立系統管理員帳號之前

`POST /v1/auth/bootstrap` 收 `activation_key`，畫面是 `screens/Init.tsx` 的第一個欄位。
驗不過就什麼都不建，安裝維持在它原本的狀態；順序是
驗證輸入 → 流量限制 → 初始化代碼 → 啟用授權 → 雜湊密碼 → 建帳號，所以打錯的初始化代碼
不會產生對外連線。

**導入授權機制之前就存在的安裝有 90 天寬限**：0007 看得到 `install_bootstrap` 已經有一列
才寫 `grace_until`，全新的資料庫沒有。少了它，升級的那一天所有既有客戶一起變成唯讀。
第一次成功檢查會把 `grace_until` 清掉——已經有真授權的安裝不再是需要寬限的那個案例。

### 回報什麼

每天一次，三個欄位：授權編號、主機識別碼、系統版本。主機識別碼是 `/etc/machine-id` 的
BLAKE3，認得出是不是同一台機器，認不出那是誰。

**在職人數曾經是第四項，拿掉了**。它是客戶的營運數字，而授權判斷從來不看它——`not_after`
由租期決定，跟公司多大無關。留著它等於為了一份沒人讀的統計，在合約裡多一條要解釋的
條款。授權伺服器的 `license` 資料表一併移除該欄位。

**沒有離線授權檔這條路**。曾經有過：經銷商拿主機識別碼產生一份 `not_after` 等於合約到期日
的授權檔，貼進畫面就啟用，期間不回報。它與線上那條路是同一把私鑰簽的、同樣綁主機指紋，
偽造難度完全相同，拿掉的理由不是它容易破——是那份檔案**發出去就撤銷不了**（線上安裝每次
檢查續 45 天，停止發放約 75 天後自動轉唯讀），而且那台主機在後台完全看不見，裝了幾份無從
得知。

真正沒有對外連線的安裝本來也撐不住這個產品：`refresh_regulations` 與
`refresh_work_calendar` 都取不到，`seed_regulations()` 只補沒有任何版本的領域，於是費率永遠停在出廠
映像那一版而畫面上沒有任何跡象——一個算錯勞健保又報錯扣繳的安裝，比一個過期的安裝糟。
遠端連接器（`/mcp` 的 OAuth）同樣要對得到外面。授權要撤銷得掉、費率要跟得上，兩件事指向
同一個前提。

**`function-test.ts` 因此需要一組授權**：`LAYSI_ACTIVATION_KEY` 搭配 `LICENSE_SERVER_URL`。
它本來就需要容器起著，而初始化在建帳號之前啟用，是產品行為而不是測試環境的例外。

## 發布版本清單

每一個 `v*` 標籤由 `release.yml` 簽一份 `release-manifest.txt`，與
`docker-compose.prod.yml` 一起附在 GitHub release 上：版本號，加上 api、web、backup
三份映像的 index digest。`mcp` 與 `api` 跑同一份映像。主機端的更新程式（見下一節）只裝它
驗過的清單上列出的 digest，用的 compose 檔也取自那份驗過 digest 的 api 映像，所以釘住 api
的 digest 就同時釘住它。release 上另外附的 `docker-compose.prod.yml`
不在簽章範圍內，那一份給手動安裝的人用，更新程式不讀它。ghcr.io 上的 tag 改得動，digest
改不動，版本字串也說不出哪三份映像屬於同一版。

**另一把金鑰，不是授權金鑰**。授權私鑰在授權伺服器上，同一把金鑰若也決定每一台機器跑
哪一份映像，那台伺服器被攻破就等於在每一台售出的機器上取得程式執行能力。版本清單的私鑰
是 GitHub secret `RELEASE_MANIFEST_SIGNING_KEY`，只進 `release.yml` 的兩個 job——建映像前
比對公鑰的 `signing-key` 與簽章的 `manifest`，兩者都只有唯讀的 `GITHUB_TOKEN`，除了 checkout、
釘住 commit 的 Bun 與 artifact 之外什麼都不跑——不進授權伺服器也不進任何映像。授權伺服器只負責轉送清單（#62），共用 crate 裡沒有任何簽章
程式，所以它改不動也產生不出一份清單。已知並接受的風險：GitHub 同時握有推映像的權限與這把
私鑰。

### 格式：簽的是位元組，不是 JSON

```text
laysi-release-manifest 1
version v0.1.13
image api sha256:<64 個小寫十六進位字元>
image web sha256:<…>
image backup sha256:<…>
signature <金鑰識別碼，16 個小寫十六進位字元> <Ed25519 簽章，128 個小寫十六進位字元>
```

前五行（各含換行）是本文，每一行 `signature` 都簽本文的原始位元組，任何語言的簽章端與
Rust 驗證端不必約定正規化形式。純文字也讓授權伺服器只能整段轉送：放進 JSON 字串裡
不會被重新序列化。授權檔簽的是 base64url 文字，而每一份清單本文都以含空格的標頭行開頭，
所以兩種簽章就算同一把金鑰也無法互相冒充。

驗證定義在 `laysi-license-token` 的 `release` 模組，產品、授權伺服器與更新程式共用：

- 格式嚴格，不修補：每行以 `\n` 結尾（含最後一行）、不得有空行或 `\r`、三份映像依
  api、web、backup 順序各一行、digest 只收小寫十六進位。被改寫過的位元組一律拒絕。
- 上限 `MAX_MANIFEST_BYTES`（4096 位元組），讀檔或讀回應時就要先擋。
- 金鑰識別碼是原始 32 位元組公鑰 SHA-256 的前 8 位元組，由公鑰推導而不是自訂字串，所以
  不會有人把同一個名字發給兩把金鑰。截成 64 位元仍然可能碰撞，機率可以忽略但不是零：
  `the_compiled_in_keys_parse_and_are_distinct` 擋下識別碼重複的 `TRUSTED_KEYS`，真的碰上
  就換一把新金鑰。識別碼只用來挑出要比對的信任公鑰，驗章一律用完整公鑰；挑錯一把的結果是
  簽章驗不過而拒絕，不會讓清單通過。
- 認不得的識別碼略過；信任的識別碼，其簽章一定要有效；至少要有一個信任的簽章；同一個
  識別碼出現兩次拒絕。換金鑰時新舊兩把一起簽，跳過中間版本的安裝也接得上。
- 更新程式的決定：清單版本高於已安裝版本才安裝；相同不動；較低只記下原因，所以重放舊
  清單沒有任何效果。已安裝的版本讀不出來是拒絕，不當成「比較舊」。版本是 `v` 加語意化
  版本（`v1.0.0-rc.2` < `v1.0.0-rc.10` < `v1.0.0`），不收前導零與 `+` 建置資訊——兩者都
  會讓兩個字串指同一版。

**信任的公鑰是 `release::TRUSTED_KEYS`**，不放在 `ops/license.rs`：更新程式只依賴
`laysi-license-token`，授權伺服器收清單時也要驗，三方要同一份清單。`ops/license.rs` 的
測試確保它與 `PUBLIC_KEY_HEX` 不同。

**這個 crate 不能帶進需要 C 編譯器的依賴**：更新程式是 musl 靜態執行檔，只用 `rust-lld`
交叉編譯。所以驗章只多了 `ed25519-dalek`（金鑰識別碼的 `sha2` 本來就在它的依賴樹裡），
沒有 `ring`、`blake3` 或任何帶 `cc` 的 crate；`ci.yml` 對兩個 musl target 檢查依賴樹。

### `release.yml` 的順序

1. `signing-key`：建映像之前，由 secret 推出公鑰，與 `TRUSTED_KEYS` 比對，不在清單裡就
   讓發布失敗——不然 ghcr.io 上會留下沒有任何安裝接受的映像。比照 `license-deploy.yml`
   比對 `PUBLIC_KEY_HEX` 的那一步。同一個 job 也擋下已經附過清單的版本。
2. `images`：每份映像把這次推上去的 index digest 寫成 artifact（matrix job 的 output
   互相覆蓋，不能用）。
3. `manifest`：獨立的 job 簽章，執行器上除了 checkout、Bun 與 artifact 之外什麼都不跑；
   `release` job 會把 `AI_API_KEY` 交給第三方 action，兩者不同機。
4. `release`：先用 Rust 驗證端與這個標籤編進去的公鑰驗過清單、比對版本等於標籤，才附上。
5. `demo`：在 `release` 之後。清單沒簽成或驗不過的標籤，任何安裝都裝不了，示範站也不跑。

**一個版本只建一次**。映像建置不是逐位元組可重現的，整套重跑會在同一個標籤下推出新的
digest；已經照第一份清單更新的安裝認為自己是最新版，跑的卻是別人不再跑的那一份。所以
`signing-key` 看到 release 上已經有 `release-manifest.txt` 就讓整個發布失敗，`release` 也只
在新舊清單本文相同時才以 `--clobber` 換掉它（同樣的 digest 簽出同樣的位元組，只重跑後段
的 job 等於換成自己）。要改就打一個新標籤。兩處都問 `.github/published-manifest.sh`：只有
release 或檔案不存在才算「還沒發布」，GitHub 回任何其他錯誤都讓發布失敗，不當成沒有。

簽章腳本是 `scripts/release-manifest.ts`（`sign`／`public-key`／`keygen`）。私鑰只從環境
變數讀，編碼與 `LICENSE_SIGNING_KEY` 相同（32 位元組 seed 的十六進位）；多把以空白分隔就
各簽一行。`keygen` 在記憶體裡產生金鑰，私鑰經 stdin 直接交給 `gh secret set`，只印公鑰與
識別碼；secret 已經存在就拒絕，因為 GitHub 讀不回 secret，覆寫等於丟掉每一台安裝信任的那
把金鑰。`scripts/release-manifest-test.ts` 以隨機金鑰跑一次腳本再交給 Rust 驗證，並確認
`testdata/release-manifest.txt` 與腳本的輸出逐位元組相同（Ed25519 簽章是確定性的）。

## 主機端更新程式

出貨的機器沒有人進得了主機：客戶、經銷商與雷希都沒有。系統管理員在產品畫面上按更新，
結果只有兩種：以新版本回來，或者自己回到前一版、連同完整的資料庫。執行的是
`backend/crates/laysi-updater`，一支靜態 musl 的 Rust 執行檔（`rust-lld` 連結，不需要 C
工具鏈），以擁有這套 stack 的 rootless 使用者、從 `systemd --user` 執行。**放在主機上而不是
容器裡**：容器換不掉自己，而把 podman socket 交給一個容器等於交出整套系統。

它只依賴 `laysi-license-token`（驗章與共用型別）、`serde`、`serde_json` 與 `libc`，不依賴
`laysi-ops`／`laysi-store`——後兩者帶進 `ring`、`blake3` 與 `cc`。`ci.yml` 對兩個 musl target
檢查依賴樹，並編出兩份 static 執行檔。

**下一版的更新程式與 compose 檔都放在 api 映像裡**（`/usr/local/share/laysi-one/`，
`backend/Dockerfile`），由目前這一版取出來用，所以更新程式不另外列進版本清單：digest 釘住
映像，也就釘住這三個檔案。安全與復原的邏輯留在更新程式裡，不寫在映像內的腳本：一個有問題
的版本不該帶走用來從它復原的那段程式。

### 主機配置

`<root>` 是 `~/.local/share/laysi-one`（`LAYSI_UPDATER_ROOT` 可改），整棵由 stack 使用者
擁有：

| 路徑 | 權限 | 內容 |
|---|---|---|
| `<root>/` | 0700 | |
| `bin/laysi-updater`、`bin/laysi-updater.previous` | 0755 | 現行與前一版的更新程式 |
| `stack/` | 0700 | 兩份 compose 檔與 `.env`（0600），旁邊各一份 `.previous` |
| `state/installed` | 0600 | 已安裝的版本與三個 digest：取自驗過的清單，不從容器讀 |
| `state/journal`、`state/lock`、`state/updater.log`、`state/updater.env` | 0600 | 日誌、鎖、紀錄、工具路徑 |
| `backup/` | 0700 | 切換前的 `pg_dump -Fc`（0600），成功或回退完成即刪 |
| `incoming/` | 0700 | 從新版 api 映像取出的檔案 |
| `requests/` | 0755 | 掛進 api（`:U,z`），api 寫旗標 |
| `results/` | 0755 | 掛進 api 與 mcp（`:ro,z`，不加 `U`），更新程式寫結果、切換標記與備用開機標記 |

只有最後兩個目錄掛進容器。請求目錄只掛進 `api`，所以從 MCP 寫不了旗標；結果目錄以唯讀
掛進 `api` 與 `mcp`，兩者都要讀得到切換標記。日誌、資料庫備份、`.env` 與 compose 檔都不在
任何掛進容器的目錄裡。`:U` 把 `requests/` chown 給
api 的 uid 10001 在主機上對應的 subuid，stack 使用者從此刪不掉裡面的東西，清空走
`podman unshare`；不採「目錄設 1777」那條路，主機上不多一個人人可寫的目錄。

### 旗標與結果（api 容器內看到的樣子）

契約定義在 `laysi-license-token` 的 `update` 模組，產品與更新程式共用：

- `CONTAINER_REQUEST_DIR` = `/var/lib/laysi-update/requests`（可寫）。api 把
  `UpdateRequest::encode()` 寫到暫存名稱、fsync、chmod 0644，再 rename 成
  `request`（`REQUEST_FILE`）。內容是 `laysi-update-request 1`、`request <請求編號>`，
  後面逐位元組接上授權伺服器給的簽章版本清單。請求編號不在簽章範圍內，只用來對上結果；
  8–64 個英數與 `-`，同時是結果的檔名。
- `CONTAINER_RESULT_DIR` = `/var/lib/laysi-update/results`（api 與 mcp 皆唯讀）。檔名就是
  請求編號，`UpdateResult::decode()` 讀，上限 `MAX_RESULT_BYTES`。每一步都整份換掉，
  `UpdateStatus::is_final()` 之後不再變；保留最新的 `KEEP_RESULTS` 份。
- 切換標記 `SWITCHING_FILE`（`switching.marker`）：更新程式在第 4 步停下第一個服務之前
  寫入（暫存檔、fsync、rename），最終結果寫出之後才刪除。**api 與 mcp 在 `update::switching()`
  為真時拒絕所有寫入，讀取照常**：回退把資料庫還原成第 5 步的樣子，這段期間的寫入會被
  還原掉。判斷只看標記在不在，不需要請求編號，所以更新程式自己起的 api 與沒有送出請求的
  mcp 看到的是同一個答案。結果目錄不存在（沒有主機端更新程式的安裝，例如開發環境）時為假；
  `lstat` 遇到 `NotFound` 以外的錯誤時為真，被擋下的寫入可以重做，被還原的寫入不會有人
  知道。第 4 步之前的寫入由第 5 步的備份保留，所以 `accepted`／`preparing` 不擋。
  標記寫不進去就不停任何服務，回 `failed-unchanged`。斷電或程序中斷留下的標記，由下一次
  取得鎖的執行（開機與收尾 unit 都是）在發現沒有進行中的更新時刪掉；在那之前它留著，
  寧可多擋一段。名稱含 `.`，不可能是請求編號，清理舊結果時不會碰到它。
  拒絕寫入接在 `dispatch` 第 0a 步（`op.writes()`），見〈產品畫面的更新卡〉。
- 備用開機標記 `FALLBACK_BOOT_FILE`（`fallback-boot.marker`）：開機或收尾 unit 失敗後，
  fallback unit 以 `boot --fallback` 執行前一版更新程式；前一版取得鎖之後先寫入標記
  （暫存檔、rename，0644，內容 `at <時間>`），再照常收尾並啟動 stack。現行更新程式下一次
  開機或收尾成功時刪掉它。寫不進去、刪不掉都只記日誌，不影響啟動。api 只讀：
  `update::fallback_boot_in()` 在它是一般檔案時為真，任何錯誤（含目錄不存在）都是假，因為
  它只決定畫面多不多一句提示；`get_license` 以 `fallback_boot` 回給畫面。名稱含 `.`，清理
  舊結果時不會碰到。比這個參數舊的更新程式只看第一個參數，照常開機但不留標記；出貨的第一版
  就帶著它，所以只有演練機與開發機會遇到。標記追的是現行更新程式有沒有起來，不是 stack
  能不能用：停在 `rollback-failed` 而只起 postgres 的開機也算成功，照樣刪掉。
- 狀態：進行中 `accepted`→`preparing`→`switching`→（`rolling-back`）；結束
  `succeeded`、`succeeded-updater-kept`（新版已在執行，但新版更新程式自我檢查不通過，
  `reason` 說原因）、`failed-unchanged`（停機前就失敗，什麼都沒變）、`rolled-back`、
  `rollback-failed`，以及拒絕 `refused-signature`／`-downgrade`／`-same-version`／
  `-installed-unknown`／`-disk-space`／`-busy`／`-blocked`。每份都帶 `received`、
  `updated`，視情況帶 `offered`、`running`、`downtime-from`／`-to`、`reason`、
  `rollback-reason`、`tools`。停機時間從 `downtime-from`（第 4 步）量到 `downtime-to`。
  `rolled-back` 帶 `rollback-reason` 表示第一次還原失敗、重試成功，內容是第一次的原因。
  `tools` 是這次執行實際用的版本（`podman=4.9.3 podman-compose=1.6.0`），只記錄、不判斷。
- 旗標是不可信輸入：以目錄 fd 加 `O_NOFOLLOW | O_NONBLOCK` 開啟、`fstat` 確認是一般檔案、
  有大小上限，驗不過章就丟棄。symlink、FIFO、目錄都不會讓更新程式讀到目錄外的檔案或卡住。

### 一次更新

1. 寫下 `accepted`；驗章、拒絕降版，與已安裝版本相同就不做任何事。
2. 事前檢查磁碟：新映像加一份資料庫備份，上一版映像要留著。新映像在拉下來之前量不到
   （更新程式不經 podman 就問不了 registry），所以先假設新版至多是現行版映像的兩倍；
   不足就停在這裡。
3. 依 digest 拉映像，從新 api 映像取出 compose 檔與下一版更新程式。新 compose 檔要有
   postgres、rustfs、api、mcp、web、backup，自家三份映像各在自己的服務裡（api 與 mcp 用
   api 映像），每個映像都以 digest 指定，自家映像要等於清單上的 digest。拉完以實際大小
   再檢查一次磁碟。到這一步系統照常運作；失敗就刪掉拉下來的映像，回 `failed-unchanged`
   （空間不足是 `refused-disk-space`）。映像庫拒絕匿名拉取或找不到映像時，`reason` 只寫
   「無法取得 … 映像：映像庫拒絕提供或沒有這個映像」，podman 的原文寫進日誌。
4. 寫入切換標記，再停止 postgres 以外的每個服務，web、mcp 先停並確認停了。停機從這裡
   開始。
5. `pg_dump -Fc`，0600，`pg_restore --list` 確認完整。
6. 前一版的 compose 檔與 `.env` 另存同目錄，再換上新版的。
7. 跑完 `secrets`（冪等，新版多一個密鑰就在這裡產生），先起 postgres，再
   `--force-recreate` 其餘常駐服務，web 與 mcp 除外。一律帶 `--no-deps`：podman-compose
   把 `depends_on` 譯成 `--requires`，有它就刪不掉、換不掉被依賴的容器。
8. 等候成功：每個服務的最新容器在執行、是切換之後建立的、建立時的 digest 等於 compose
   檔指定的那個也正在執行它（多平台映像以 index digest 拉取時 podman 回報的就是 index
   digest）、自家映像等於清單、有 healthcheck 的是 healthy；反覆重啟的容器立刻算失敗。
   逾時 20 分鐘，涵蓋 migration。通過之後才起 web 與 mcp，以同樣條件確認（逾時 3 分鐘）。
9. 失敗：先放回前一版的檔案，確認 postgres 以外都停了，以**舊版的 postgres 映像**重建
   容器，`DROP DATABASE … WITH (FORCE)` 後 `pg_restore --create`（保留 ICU zh-TW 定序），
   成功才刪備份、起舊版並以第 8 步的條件確認。**還原失敗（含之前的 DROP）在同一次執行裡
   立刻重試一次**：先把第一次的原因寫進日誌（`restore-retry`），刪掉 postgres 以外已停止
   的容器與新版的映像釋出空間，重建舊版 postgres，再從同一份備份還原。備份從頭到尾不改寫，
   還原成功才刪。重試成功就照常起舊版並判斷，結果是 `rolled-back`，`rollback-reason`
   寫第一次失敗的原因；重試也失敗就是 `rollback-failed`，備份留著。回退不通過就停在舊版、
   不再重試。
10. 成功：新版更新程式先在本機跑 `self-check`（讀得了這次的日誌、寫得出 unit），通過才
    換上，前一版留在 `.previous`；接著刪掉現行與前一版以外的映像與資料庫備份。

**換回舊映像不構成回退**：sqlx 遇到資料庫裡有執行檔不認得的 migration 會回
`VersionMissing` 拒絕啟動，所以回退一定連同資料庫。**入口開放之後才判定失敗時，回退照樣
還原資料庫**：第 8 步通過核心之後 web 與 mcp 才啟動，之後若 web 或 mcp 自己沒通過（上限
是入口的逾時），資料庫仍要回到第 5 步。這段期間 api 與 mcp 要依切換標記拒絕寫入，寫入才
不會隨回退消失；`dispatch` 的第 0a 步已經接上這道檢查。結果的 `reason`
寫出入口開放了多久。

### 產品畫面的更新卡

「安全與營運」的「系統更新」卡（`components/UpdateCard.tsx`，狀態機在 `web/src/update.ts`）
是上面那份契約的畫面端。

**放行的版本存在 `license_state.release_manifest`**：授權檢查的回應帶一份簽章版本清單，
`ops/license.rs` 的 `released_manifest()` 用 `release::verify_with` 驗過才存，驗不過存空字串；
每次成功的檢查整份換掉，所以撤回放行也一併生效。存的是簽章全文、逐位元組不動，因為請求原樣
帶它，更新程式再驗一次。資料列只是一份副本：讀版本與寫請求每次都重新驗章，手改資料庫
換不到任何版本。`get_license` 因此多回四個欄位：`running_version`、`released_version`（驗過
章且 `decide()` 為 `Install` 才有，否則 `null`）、`updater_present`（請求目錄存在才為真，
沒有更新程式的安裝不畫按鈕，也不畫「更新期間」與「更新失敗時」兩列——那兩句講的是更新程式
的行為，由維運人員更新的安裝兩句都不成立），以及 `fallback_boot`（備用開機標記在不在，見
〈旗標與結果〉）。畫面只知道有沒有放行給這台機器，沒有放行版本時不說「最新」。

**兩個操作，同屬 `manage_license`**（migration `0025_release_manifest.sql` 整列改寫那條
permission，並種兩列 `mcp_tool_grant`，檔案接在 `laysi-ops` 測試的 `SCHEMA` 後面）：

| 操作 | `risk()` | `writes()` | `ui_only` | MCP 開關 |
|---|---|---|---|---|
| `get_update_ticket` | `AdminOnly` | false | 是 | 關閉，且開不起來 |
| `get_update_result` | `AdminOnly` | false | 否 | 關閉 |

`get_update_ticket` 把請求寫進旗標目錄（暫存檔、fsync、0644、rename、fsync 目錄）並回傳
請求編號。`writes()` 為 false，授權到期而唯讀的安裝照樣更新得了自己；名稱用 `get_` 開頭，
由 `reads_are_not_writes_whatever_floor_they_carry` 把關。`get_update_result { request_id }`
讀 `results/<編號>`，並核對檔案內的編號等於要求的編號；找不到時回 `found = false`，目錄
不存在也一樣，所以結果目錄裡上一次的結果不會被當成這一次的。

**`ui_only`** 是 `Operation` trait 的新掛鉤（預設 `None`），回傳該去的畫面名稱。MCP 呼叫
在讀 `mcp_tool_grant` 之前就被擋下，錯誤訊息請使用者到「安全與營運」執行；
`set_mcp_tool_grant` 也拒絕把這類操作打開。理由：旗標目錄只掛進 api 容器，AI 不該決定系統
何時停機。`schema-live-test` 的 `NEVER_CALLED` 列出 `get_update_ticket`，因為呼叫它會
真的寫旗標。

**更新期間唯讀**：`dispatch` 第 0a 步在授權檢查之前問 `op.writes()` 與
`UpdateDirs::switching()`，兩個 channel 都擋，回 `OpError::Updating`（`updating`，HTTP
403），被擋下的呼叫照樣寫稽核。授權底線的 `Unlicensed` 原本沒有對應的狀態碼，一併對到 403。
測試以 `Registry::in_update_dirs` 指向自己的目錄。

**卡片的狀態**：沒有放行版本、可更新（按鈕依 `permissions` 與 `updater_present` 決定畫不畫，
沒有權限時印誰維護）、進行中，以及六種結束：

| 狀態 | 條件 |
|---|---|
| 成功 | 結果為 `succeeded`／`succeeded-updater-kept`，或結果讀不到而 `/v1/meta` 的版本已變 |
| 失敗並已回復 | `failed-unchanged`／`rolled-back`，說出回到的版本 |
| 失敗且未回復 | `rollback-failed` |
| 沒有執行 | 任一種 `refused-*`，每一種各有一句；或「已接收」在 60 秒內從未出現 |
| 無法確認 | 「已接收」出現過或 api 曾經消失，卻在 45 分鐘內沒有最終結果，而最後一次輪詢有回應 |
| 沒有回應 | 同上，但最後一次輪詢 `/v1/meta` 與 `get_update_result` 都沒有回應 |

**前一版更新程式接手啟動時**：`fallback_boot` 為真，卡片在事實列下方多一句
`FALLBACK_BOOT_MESSAGE`（`web/src/update.ts` 的 `updaterNotice`），與上表的狀態並存，有沒有
放行版本都畫。系統照常可用，只有主機的 systemd 紀錄看得到這件事，而機器每次開機都靠前一版，
直到有人按「更新」才會發現——那時現行更新程式仍起不來，請求回 `failed-unchanged`。所以按鈕
照畫，提示請人洽經銷商檢修。現行更新程式下一次啟動成功，標記刪掉，提示跟著消失。

**「沒有執行」只留給拒絕，以及「已接收」從未出現**。已接收之後逾時，更新可能已經執行，說
沒有執行會讓人重按；那一格說無法確認結果、請重新整理本頁查看目前版本。輪詢同時問
`/v1/meta` 與 `get_update_result`（帶請求編號），每次 5 秒逾時，api 在換版途中消失時
回應失敗只會記為「曾經消失」，不會讓畫面壞掉。

**沒有回應另成一格**：回退失敗而保留了資料庫備份時，更新程式只啟動 postgres（`machine.rs`
的 `start_what_is_safe`），api 不會回來。這時請人重新整理，換來的是一個讀不到任何東西的頁面，
卡片上的結果也跟著消失；所以那一格不畫「重新載入」，只請使用者盡快洽經銷商。兩個端點都沒有
回應才算，`get_update_result` 失敗而 `/v1/meta` 有回應仍是無法確認。

`refused-disk-space` 的那一句帶出結果檔的 `reason`（更新程式算出的剩餘與需要的容量），
沒有 `reason` 才用固定句；其餘拒絕的 `reason` 寫給維運人員看，畫面不印。結束時卡片重讀授權、
`/v1/meta`、`list_api_tokens`、`get_system_health` 與 `/healthz`，讀不到就保留原值：同一頁
「系統健康」的版本與 migration 才不會停在更新前，而 api 不在時卡片也不會因為授權讀不到而整張
消失。讀得到時一併清掉頁首的錯誤，停機期間重讀失敗留下的「伺服器回應異常」不會在更新完成後
還掛著。

**整頁重讀時也保留授權**：分頁重新變成可見時 App 會 `reload()`，「安全與營運」整頁重讀。
停機那一分鐘裡切一次分頁，`get_license` 必定失敗；原本這時把授權清成 `null`，更新卡連同它
正在等的結果一起被拆掉，api 回來之後也不會自己出現。現在連不上伺服器（`update.ts` 的
`serverUnreachable`：不是 `OpFailed`，或代碼為 `network`）就保留上一次的值，只有操作本身
回拒絕才清掉。

**重新整理後接續**：請求編號與已看到的狀態（`seenResult`、`wentDown`、起始版本與時間）存在
`localStorage` 的 `laysi.update.run`。選 `localStorage` 而不是 `sessionStorage`，是因為更新
是以分鐘計的，管理員可能關掉分頁再回來；兩個分頁同時開著只是各自輪詢同一個編號，結果
相同。卡片掛載時讀回，繼續輪詢同一個編號；出現最終狀態、顯示出來的同時清除。讀回的編號
只用來輪詢，新的更新永遠向 `get_update_ticket` 要新的編號，所以不會把舊編號當成新的更新：
結果已是最終的編號只會顯示那個結果一次，然後清除。超過一天、格式不符或無法讀取的紀錄
一律丟掉。這些規則都在 `update.ts` 的純函式裡，由 `scripts/update-test.ts` 驗證。

**停機時間**：畫面寫「約一分鐘」，只存在 `update.ts` 的 `DOWNTIME_PHRASE`。數字來自 #61 的
CI 端對端測試，從第 4 步停止服務量到最終結果，成功路徑 18–20 秒、最長 30 秒，用的是假
映像，真實映像的 migration 與拉取不在其中；取整到一分鐘留出餘裕。#64 的實機演練量到
實測值之後換掉這個字串。

### 日誌與開機

每一步開始前把狀態寫進 `state/journal`（暫存檔、fsync、rename、fsync 目錄），所以任何
時間點斷電，開機都知道下一步。最終結果寫進結果目錄之後才刪日誌，寫不進去就留著日誌讓
下一次收尾再寫；收到旗標卻在寫日誌之前就中斷的請求，清空請求目錄時回
`failed-unchanged`。分界在切換：

| 日誌狀態 | 中斷後 |
|---|---|
| `preparing`、`prepared` | 什麼都沒變：刪掉拉下來的映像，回 `failed-unchanged` |
| `stopping`、`backed-up` | 起舊版，刪備份，回 `rolled-back` |
| `files-saved`、`files-swapped` | 先放回前一版檔案，其餘同上 |
| `switching`、`core-up` | 停掉仍在執行的服務，重做切換再判斷；不通過就回退 |
| `committed`、`finishing` | 做完：記下新版、換更新程式、清理 |
| `rolling-back` | 重做回退 |
| `restoring` | 重新還原資料庫；`restore-retry` 已寫下時是續做那一次重試，不會再開始第二次 |
| `restored` | 起舊版並判斷 |
| `rollback-failed` | 終止：之後只啟動 stack，不再還原；還原本身失敗、備份留著時只啟動 postgres |

**斷電會留下讀不了的容器**：斷電前一刻建立的容器，它那一層的 `link` 與 `lower` 檔已經
建立卻是空的；`podman ps` 列得出它，任何 inspect 都失敗在 `getting graph driver info`，
後面接著 `readlink …/overlay: invalid argument`——空的 link 被讀成 overlay 目錄本身。
容器不存資料，所以列容器時 inspect 失敗在這一整串、又不在執行的專案容器一律 `podman rm --force`，
之後照常重建；在執行、暫停或正在停止的，以及其他錯誤（同一個開頭底下的權限與 I/O 錯誤也算），
照樣回錯誤。少了這一步，`rolling-back` 斷電之後重做回退的第一步就失敗，直接落到
`rollback-failed`，重開機也起不來。

日誌存在、階段在 `stopping` 之後且結果還不是最終時，切換標記一定在；收尾一個這樣的日誌時
更新程式先補寫標記。最終結果寫出、日誌刪掉之後才刪標記，所以中斷在兩者之間也只會多擋一段。

**開機時 stack 只由更新程式啟動**：compose 用 `unless-stopped`，而 rootless 的
`podman-restart.service` 只啟動 `restart: always` 的容器；任何服務都不得設 `always`，
否則它會在收尾之前被啟動。順序是先收尾日誌、再啟動 stack；日誌讀不懂或收尾失敗時什麼都
不啟動、結束碼非零，交給前一版。

systemd unit 由更新程式自己寫出（`units.rs`），跟著更新程式一起換：

- `laysi-updater.path` 監看 `requests/request`，觸發 `laysi-updater.service`（oneshot）。
  `ExecStopPost=` 在每一次結束時清空請求目錄，失敗也一樣——檔案還在，path unit 會在
  service 結束時再觸發、一秒五次之後轉為 `failed` 而不再監看。執行期間寫入的旗標因此被丟棄
  並回 `refused-busy`，不排隊也不合併；`StartLimitIntervalSec=0`。
  例外是執行本身失敗、而且沒有進行中的更新（沒有日誌）：沒有結果的旗標改回
  `failed-unchanged`（更新程式在開始更新前中斷）。現行更新程式根本起不來時，旗標從來沒有
  被讀過，回 busy 會讓畫面請人稍後再試一個每次都失敗的更新程式。執行成敗取 systemd 交給
  `ExecStopPost=` 的 `SERVICE_RESULT`。這時回答旗標的是前一版的 `discard-requests`，所以
  前一版也帶著這條規則才生效。
  它排在開機 unit 之後才開始監看，並設 `DefaultDependencies=no`：path unit 預設排在
  `paths.target` 之前，而開機 unit 是 service、排在 `basic.target`（也就在 `paths.target`）
  之後，兩者相加是循環，systemd 每次開機都刪掉 path unit 的啟動工作，重開過的機器就再也
  收不到更新請求。端對端測試只重啟開機 unit、不經過 user manager 開機，看不到這件事。
- `laysi-updater-boot.service`：開機收尾並啟動 stack，`ExecStop=` 在關機時依序停下 web／
  mcp、其餘、postgres。`OnFailure=` 啟動 `laysi-updater-fallback.service`，以
  `bin/laysi-updater.previous boot --fallback` 再做一次，並留下備用開機標記。
- `laysi-updater.service` 失敗時（例如程序被殺），`OnFailure=` 的
  `laysi-updater-recover.service` 立刻收尾，不必等下一次開機；它失敗同樣交給前一版。
- 每一次執行（旗標、開機、收尾）都先取 `state/lock` 的 `flock`。旗標那次拿不到就回
  `refused-busy`；開機那次等鎖。

podman 在 systemd 底下會把 conmon 留在呼叫的 unit 的 cgroup 裡，oneshot 一結束就連同容器
被殺掉，所以更新程式呼叫 podman 時移除 `INVOCATION_ID`。

對外埠的轉送程序 `rootlessport` 不看那個變數，一律留在啟動容器的那個 unit 底下。開機 unit
有 `RemainAfterExit=yes`，cgroup 留著所以沒事；更新與收尾兩個 unit 跑完就結束，轉送程序
跟著被收掉，容器全部健康而 8088、8089 連不上，要重開機才回來。所以 `compose up` 一律經
`systemd-run --user --scope` 在自己的 scope 裡執行，scope 在最後一個程序結束時自己消失。
`systemd-run` 走 user manager 的私有 socket，沒有 `dbus-user-session` 也能用。只看容器
狀態看不出這件事，所以端對端測試在每一次收斂後都連一次兩個對外埠。

`systemd-run --scope` 會替它執行的程式重新設一個 `INVOCATION_ID`，所以 scope 裡再以
`env -u` 拿掉一次，conmon 才回到 podman 自己開的 `libpod-conmon-*.scope`。那個 scope 不帶
預設相依（`DefaultDependencies=no`），這個 scope 也照做：帶著預設相依的 scope 會在關機時
與開機 unit 同時停下，conmon 在 `ExecStop=` 依序停 stack、postgres 還沒停好之前就被殺掉。

### 出貨與 compose

`install` 是唯一的出貨步驟：以 stack 使用者執行，收一份簽章版本清單，先驗章並檢查主機，
才建好目錄、放上更新程式、從那一版的 api 映像取 compose 檔並檢查、寫 `.env` 與 unit、確認
lingering（沒有就 `loginctl enable-linger`，失敗請 root 執行一次），最後寫第一份
`state/installed` 並啟動開機 unit——它先跑完 `secrets` 才啟動其餘服務。寫下
`state/installed` 之前失敗可以直接重跑；寫下之後就算安裝完成，由開機 unit 負責啟動。

**工具版本在 `install` 嚴格檢查，之後只記錄**。podman 4.9.3（Ubuntu 24.04）與以 uv 安裝的
podman-compose 1.6.0 是驗證過的組合，`updater-e2e.yml` 釘住這兩個版本。`install` 要求三件事，
缺一項就以繁體中文說明並拒絕：podman-compose 恰好是 `install.rs` 的
`PODMAN_COMPOSE_VERSION`、podman 用 netavark 網路後端、podman 找得到 aardvark-dns（服務
以名稱互找，Ubuntu 的 CNI 沒有 DNS 外掛）。`updater-e2e.yml` 的 `PODMAN_COMPOSE_VERSION`
與那個常數不同時，`laysi-updater` 的單元測試不通過。之後的每一次執行都不因版本拒絕，只把
實際版本寫進結果的 `tools`：主機事後升級工具時，能放寬檢查的只有更新的更新程式，而它正是
被拒絕的那一次要裝上的。

**自家映像必須公開**。出貨機器不帶任何 registry 憑證，更新程式也從不登入 registry，三份
映像以 digest 匿名拉取；完整性由簽章版本清單裡的 digest 保證，能不能用由授權決定。
`ghcr.io/laysi/laysi-one-api`、`-web`、`-backup` 三個套件的可見度要由人在 GitHub 上改成
公開，**在第一台出貨機器之前完成**；沒有公開時，更新停在第 3 步並回 `failed-unchanged`，
`install` 也在拉 api 映像時停下。

`docker-compose.ship.yml` 是出貨機器的 overlay：自家映像以
`${LAYSI_REGISTRY:?}/laysi-one-*@${LAYSI_*_DIGEST:?}` 引用（`.env` 缺值就拒絕啟動）、拿掉
postgres 對 `secrets` 的依賴、釘住容器名稱、api 掛兩個共用目錄而 mcp 只掛結果目錄、
`in_pod: false`。第三方映像在
`docker-compose.prod.yml` 裡就以 digest 釘住，one.laysi.io 與示範站沿用同一份。postgres
釘的是 `postgres:18` 的 trixie 版（ICU 76）：換 Debian 基底就換 ICU，在舊 ICU 建立的資料
目錄每次連線都警告 `collation version mismatch`，zh-TW 定序的文字索引要重建。

**換 rustfs 或 postgres 的 digest 要先過 `scripts/third-party-image-check.ts`**（`ci.yml`
的 `third-party-images`）：RustFS 的物件不在回退範圍內，所以新舊兩版要讀得了同一份資料，
舊→新→舊；postgres 另外比對兩版的 ICU，不同時那次變更要帶一份執行 `REINDEX` 與
`ALTER DATABASE … REFRESH COLLATION VERSION` 的 migration，否則不通過。

### 資料庫還原之後的 RustFS

回退只還原資料庫，RustFS 的物件維持回退當下的樣子：

- **文件版本不會指向不存在的物件**：物件先寫進桶子、成功才寫資料列，而文件從不刪除；
  還原後的資料庫只會少列，桶子裡多出沒有列指著的物件。
- **暫存上傳可能指向已刪除的暫存物件**：歸檔成功後會刪暫存物件（`documents.rs`
  `file_staged`），清除工作也會刪過期的暫存物件（`uploads.rs`）。還原後那一列又變回等待
  歸檔，物件卻不在了。歸檔它時比照過期處理：刪掉那一列，請使用者重新上傳。

### 不包含

逾期自動更新（EPIC #59 決定：只有管理員按下更新才執行）、RustFS 物件的備份與還原、
PostgreSQL 大版本升級。實機才看得到的部分（`$XDG_RUNTIME_DIR`、bind mount 的 SELinux
標籤、開機順序）由 #64 的演練涵蓋。

## 異地備份

備份的目的地是設定，不是部署佈線——這個產品沒有任何一項需要人去主機上編檔案。
`backup_setting` 是單列表，形狀與 `oidc_setting` 相同：`secret_key` 進得去、出不來，
稽核紀錄依欄位名遮蔽（`REDACTED_FIELDS`）。畫面在「安全與營運」，操作是
`get_backup_setting`／`update_backup_setting`／`confirm_backup_key`／
`reset_backup_key`，四個都是 `AdminOnly`。

**金鑰是非對稱的**。備份檔裡有完整身分證字號，放進別人家的桶子一定要加密；而對稱金鑰
必須留給加密的那一方讀得到，也就是存進資料庫——但資料庫本身就在那份加密檔裡，還原時
需要的鑰匙只存在於它要打開的東西裡面。`age_recipient` 是公鑰，備份工作只需要它。

**私鑰由備份容器產生，不由伺服器產生**。容器是持有 `age` 的那一方，也就是唯一不會把
金鑰格式寫錯的那一方——格式錯了要到有人嘗試還原的那一天才會發現。私鑰落在
`age_identity_pending`，畫面顯示一次；`confirm_backup_key` 是管理員說「已經抄下來了」，
而**在那之前備份不執行**，所以沒有任何一份備份含有開啟它的鑰匙。

**抄丟了要換得掉**。金鑰只在 `age_recipient` 為空的那一次產生，確認之後私鑰
就永久不存在，所以少了這條路，抄丟的安裝會繼續每天備份到誰也打不開的檔案，而畫面上
沒有任何動作改得了它。`reset_backup_key` 把兩半一起清空，下一次備份重新產生一組並再
顯示一次。它停在這裡：**已經寫出去的備份檔仍然只能以原金鑰開啟**，而要不要刪掉它們不
是這個操作決定得了的事，畫面上的確認視窗照實說這一句。

伺服器因此不必引入 `age` crate：那會為了三行金鑰產生多帶進約六十個 crate（含 fluent
i18n 與已被標記未來會被 Rust 拒絕的 `proc-macro-error2`）。

**順序是先資料庫再文件**。物件是先配鍵、位元組進桶子成功才寫資料庫那一列，所以桶子
只增不改。先 dump 的話，備份檔可能指到還沒複製過去的物件，下一輪補上；反過來則會留下
一列指向任何副本都沒有的物件。

**文件也加密，而且只加密一次**。掃描的合約不比 dump 不敏感。物件寫進去之後永不改變，
所以「遠端已經有了」就等於「已經做過了」——即使 `age` 每次輸出都不同，整個流程仍是
增量的。保留天數只套用在 dump 上：物件是資料庫仍然指著的那份文件的唯一副本。

`backup` 容器同時持有兩組憑證（本地物件儲存讀、遠端目的地寫），兩組都不 export——
export 其中一組會被另一組的指令默默拿去用。


## 沒有 `.env` 的部署（compose 自己產生密鑰）

`docker compose up` 需要的值全部有預設，唯二必須每個安裝不同的——資料庫密碼與初始化
代碼——由 `secrets` 服務在第一次啟動時產生，寫進 `secrets` 資料卷。

**為什麼是檔案而不是環境變數**：compose 在任何容器啟動之前就完成插值，所以它插得進去
的值只能是某個人事先寫好的。反過來，一個一次性的服務可以把值寫進資料卷，其餘服務讀檔
——postgres 本身就支援 `POSTGRES_PASSWORD_FILE`，而 `laysi_store::env_or_file` 讓伺服器
的任何設定都收 `NAME_FILE`。**用 entrypoint 包一層也做得到，但那樣
`compose exec api laysi-admin …` 就拿不到連線字串**，而那是離線逃生門。

**產生過就不再產生**（`[ -s ]` 判斷）。換掉密碼而 `pgdata` 還在，留下的是一個誰也打
不開的資料庫。檔案模式 0444：api 以 uid 10001 執行，root 寫的 0600 它讀不到。

**`RUSTFS_SECRET_KEY` 是唯一的例外**，它由 RustFS 在啟動時從環境讀取、沒有檔案形式，
而覆寫別人映像的 entrypoint 會在下一次改版時壞掉。那個桶子關著靠的不是這組密鑰，而是
9000 與 9001 都不發布。要換就在環境裡覆寫它。

**`pgdata` 掛在 `/var/lib/postgresql`，不是 `/var/lib/postgresql/data`**：PostgreSQL 18
的官方映像把 `PGDATA` 移到 `/var/lib/postgresql/18/docker`，資料卷宣告也跟著上移一層。
**PostgreSQL 17 留下的資料卷不能直接換上 18 的映像**——資料目錄格式逐個大版本不同，
起不來的伺服器不會自己升級。既有安裝要先在 17 上 `pg_dump`，換映像、清掉資料卷，再
`psql` 灌回去；備份容器的 `postgresql18-client` 就是為此而升的（`pg_dump` 的版本不得
低於伺服器）。


## 政策值搬離環境變數

三項進 `company_setting`：`session_idle_minutes`、`session_max_hours`、
`mcp_rate_limit_per_minute`。**不另建表**——那張表已經有 label、note、`editable`、
`AdminOnly` 的寫入路徑與畫面，而基礎設定的欄位是泛型渲染的，所以前端一行都不用改。

它們同時被沒有 `Store` 可用的路徑讀到（登入寫出的 cookie、每次請求的閒置判定、`/mcp`
的計數器），所以走 `business_offset` 那條路：啟動時 `load_policy_settings` 讀進行程內的
atomic，`update_company_setting` 寫完重新發布。`RateLimiter` 因此分成 `Fixed` 與 `Live`
兩種額度來源——**設定改了要在下一次呼叫生效，不是下一次重啟**，否則畫面上那個數字是假的。
逾時填 0 會被寫入端擋下：那等於所有人下一次點擊就登出，而且沒有路可以回來改。

**`SESSION_COOKIE_SECURE` 整個刪掉，改由請求推導**。判準是 `X-Forwarded-Proto` 是否為
`https`（多層 proxy 會串接，取第一段，那是客戶端自己的協定）。兩條都不成立：

- 留成旗標，就是要人記得在搬到 HTTPS 那天去翻它，而忘了翻的後果是 session cookie 以
  明文送出；
- 一律加上 `Secure`，`http://localhost` 因為是 secure context 仍然可用，但區域網路 IP
  上的純 HTTP 內部主機會登不進去，而那是中小企業的常見部署。

Caddyfile 因此加上 `trusted_proxies static private_ranges`：這個容器只講純 HTTP，不設
這一行的話它會用自己的協定覆蓋掉前面那層 TLS proxy 送來的標頭。限制在私有網段，所以
從公網來的標頭決定不了任何事。

**`MCP_ALLOWED_ORIGINS` 留在環境變數，因為它不是設定**：它守的是部署者自己決定要發布
的那個埠，與埠號同一層。

**伺服器不掛 CORS 層**：兩種部署下瀏覽器都看不到跨源請求——開發時 Vite 把 `/v1`、
`/mcp`、`/healthz` 代理到本機伺服器，compose 部署下 Caddy 送前端並代理同樣的路徑，
而 session cookie 兩邊都是 `SameSite=Strict`。原本的 `CORS_ALLOWED_ORIGIN` 預設
`http://localhost:5173` 又寫在程式與 compose 兩處，於是正式站每一個回應都帶著
`access-control-allow-origin: http://localhost:5173` 與 `allow-credentials: true`，
指著一個不存在的來源，而畫面上沒有地方改得動它。整層與那個環境變數一起移除。
