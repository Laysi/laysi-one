//! Structural master data: accounts, employees, bank accounts, dimension
//! values and approval rules.
//!
//! None of these are day-to-day transactions. They decide which accounts later
//! entries may use, which employee a login maps to, which GL account receipts
//! and payments hit, which values the department field may take, and how many
//! approvals a claim needs. Keeping them together makes the validation of
//! structural changes and the "off for AI by default" policy easy to review in
//! one place.
//!
//! Creating receivables and payables deliberately lives elsewhere: those write
//! a subsidiary ledger row and an unposted voucher together, so they belong in
//! `receivables.rs` / `payables.rs` alongside settlement and reconciliation.
//!
//! `report_group` and `cash_flow_section` decide where an account lands in the
//! three statements. Both must be sent explicitly; P&L accounts send `null`
//! for the cash flow section rather than omitting it. Rust returns a clear
//! error based on the 0006 CHECK, and the database is the final gate.
//!
//! Full national ID numbers are never stored, only masked values. The three
//! insured brackets are facts filed with the authorities and are kept as
//! entered, never validated or corrected against a ruleset. Creating an
//! employee does not create a `user_account`; `create_employee_login` does,
//! and only for the ordinary staff role: choosing anyone's role is a system
//! control and stays with the administrator.

use crate::ops::employees::{BASIS_HOURLY, BASIS_MONTHLY, INSURED_EMPLOYEE, INSURED_OWNER};
use crate::ops::settings::{check_new_login, external_login_resolves, write_login, NewLogin};
use crate::{money, parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::NaiveDate;
use laysi_domain::Money;
use laysi_store::{
    CreateBankAccount as StoreCreateBankAccount, CreateDimensionValue as StoreCreateDimensionValue,
    SetApprovalRule as StoreSetApprovalRule, UpdateDimensionValue as StoreUpdateDimensionValue,
    WriteAccount, WriteEmployee, WriteEmployeeBankAccount,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(CreateAccount),
        Box::new(UpdateAccount),
        Box::new(CreateEmployee),
        Box::new(UpdateEmployee),
        Box::new(CreateEmployeeLogin),
        Box::new(CreateBankAccount),
        Box::new(CreateDimensionValue),
        Box::new(UpdateDimensionValue),
        Box::new(SetApprovalRule),
    ]
}

fn default_true() -> bool {
    true
}

fn default_full_time() -> String {
    "full_time".into()
}

fn default_active_status() -> String {
    "active".into()
}

fn default_monthly_basis() -> String {
    BASIS_MONTHLY.into()
}

fn default_insured_employee() -> String {
    INSURED_EMPLOYEE.into()
}

// Accounts

#[derive(Deserialize, JsonSchema)]
pub struct AccountInput {
    /// Account code.
    pub code: String,
    /// Account name.
    pub name: String,
    /// One of `asset`, `liability`, `equity`, `revenue`, `expense`.
    pub kind: String,
    /// Normal balance side. Defaults to the convention for the kind; contra
    /// accounts can override it explicitly.
    #[serde(default)]
    pub normal_side: Option<String>,
    /// Whether entries may be posted directly to it. Parent accounts are
    /// forced to false.
    #[serde(default = "default_true")]
    pub is_postable: bool,
    /// Whether it may still be used by new entries. Disabling does not delete
    /// historical entries.
    #[serde(default = "default_true")]
    pub is_active: bool,
    /// Parent account code.
    #[serde(default)]
    pub parent_code: Option<String>,
    /// Display order.
    #[serde(default)]
    pub sort_order: i32,
    /// Balance sheet or income statement group. Required; it is never guessed
    /// from the name.
    pub report_group: String,
    /// Cash flow activity. P&L accounts must send null explicitly rather than
    /// omitting the field.
    #[serde(default)]
    pub cash_flow_section: Option<String>,
}

fn require_account_report_fields(input: &Value) -> Result<(), OpError> {
    let Some(object) = input.as_object() else {
        return Err(OpError::BadInput(
            "科目輸入必須是物件，並填報表分組與現金流量活動別。".into(),
        ));
    };
    if !object.contains_key("report_group") {
        return Err(OpError::BadInput(
            "報表分組必填，請指定科目在資產負債表或損益表的分段。".into(),
        ));
    }
    if !object.contains_key("cash_flow_section") {
        return Err(OpError::BadInput(
            "現金流量活動別必填；損益科目請留空。".into(),
        ));
    }
    Ok(())
}

fn account_schema() -> Value {
    let mut schema = schema_of::<AccountInput>();
    let required = schema
        .as_object_mut()
        .and_then(|object| object.get_mut("required"))
        .and_then(Value::as_array_mut)
        .expect("AccountInput schema 應有 required 陣列");
    let field = json!("cash_flow_section");
    if !required.contains(&field) {
        required.push(field);
    }
    schema
}

fn normal_side_for(kind: &str) -> Result<&'static str, OpError> {
    match kind {
        "asset" | "expense" => Ok("debit"),
        "liability" | "equity" | "revenue" => Ok("credit"),
        other => Err(OpError::BadInput(format!(
            "kind 只能是 asset、liability、equity、revenue 或 expense，收到的是「{other}」"
        ))),
    }
}

fn validate_account(input: &AccountInput) -> Result<String, OpError> {
    if input.code.trim().is_empty() || input.name.trim().is_empty() {
        return Err(OpError::BadInput("科目代號與名稱都要填寫".into()));
    }
    if input
        .parent_code
        .as_deref()
        .is_some_and(|parent| parent.trim() == input.code.trim())
    {
        return Err(OpError::BadInput("父科目不能指向自己".into()));
    }

    let report_ok = match input.kind.as_str() {
        "asset" => matches!(
            input.report_group.as_str(),
            "current_asset" | "non_current_asset"
        ),
        "liability" => matches!(
            input.report_group.as_str(),
            "current_liability" | "non_current_liability"
        ),
        "equity" => input.report_group == "equity",
        "revenue" => matches!(input.report_group.as_str(), "revenue" | "other_income"),
        "expense" => matches!(
            input.report_group.as_str(),
            "cogs" | "operating_expense" | "other_expense"
        ),
        _ => {
            normal_side_for(&input.kind)?;
            false
        }
    };
    if !report_ok {
        return Err(OpError::BadInput(format!(
            "報表分組「{}」不適用科目類別「{}」，請改選該類別允許的分組。",
            input.report_group, input.kind
        )));
    }

    let section = input.cash_flow_section.as_deref();
    let section_value_ok = section
        .is_none_or(|value| matches!(value, "cash" | "operating" | "investing" | "financing"));
    let section_kind_ok = match input.kind.as_str() {
        "asset" => section.is_some(),
        "liability" | "equity" => section.is_some() && section != Some("cash"),
        "revenue" | "expense" => section.is_none(),
        _ => false,
    };
    if !section_value_ok || !section_kind_ok {
        return Err(OpError::BadInput(format!(
            "現金流量活動別不適用科目類別「{}」：資產、負債、權益要指定活動別，只有資產可用 cash；收益與費用請留空。",
            input.kind
        )));
    }

    match input.normal_side.as_deref() {
        Some("debit") => Ok("debit".into()),
        Some("credit") => Ok("credit".into()),
        Some(other) => Err(OpError::BadInput(format!(
            "normal_side 只能是 debit 或 credit，收到的是「{other}」"
        ))),
        None => Ok(normal_side_for(&input.kind)?.into()),
    }
}

fn account_write<'a>(input: &'a AccountInput, normal_side: &'a str) -> WriteAccount<'a> {
    WriteAccount {
        code: input.code.trim(),
        name: input.name.trim(),
        kind: input.kind.as_str(),
        normal_side,
        is_postable: input.is_postable,
        is_active: input.is_active,
        parent_code: input
            .parent_code
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        sort_order: input.sort_order,
        report_group: input.report_group.as_str(),
        cash_flow_section: input.cash_flow_section.as_deref(),
    }
}

struct CreateAccount;

#[async_trait]
impl Operation for CreateAccount {
    fn name(&self) -> &'static str {
        "create_account"
    }
    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "建立會計科目。報表分組與現金流量活動別為必填，決定科目出現在資產負債表、損益表與現金流量表的哪一段；借貸方向依類別帶入慣例值，備抵科目可覆寫。"
    }
    fn input_schema(&self) -> Value {
        account_schema()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        require_account_report_fields(&input)?;
        let input: AccountInput = parse(input)?;
        let normal_side = validate_account(&input)?;
        let account = ctx
            .store
            .create_account(account_write(&input, &normal_side))
            .await?;
        let summary = format!(
            "建立科目 {} {} · 報表分組 {} · 現金流量活動別 {}",
            account.code,
            account.name,
            account.report_group,
            account
                .cash_flow_section
                .as_deref()
                .unwrap_or("併入本期損益")
        );
        Ok(OpOutcome::new(json!({ "account": account }), summary))
    }
}

struct UpdateAccount;

#[async_trait]
impl Operation for UpdateAccount {
    fn name(&self) -> &'static str {
        "update_account"
    }
    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "完整更新或停用會計科目。報表分組與現金流量活動別仍必填；停用只阻止新分錄，歷史帳保留。"
    }
    fn input_schema(&self) -> Value {
        account_schema()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        require_account_report_fields(&input)?;
        let input: AccountInput = parse(input)?;
        let normal_side = validate_account(&input)?;
        let account = ctx
            .store
            .update_account(account_write(&input, &normal_side))
            .await?;
        let summary = format!(
            "更新科目 {} {} · {}",
            account.code,
            account.name,
            if account.is_active {
                "啟用"
            } else {
                "已停用，歷史分錄保留"
            }
        );
        Ok(OpOutcome::new(json!({ "account": account }), summary))
    }
}

// Employees

#[derive(Deserialize, JsonSchema)]
pub struct EmployeeInput {
    /// Employee number, e.g. `E001`. Leave blank when creating and the next
    /// `E###` is assigned; required when updating, because it is the key.
    #[serde(default)]
    pub emp_no: String,
    /// Name.
    pub name: String,
    /// Department dimension value code; only the department dimension is
    /// searched.
    #[serde(default)]
    pub dept_code: Option<String>,
    /// Job title.
    #[serde(default)]
    pub title: String,
    /// One of `full_time`, `part_time`, `contract`.
    #[serde(default = "default_full_time")]
    pub employment_type: String,
    /// `monthly` or `hourly`. It decides what `base_salary` means: a monthly
    /// salary prorated by calendar day, or an hourly rate paid on the hours
    /// the punches record. A separate axis from `employment_type`, because a
    /// part timer may be on a monthly salary.
    #[serde(default = "default_monthly_basis")]
    pub pay_basis: String,
    /// `employee` or `owner`. An owner bears the health insurance premium in
    /// full and has no pension contributed for them.
    #[serde(default = "default_insured_employee")]
    pub insured_as: String,
    /// Date of birth. Overwritten on every save; omit it to clear it.
    #[serde(default)]
    pub birthday: Option<NaiveDate>,
    /// Hire date.
    pub hired_on: NaiveDate,
    /// Planned or actual leaving date. Required when status is leaving or left.
    #[serde(default)]
    pub left_on: Option<NaiveDate>,
    /// One of `active`, `leaving`, `left`.
    #[serde(default = "default_active_status")]
    pub status: String,
    /// Base salary. Amounts are strings in JSON. Three states on update: omit
    /// to leave it alone, send `"0"` to clear it back to not-agreed-yet, send
    /// a number to change it. On create, omitting it means not agreed yet and
    /// payroll refuses this employee by name instead of paying nothing.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    #[schemars(with = "Option<String>")]
    pub base_salary: Option<Money>,
    /// Labour insurance salary as filed with the authorities; kept as given,
    /// never corrected against a ruleset. Three states, as `base_salary`.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    #[schemars(with = "Option<String>")]
    pub labor_insured_salary: Option<Money>,
    /// Health insurance salary as filed with the authorities; kept as given,
    /// never corrected against a ruleset. Three states, as `base_salary`.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    #[schemars(with = "Option<String>")]
    pub health_insured_salary: Option<Money>,
    /// Pension contribution wage as filed with the authorities; kept as given,
    /// never corrected against a ruleset. Three states, as `base_salary`.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    #[schemars(with = "Option<String>")]
    pub pension_wage: Option<Money>,
    /// National ID, one letter followed by nine digits, checksum included.
    /// May be blank while it has not been collected.
    #[serde(default)]
    pub id_no: String,
    /// Every bank account kept for this person. Omit to leave the accounts on
    /// file alone; send the whole list to replace them.
    #[serde(default)]
    pub bank_accounts: Option<Vec<EmployeeBankAccountInput>>,
    /// Notes.
    #[serde(default)]
    pub note: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct EmployeeBankAccountInput {
    /// Bank and branch.
    #[serde(default)]
    pub bank_name: String,
    /// Account number.
    pub account_no: String,
    /// Whether the monthly payroll transfer goes here. At most one account per
    /// person may be marked; when none is, the first one in the list is used.
    #[serde(default)]
    pub is_payroll: bool,
    /// What this account is for.
    #[serde(default)]
    pub note: String,
}

/// Positional value of the leading letter of a national ID.
///
/// Not a regulation value in the sense of `backend/regulation/`: this is the
/// check digit algorithm published with the identifier format, not a rate,
/// bracket or ceiling that changes with the law.
fn id_letter_code(letter: char) -> Option<u32> {
    const ORDER: [char; 26] = [
        'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J', 'K', 'L', 'M', 'N', 'P', 'Q', 'R', 'S', 'T',
        'U', 'V', 'X', 'Y', 'W', 'Z', 'I', 'O',
    ];
    ORDER
        .iter()
        .position(|c| *c == letter)
        .map(|i| i as u32 + 10)
}

/// Whether a national ID is well formed: one letter, nine digits, and a
/// trailing check digit that agrees with the rest.
///
/// A typo in this number reaches the labour insurance filing and the
/// withholding statement, where it is the tax authority that finds it. The
/// check digit catches most of them here instead.
fn is_valid_id_no(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(letter) = chars.next().map(|c| c.to_ascii_uppercase()) else {
        return false;
    };
    let digits: Vec<u32> = chars.filter_map(|c| c.to_digit(10)).collect();
    if value.chars().count() != 10 || digits.len() != 9 {
        return false;
    }
    let Some(code) = id_letter_code(letter) else {
        return false;
    };
    let mut sum = code / 10 + (code % 10) * 9;
    for (i, d) in digits.iter().take(8).enumerate() {
        sum += d * (8 - i as u32);
    }
    (sum + digits[8]) % 10 == 0
}

fn validate_employee(input: &EmployeeInput) -> Result<(), OpError> {
    if input.emp_no.trim().is_empty() || input.name.trim().is_empty() {
        return Err(OpError::BadInput("員工編號與姓名都要填寫".into()));
    }
    let id_no = input.id_no.trim();
    if !id_no.is_empty() && !is_valid_id_no(id_no) {
        return Err(OpError::BadInput(
            "身分證字號要一個英文字母加九碼數字，且檢查碼相符；還沒收到可以留空".into(),
        ));
    }
    if !matches!(
        input.employment_type.as_str(),
        "full_time" | "part_time" | "contract"
    ) {
        return Err(OpError::BadInput("僱用型態只能是正職、兼職或約聘".into()));
    }
    if !matches!(input.pay_basis.as_str(), BASIS_MONTHLY | BASIS_HOURLY) {
        return Err(OpError::BadInput("計薪基準只能是月薪或時薪".into()));
    }
    if !matches!(input.insured_as.as_str(), INSURED_EMPLOYEE | INSURED_OWNER) {
        return Err(OpError::BadInput("投保身分只能是受僱者或負責人".into()));
    }
    if !matches!(input.status.as_str(), "active" | "leaving" | "left") {
        return Err(OpError::BadInput("狀態只能是在職、離職中或已離職".into()));
    }
    if matches!(input.status.as_str(), "leaving" | "left") && input.left_on.is_none() {
        let label = if input.status == "leaving" {
            "待離職"
        } else {
            "已離職"
        };
        return Err(OpError::BadInput(format!(
            "狀態為{label}時必須填寫離職日。"
        )));
    }
    if input
        .left_on
        .is_some_and(|left_on| left_on < input.hired_on)
    {
        return Err(OpError::BadInput("離職日必須在到職日當天或之後".into()));
    }
    if input
        .birthday
        .is_some_and(|birthday| birthday >= input.hired_on)
    {
        return Err(OpError::BadInput("生日必須早於到職日".into()));
    }
    // Zero means the amount has not been agreed yet. Payroll refuses such an
    // employee rather than paying nothing, so the roster does not have to hold
    // a number before there is one.
    if input.base_salary.is_some_and(|value| value < Money::ZERO) {
        return Err(OpError::BadInput(
            "底薪不能是負數；尚未談定時留空即可".into(),
        ));
    }
    validate_bank_accounts(input)?;
    if [
        input.labor_insured_salary,
        input.health_insured_salary,
        input.pension_wage,
    ]
    .iter()
    .any(|value| value.is_some_and(|value| value < Money::ZERO))
    {
        return Err(OpError::BadInput(
            "三個投保申報級距不能是負數；級距照申報事實保存。".into(),
        ));
    }
    Ok(())
}

/// Reject a list the roster could not print back. An account with no number
/// is not an account, and two payroll accounts leave payday with no answer.
fn validate_bank_accounts(input: &EmployeeInput) -> Result<(), OpError> {
    let Some(accounts) = &input.bank_accounts else {
        return Ok(());
    };
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for account in accounts {
        let bank_name = account.bank_name.trim();
        let account_no = account.account_no.trim();
        if bank_name.is_empty() || account_no.is_empty() {
            return Err(OpError::BadInput(
                "銀行帳戶要填銀行名稱與帳號；不再使用的帳戶請整筆刪除".into(),
            ));
        }
        if seen.contains(&(bank_name, account_no)) {
            return Err(OpError::BadInput(format!(
                "{bank_name} {account_no} 重複登錄了兩次"
            )));
        }
        seen.push((bank_name, account_no));
    }
    if accounts.iter().filter(|account| account.is_payroll).count() > 1 {
        return Err(OpError::BadInput("薪轉帳戶只能指定一個".into()));
    }
    Ok(())
}

/// The accounts as the store writes them. Kept out of [`employee_write`]
/// because the borrowed slice has to outlive the call.
fn employee_accounts(input: &EmployeeInput) -> Option<Vec<WriteEmployeeBankAccount<'_>>> {
    let accounts = input.bank_accounts.as_ref()?;
    // A single account nobody marked is still where the salary goes; asking
    // for the flag would only be asking the caller to repeat itself.
    let none_marked = !accounts.iter().any(|account| account.is_payroll);
    Some(
        accounts
            .iter()
            .enumerate()
            .map(|(index, account)| WriteEmployeeBankAccount {
                bank_name: account.bank_name.trim(),
                account_no: account.account_no.trim(),
                is_payroll: account.is_payroll || (none_marked && index == 0),
                note: account.note.trim(),
            })
            .collect(),
    )
}

fn employee_write<'a>(
    input: &'a EmployeeInput,
    actor: &'a str,
    bank_accounts: Option<&'a [WriteEmployeeBankAccount<'a>]>,
) -> WriteEmployee<'a> {
    WriteEmployee {
        emp_no: input.emp_no.trim(),
        name: input.name.trim(),
        dept_code: input
            .dept_code
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        title: input.title.trim(),
        employment_type: input.employment_type.as_str(),
        pay_basis: input.pay_basis.as_str(),
        insured_as: input.insured_as.as_str(),
        birthday: input.birthday,
        hired_on: input.hired_on,
        left_on: input.left_on,
        status: input.status.as_str(),
        base_salary: input.base_salary,
        labor_insured_salary: input.labor_insured_salary,
        health_insured_salary: input.health_insured_salary,
        pension_wage: input.pension_wage,
        id_no: input.id_no.trim(),
        bank_accounts,
        note: input.note.trim(),
        actor,
        keep_active_role: crate::ROLE_ADMIN,
    }
}

struct CreateEmployee;

#[async_trait]
impl Operation for CreateEmployee {
    fn name(&self) -> &'static str {
        "create_employee"
    }
    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "建立員工主檔。員工編號留空會自動配號；身分證字號會驗檢查碼，勞保申報與扣繳憑單都要用到；三個投保級距是向主管機關申報的事實，照輸入保存。登入帳號另行核發。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<EmployeeInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let mut input: EmployeeInput = parse(input)?;
        // Blank means "give me the next one", the same as `create_user_account`.
        // Two screens asking for the same field and only one of them numbering
        // it is a difference the user has to learn for no reason.
        if input.emp_no.trim().is_empty() {
            input.emp_no = ctx.store.next_employee_no().await?;
        }
        validate_employee(&input)?;
        let accounts = employee_accounts(&input);
        let employee = ctx
            .store
            .create_employee(employee_write(&input, &ctx.actor, accounts.as_deref()))
            .await?;
        let summary = format!(
            "建立員工 {} {} · 未建立登入帳號",
            employee.emp_no, employee.name
        );
        Ok(OpOutcome::new(
            json!({
                "employee": employee,
                "login_account_created": false,
                "login_note": "登入帳號請在名冊上核發。",
            }),
            summary,
        ))
    }
}

struct UpdateEmployee;

#[async_trait]
impl Operation for UpdateEmployee {
    fn name(&self) -> &'static str {
        "update_employee"
    }
    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "更新員工主檔。本薪與三項投保級距沒帶就不變更，帶 0 表示尚未設定；身分證字號會驗檢查碼；狀態為待離職或已離職時必須填離職日。狀態改為已離職會在同一個交易停用該員工的登入帳號、結束登入工作階段並撤銷 MCP Token，不等離職日；唯一啟用中的系統管理員不能這樣關掉。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<EmployeeInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let input: EmployeeInput = parse(input)?;
        validate_employee(&input)?;
        let accounts = employee_accounts(&input);
        let updated = ctx
            .store
            .update_employee(employee_write(&input, &ctx.actor, accounts.as_deref()))
            .await?;
        let employee = updated.employee;
        let closed = updated.closed;
        let mut closed_note = String::new();
        if !closed.logins.is_empty() {
            closed_note.push_str(&format!(" · 停用登入帳號 {}", closed.logins.join("、")));
        }
        if closed.tokens > 0 {
            closed_note.push_str(&format!(" · 撤銷 {} 組 MCP Token", closed.tokens));
        }
        let summary = format!(
            "更新員工 {} {} · 狀態 {}{}",
            employee.emp_no,
            employee.name,
            crate::ops::employees::status_label(&employee.status),
            if closed_note.is_empty() {
                " · 登入帳號未變更".to_string()
            } else {
                closed_note
            }
        );
        Ok(OpOutcome::new(
            json!({
                "employee": employee,
                // What ending the employment shut off, so the screen can say it
                // rather than only reporting a saved record.
                "closed_access": closed,
            }),
            summary,
        ))
    }
}

struct CreateEmployeeLogin;

#[derive(Deserialize, JsonSchema)]
pub struct CreateEmployeeLoginInput {
    /// Employee number from the roster. The person must already be on it.
    pub emp_no: String,
    /// Work address. It is also the login name.
    pub email: String,
}

#[async_trait]
impl Operation for CreateEmployeeLogin {
    fn name(&self) -> &'static str {
        "create_employee_login"
    }
    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "為名冊上的人核發一般職員登入帳號，用來打卡、請假與請款。密碼由系統產生，僅在本次回應出現一次，本人首次登入必須變更；地址落在外部登入的網域內時改為免密碼。角色固定為一般職員，其餘角色請由系統管理員在「帳號管理」建立。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<CreateEmployeeLoginInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: CreateEmployeeLoginInput = parse(input)?;
        let employee = ctx.store.employee(i.emp_no.trim()).await?;
        if employee.status == "left" {
            return Err(OpError::BadInput(format!(
                "{} {} 已離職，不核發登入帳號",
                employee.emp_no, employee.name
            )));
        }
        // `has_login` is the same field the roster draws this button from, so
        // the screen and the refusal cannot disagree.
        if employee.has_login {
            return Err(OpError::BadInput(format!(
                "{} {} 已經有登入帳號，要改地址或停用請洽系統管理員",
                employee.emp_no, employee.name
            )));
        }

        let email = i.email.trim().to_ascii_lowercase();
        // Which way in this account gets is not HR's decision: with the
        // provider set up and the address inside its domain, a password would
        // be a second credential nobody uses.
        let login = NewLogin {
            email: &email,
            display_name: employee.name.trim(),
            role_code: crate::ROLE_STAFF,
            employee_id: employee.id,
            external_login_only: external_login_resolves(ctx, &email).await?,
        };
        let role_name = check_new_login(ctx, &login).await?;
        let issued = write_login(ctx, &login, role_name).await?;

        let summary = format!(
            "核發登入帳號 · {} {}（{}）{}",
            employee.emp_no,
            employee.name,
            issued.user.username,
            if issued.temporary_password.is_some() {
                "，臨時密碼僅顯示一次"
            } else {
                "，外部登入"
            }
        );
        Ok(OpOutcome::new(
            json!({
                "employee_no": employee.emp_no,
                "employee_name": employee.name,
                "username": issued.user.username,
                "role_name": issued.role_name,
                // Shown once and never retrievable: only the hash is stored.
                "temporary_password": issued.temporary_password,
                "must_change_password": issued.temporary_password.is_some(),
            }),
            summary,
        ))
    }
}

// Bank accounts and dimension values

#[derive(Deserialize, JsonSchema)]
pub struct CreateBankAccountInput {
    /// Account name.
    pub name: String,
    /// Bank account number.
    pub account_no: String,
    /// Required GL account; must be an active, postable asset account.
    pub gl_code: String,
    /// Display order.
    #[serde(default)]
    pub sort_order: i32,
}

fn validate_bank_account(input: &CreateBankAccountInput) -> Result<(), OpError> {
    if input.name.trim().is_empty() || input.account_no.trim().is_empty() {
        return Err(OpError::BadInput("銀行帳戶名稱與帳號都要填寫".into()));
    }
    if input.gl_code.trim().is_empty() {
        return Err(OpError::BadInput(
            "綁定的總帳科目必填：銀行收付款與核銷要指定入帳科目".into(),
        ));
    }
    Ok(())
}

struct CreateBankAccount;

#[async_trait]
impl Operation for CreateBankAccount {
    fn name(&self) -> &'static str {
        "create_bank_account"
    }
    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "建立銀行帳戶，必須綁定一個啟用中的可入帳資產科目；未綁定總帳科目時，銀行收付款與核銷無法入帳。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<CreateBankAccountInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let input: CreateBankAccountInput = parse(input)?;
        validate_bank_account(&input)?;
        let account = ctx
            .store
            .create_bank_account(StoreCreateBankAccount {
                name: input.name.trim(),
                account_no: input.account_no.trim(),
                gl_code: input.gl_code.trim(),
                sort_order: input.sort_order,
            })
            .await?;
        let summary = format!(
            "建立銀行帳戶 #{} {} · 綁定總帳科目 {}",
            account.id, account.name, account.gl_code
        );
        Ok(OpOutcome::new(json!({ "bank_account": account }), summary))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateDimensionValueInput {
    /// Existing dimension code; this operation does not create dimensions.
    pub dimension_code: String,
    /// Dimension value code. Empty means the display name; it is only a lookup
    /// key for `dept_code` and every screen picks values by label.
    #[serde(default)]
    pub code: String,
    /// Display name.
    pub label: String,
    /// Whether it is active.
    #[serde(default = "default_true")]
    pub active: bool,
    /// Display order.
    #[serde(default)]
    pub sort_order: i32,
}

fn validate_dimension_value(input: &CreateDimensionValueInput) -> Result<(), OpError> {
    if input.dimension_code.trim().is_empty() || input.label.trim().is_empty() {
        return Err(OpError::BadInput("類別與名稱都要填寫".into()));
    }
    Ok(())
}

struct CreateDimensionValue;

#[async_trait]
impl Operation for CreateDimensionValue {
    fn name(&self) -> &'static str {
        "create_dimension_value"
    }
    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "在既有分類下新增一個項目。部門分類的項目可用於員工與分錄的部門欄位。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<CreateDimensionValueInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let input: CreateDimensionValueInput = parse(input)?;
        validate_dimension_value(&input)?;
        let code = if input.code.trim().is_empty() {
            input.label.trim()
        } else {
            input.code.trim()
        };
        let value = ctx
            .store
            .create_dimension_value(StoreCreateDimensionValue {
                dimension_code: input.dimension_code.trim(),
                code,
                label: input.label.trim(),
                active: input.active,
                sort_order: input.sort_order,
            })
            .await?;
        let summary = format!("新增分類項目 {}", value.label);
        Ok(OpOutcome::new(json!({ "value": value }), summary))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateDimensionValueInput {
    /// Existing dimension code.
    pub dimension_code: String,
    /// Code of the value to update.
    pub code: String,
    /// New display name. Omit to leave it.
    #[serde(default)]
    pub label: Option<String>,
    /// Whether it stays selectable. Omit to leave it.
    #[serde(default)]
    pub active: Option<bool>,
}

fn validate_update_dimension_value(input: &UpdateDimensionValueInput) -> Result<(), OpError> {
    if input.dimension_code.trim().is_empty() || input.code.trim().is_empty() {
        return Err(OpError::BadInput("類別與項目都要指定".into()));
    }
    if input.label.as_ref().is_some_and(|l| l.trim().is_empty()) {
        return Err(OpError::BadInput("名稱不能留空".into()));
    }
    if input.label.is_none() && input.active.is_none() {
        return Err(OpError::BadInput("沒有變更".into()));
    }
    Ok(())
}

struct UpdateDimensionValue;

#[async_trait]
impl Operation for UpdateDimensionValue {
    fn name(&self) -> &'static str {
        "update_dimension_value"
    }
    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "改分類項目的名稱，或停用項目。停用後不再出現在選單，已經記在單據與員工上的不受影響。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<UpdateDimensionValueInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let input: UpdateDimensionValueInput = parse(input)?;
        validate_update_dimension_value(&input)?;
        let value = ctx
            .store
            .update_dimension_value(StoreUpdateDimensionValue {
                dimension_code: input.dimension_code.trim(),
                code: input.code.trim(),
                label: input.label.as_deref().map(str::trim),
                active: input.active,
            })
            .await?;
        let summary = match input.active {
            Some(false) => format!("停用分類項目 {}", value.label),
            Some(true) => format!("啟用分類項目 {}", value.label),
            None => format!("分類項目改名為 {}", value.label),
        };
        Ok(OpOutcome::new(json!({ "value": value }), summary))
    }
}

// Approval rules

#[derive(Deserialize, JsonSchema)]
pub struct SetApprovalRuleInput {
    /// Step number, also the upsert key.
    pub step_no: i32,
    /// Responsible role. null is allowed for a step the claimant handles.
    #[serde(default)]
    pub role_code: Option<String>,
    /// Step name. May be omitted when deleting.
    #[serde(default)]
    pub label: Option<String>,
    /// Description of the condition.
    #[serde(default)]
    pub condition_note: String,
    /// Company policy amount threshold; null means always required. Amounts
    /// are strings in JSON.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    #[schemars(with = "Option<String>")]
    pub min_amount: Option<Money>,
    /// true deletes this step; a flow always keeps at least one.
    #[serde(default)]
    pub delete: bool,
}

fn validate_approval_rule(input: &SetApprovalRuleInput) -> Result<(), OpError> {
    if input.step_no <= 0 {
        return Err(OpError::BadInput("簽核關卡的順序要大於零".into()));
    }
    if input.min_amount.is_some_and(|amount| amount < Money::ZERO) {
        return Err(OpError::BadInput(
            "金額門檻不能是負數；不填代表一律要經過這一關".into(),
        ));
    }
    if !input.delete
        && input
            .label
            .as_deref()
            .is_none_or(|label| label.trim().is_empty())
    {
        return Err(OpError::BadInput(
            "關卡要有名稱，簽核流程上才分得出是哪一關".into(),
        ));
    }
    Ok(())
}

struct SetApprovalRule;

#[async_trait]
impl Operation for SetApprovalRule {
    fn name(&self) -> &'static str {
        "set_approval_rule"
    }
    fn risk(&self) -> Risk {
        Risk::BossOnly
    }
    fn summary(&self) -> &'static str {
        "新增、覆寫或刪除一關費用簽核規則；相同關卡編號會覆蓋，未設金額門檻代表一律需要。金額門檻可隨時調整；整套流程至少保留一關。"
    }
    fn input_schema(&self) -> Value {
        schema_of::<SetApprovalRuleInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let input: SetApprovalRuleInput = parse(input)?;
        validate_approval_rule(&input)?;

        if input.delete {
            ctx.store.delete_approval_rule(input.step_no).await?;
            let rules = ctx.store.approval_rules().await?;
            return Ok(OpOutcome::new(
                json!({ "deleted_step_no": input.step_no, "steps": rules }),
                format!(
                    "刪除簽核流程第 {} 關 · 尚餘 {} 關",
                    input.step_no,
                    rules.len()
                ),
            ));
        }

        let label = input.label.as_deref().unwrap_or_default().trim();
        let rule = ctx
            .store
            .set_approval_rule(StoreSetApprovalRule {
                step_no: input.step_no,
                role_code: input
                    .role_code
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty()),
                label,
                condition_note: input.condition_note.trim(),
                min_amount: input.min_amount,
            })
            .await?;
        let threshold = rule
            .min_amount
            .map(|amount| format!("{} 以上", money(amount)))
            .unwrap_or_else(|| "一律需要".into());
        let summary = format!(
            "設定簽核第 {} 關 {} · {threshold}",
            rule.step_no, rule.label
        );
        Ok(OpOutcome::new(json!({ "rule": rule }), summary))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn amount(value: &str) -> Money {
        Decimal::from_str(value).unwrap()
    }

    fn valid_account() -> AccountInput {
        AccountInput {
            code: "6119".into(),
            name: "測試費用".into(),
            kind: "expense".into(),
            normal_side: None,
            is_postable: true,
            is_active: true,
            parent_code: None,
            sort_order: 0,
            report_group: "operating_expense".into(),
            cash_flow_section: None,
        }
    }

    fn valid_employee() -> EmployeeInput {
        EmployeeInput {
            emp_no: "E001".into(),
            name: "測試員工".into(),
            dept_code: None,
            title: String::new(),
            employment_type: "full_time".into(),
            pay_basis: BASIS_MONTHLY.into(),
            insured_as: INSURED_EMPLOYEE.into(),
            birthday: None,
            hired_on: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
            left_on: None,
            status: "active".into(),
            base_salary: Some(amount("1")),
            labor_insured_salary: None,
            health_insured_salary: None,
            pension_wage: None,
            id_no: "A123456789".into(),
            bank_accounts: None,
            note: String::new(),
        }
    }

    #[test]
    fn create_account_rejects_missing_report_group_with_reason() {
        let error = require_account_report_fields(&json!({
            "code": "6119",
            "name": "測試費用",
            "kind": "expense",
            "cash_flow_section": null
        }))
        .unwrap_err();
        assert!(error.to_string().contains("報表分組"));
        assert!(error.to_string().contains("損益表"));
        assert_eq!(CreateAccount.risk(), Risk::NeedsConfirm);
    }

    #[test]
    fn update_account_rejects_reporting_group_that_conflicts_with_kind() {
        let mut input = valid_account();
        input.report_group = "current_asset".into();
        let error = validate_account(&input).unwrap_err();
        assert!(error.to_string().contains("不適用"));
        assert!(UpdateAccount.summary().contains("停用只阻止新分錄"));
    }

    /// The number reaches the labour insurance filing and the withholding
    /// statement, so a typo is found by the authorities rather than here.
    #[test]
    fn an_id_number_whose_check_digit_disagrees_is_refused() {
        let mut input = valid_employee();
        assert!(validate_employee(&input).is_ok());

        input.id_no = "A123456788".into();
        let error = validate_employee(&input).unwrap_err();
        assert!(error.to_string().contains("檢查碼"));

        // Masking it is no longer a way to store it: the masked form fails the
        // same check as any other malformed value.
        input.id_no = "A12*****89".into();
        assert!(validate_employee(&input).is_err());

        // Not collected yet is not an error.
        input.id_no = String::new();
        assert!(validate_employee(&input).is_ok());
        assert!(CreateEmployee.summary().contains("登入帳號另行核發"));
    }

    #[test]
    fn editing_a_job_title_alone_leaves_every_amount_untouched() {
        // The roster never returns the four amounts, so an edit drawer has
        // nothing to send back for them. Absent has to mean "leave as is",
        // or changing a job title would zero out the salary.
        let input: EmployeeInput = parse(json!({
            "emp_no": "E001",
            "name": "測試員工",
            "title": "資深工程師",
            "hired_on": "2026-07-01",
        }))
        .unwrap();
        assert_eq!(input.base_salary, None);
        assert_eq!(input.labor_insured_salary, None);
        assert_eq!(input.health_insured_salary, None);
        assert_eq!(input.pension_wage, None);

        // An explicit zero is a different request: clear it back to unset.
        let cleared: EmployeeInput = parse(json!({
            "emp_no": "E001",
            "name": "測試員工",
            "hired_on": "2026-07-01",
            "base_salary": "0",
        }))
        .unwrap();
        assert_eq!(cleared.base_salary, Some(Money::ZERO));
    }

    #[test]
    fn update_employee_requires_left_on_for_a_leaving_status() {
        let mut input = valid_employee();
        input.status = "leaving".into();
        let error = validate_employee(&input).unwrap_err();
        assert!(error.to_string().contains("離職日"));
        assert!(UpdateEmployee.summary().contains("離職日"));
    }

    fn account(bank_name: &str, account_no: &str, is_payroll: bool) -> EmployeeBankAccountInput {
        EmployeeBankAccountInput {
            bank_name: bank_name.into(),
            account_no: account_no.into(),
            is_payroll,
            note: String::new(),
        }
    }

    /// Payday has one destination. Two accounts claiming it is a question the
    /// screen cannot answer later, so it is refused at the point it is typed.
    #[test]
    fn only_one_account_can_be_the_payroll_account() {
        let mut input = valid_employee();
        input.bank_accounts = Some(vec![
            account("玉山銀行", "0012345", true),
            account("台北富邦", "6789012", true),
        ]);
        let error = validate_employee(&input).unwrap_err();
        assert!(error.to_string().contains("薪轉帳戶只能指定一個"));
    }

    /// One account nobody marked is where the salary goes: asking for the flag
    /// would be asking the caller to repeat what the list already says.
    #[test]
    fn an_unmarked_list_pays_into_its_first_account() {
        let mut input = valid_employee();
        input.bank_accounts = Some(vec![
            account("玉山銀行", "0012345", false),
            account("台北富邦", "6789012", false),
        ]);
        let written = employee_accounts(&input).unwrap();
        assert!(written[0].is_payroll);
        assert!(!written[1].is_payroll);
    }

    /// Omitting the list leaves the accounts on file alone, the same three
    /// states the four amounts use.
    #[test]
    fn omitting_the_list_leaves_the_accounts_on_file_alone() {
        assert!(employee_accounts(&valid_employee()).is_none());
    }

    #[test]
    fn an_account_needs_a_bank_and_a_number() {
        let mut input = valid_employee();
        input.bank_accounts = Some(vec![account("玉山銀行", "   ", false)]);
        let error = validate_employee(&input).unwrap_err();
        assert!(error.to_string().contains("銀行名稱與帳號"));
    }

    #[test]
    fn the_same_account_cannot_be_listed_twice() {
        let mut input = valid_employee();
        input.bank_accounts = Some(vec![
            account("玉山銀行", "0012345", true),
            account("玉山銀行", " 0012345 ", false),
        ]);
        let error = validate_employee(&input).unwrap_err();
        assert!(error.to_string().contains("重複登錄"));
    }

    #[test]
    fn create_bank_account_requires_a_general_ledger_code() {
        let input = CreateBankAccountInput {
            name: "營運戶".into(),
            account_no: "0001".into(),
            gl_code: String::new(),
            sort_order: 0,
        };
        assert!(validate_bank_account(&input)
            .unwrap_err()
            .to_string()
            .contains("總帳科目"));
        assert_eq!(CreateBankAccount.risk(), Risk::NeedsConfirm);
    }

    #[test]
    fn create_dimension_value_rejects_an_empty_dimension_code() {
        let input = CreateDimensionValueInput {
            dimension_code: String::new(),
            code: String::new(),
            label: "業務部".into(),
            active: true,
            sort_order: 0,
        };
        assert!(validate_dimension_value(&input).is_err());
        assert!(CreateDimensionValue
            .summary()
            .contains("在既有分類下新增一個項目"));
    }

    #[test]
    fn update_dimension_value_needs_something_to_change_and_a_non_empty_name() {
        let mut input = UpdateDimensionValueInput {
            dimension_code: "department".into(),
            code: "業務部".into(),
            label: None,
            active: None,
        };
        assert!(validate_update_dimension_value(&input)
            .unwrap_err()
            .to_string()
            .contains("沒有變更"));

        input.label = Some("  ".into());
        assert!(validate_update_dimension_value(&input).is_err());

        input.label = Some("業務一部".into());
        assert!(validate_update_dimension_value(&input).is_ok());

        input.label = None;
        input.active = Some(false);
        assert!(validate_update_dimension_value(&input).is_ok());
    }

    #[test]
    fn set_approval_rule_is_boss_only_and_preserves_null_as_always() {
        let input = SetApprovalRuleInput {
            step_no: 1,
            role_code: None,
            label: Some("申請人上傳".into()),
            condition_note: String::new(),
            min_amount: None,
            delete: false,
        };
        assert!(validate_approval_rule(&input).is_ok());
        assert_eq!(SetApprovalRule.risk(), Risk::BossOnly);
        assert!(SetApprovalRule.summary().contains("至少保留一關"));
        assert_eq!(
            laysi_store::StoreError::LastApprovalRule.to_string(),
            "簽核流程至少保留一關，不能刪除最後一關"
        );
    }

    #[test]
    fn normal_side_uses_kind_convention_but_allows_contra_accounts() {
        let input = valid_account();
        assert_eq!(validate_account(&input).unwrap(), "debit");
        let mut contra = input;
        contra.normal_side = Some("credit".into());
        assert_eq!(validate_account(&contra).unwrap(), "credit");
    }

    #[test]
    fn account_schema_requires_both_reporting_fields() {
        let schema = account_schema();
        let required = schema["required"].as_array().expect("schema 應有 required");
        assert!(required.contains(&json!("report_group")));
        assert!(required.contains(&json!("cash_flow_section")));
        let kinds = schema["properties"]["cash_flow_section"]["type"]
            .as_array()
            .expect("cash_flow_section 應允許字串或 null");
        assert!(kinds.contains(&json!("string")));
        assert!(kinds.contains(&json!("null")));
    }
}
