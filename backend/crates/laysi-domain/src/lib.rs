//! Pure domain model: money, double-entry journals, trial balance.
//!
//! This layer is deliberately synchronous, database-free and non-async. The
//! invariants of double-entry bookkeeping (debits equal credits, line count,
//! sign) have to be checkable in any context — a REST request, an MCP call,
//! a batch import, a unit test — not only when a database is reachable.
//!
//! The database enforces the same thing with a deferred constraint trigger
//! (see `backend/migrations/0001_initial.sql`). The duplication is not
//! waste: this layer produces messages a person can act on, and SQL holds
//! even when something bypasses the application.

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, Utc};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Hours east of UTC that the business runs on.
///
/// This is a single-country product and Taiwan has been UTC+8 all year round
/// with no daylight saving since 1979, so a fixed offset is exact here rather
/// than an approximation of a time zone database.
///
/// It matters more than it looks. Reading the server's UTC clock puts every
/// event between 00:00 and 08:00 local on the previous day: a voucher dated
/// yesterday, an arrival at 07:50 recorded against the day before. The same
/// offset is written into the CHECK on `attendance_punch`, so the database
/// agrees with this constant.
pub const BUSINESS_UTC_OFFSET_HOURS: i32 = 8;

/// The business time zone as a chrono offset.
pub fn business_offset() -> FixedOffset {
    FixedOffset::east_opt(BUSINESS_UTC_OFFSET_HOURS * 3600)
        .expect("BUSINESS_UTC_OFFSET_HOURS 必須在 chrono 接受的範圍內")
}

/// Wall-clock date and time of an instant, as a person here would read it.
pub fn business_time(at: DateTime<Utc>) -> NaiveDateTime {
    at.with_timezone(&business_offset()).naive_local()
}

/// Today's date in the business time zone.
pub fn business_today() -> NaiveDate {
    business_time(Utc::now()).date()
}

/// Money type.
///
/// Always [`rust_decimal::Decimal`], never `f64`: accounting requires a
/// year to add up exactly, which binary floating point cannot do. Always
/// serialised as a string in JSON (`"8420.00"`), with thousands separators
/// left to the frontend.
pub type Money = Decimal;

/// Debit or credit.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// Debit.
    Debit,
    /// Credit.
    Credit,
}

impl Side {
    /// Display label for the screen and MCP responses.
    pub fn label_zh(&self) -> &'static str {
        match self {
            Side::Debit => "借",
            Side::Credit => "貸",
        }
    }
}

/// Voucher status.
///
/// The four states describe situations rather than technical categories:
/// `draft` is drafted by AI awaiting review, `pending` is confirmed and
/// saved but not yet in the statements, `posted` is in the statements and
/// immutable, and `void` was withdrawn before posting. Correcting a posted
/// voucher always goes through a reversing entry, never a status rollback.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum VoucherStatus {
    /// Drafted by AI, awaiting confirmation.
    Draft,
    /// Confirmed and saved, awaiting posting.
    Pending,
    /// Posted.
    Posted,
    /// Voided.
    Void,
}

impl VoucherStatus {
    /// Display label used on screen.
    pub fn label_zh(&self) -> &'static str {
        match self {
            VoucherStatus::Draft => "待確認",
            VoucherStatus::Pending => "待過帳",
            VoucherStatus::Posted => "已過帳",
            VoucherStatus::Void => "已作廢",
        }
    }

    /// Whether it has reached the statements. A posted voucher may not be
    /// modified or deleted.
    pub fn is_immutable(&self) -> bool {
        matches!(self, VoucherStatus::Posted)
    }
}

/// Input for one journal line.
///
/// Debit and credit are separate fields rather than a direction plus an
/// amount, matching what the user sees on screen: a voucher has two
/// columns. The cost is having to forbid filling both explicitly, see
/// [`DomainError::BothSides`].
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
pub struct LineInput {
    /// Account code, matching `account.code`.
    pub account_code: String,
    /// Debit amount; `"0"` when unused. A string in JSON.
    #[serde(with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub debit: Money,
    /// Credit amount; `"0"` when unused. A string in JSON.
    #[serde(with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub credit: Money,
    /// Line memo, falling back to the voucher memo.
    #[serde(default)]
    pub memo: Option<String>,
    /// Which bank account this line moves, matching `bank_account.id`. The
    /// cash position is derived from this field.
    #[serde(default)]
    pub bank_account_id: Option<i32>,
    /// Which department carries this line, matching a department code in
    /// `dimension_value`. Department profit and loss is derived from this
    /// field, so a line left empty is reported as unattributed rather than
    /// spread over the others.
    #[serde(default)]
    pub dept_code: Option<String>,
}

/// Error raised when a journal breaks the rules of double entry.
///
/// Messages always state which line and by how much: the user needs
/// something they can act on, not "validation failed".
#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum DomainError {
    /// Fewer than two lines. Double entry needs at least one of each.
    #[error("傳票至少要有兩行分錄")]
    TooFewLines,

    /// One line fills both sides, so its direction is undefined.
    #[error("第 {line} 行同時填了借方與貸方")]
    BothSides { line: usize },

    /// The line is zero on both sides and means nothing.
    #[error("第 {line} 行借貸都是零")]
    EmptyLine { line: usize },

    /// Debits and credits differ. `diff` is the absolute difference, ready to
    /// read out to the user.
    #[error("借貸不相等，差 {diff} 元")]
    Unbalanced { diff: Money },

    /// Negative amount. To reverse an entry, use the other column; a negative
    /// would make the trial balance totals meaningless.
    #[error("金額不可為負（第 {line} 行）")]
    NegativeAmount { line: usize },
}

/// Validate a double-entry journal. This is the core invariant of the whole
/// system.
///
/// The order of the checks is fixed because only the first problem is
/// reported, and the order decides what the user sees first:
///
/// 1. Line count. If the shape is wrong, checking lines is pointless.
/// 2. Per line, in the order negative, both sides, both zero. Negative comes
///    first because input like `-500 / 300` also matches "both sides", while
///    the real problem is the minus sign.
/// 3. Totals. Only once every line is valid does the voucher's balance
///    matter.
///
/// Line numbers are 1-based, because the message is for a person, not for
/// an array index.
pub fn validate_entry(lines: &[LineInput]) -> Result<(), DomainError> {
    if lines.len() < 2 {
        return Err(DomainError::TooFewLines);
    }

    for (i, l) in lines.iter().enumerate() {
        let line = i + 1;
        if l.debit < Money::ZERO || l.credit < Money::ZERO {
            return Err(DomainError::NegativeAmount { line });
        }
        if !l.debit.is_zero() && !l.credit.is_zero() {
            return Err(DomainError::BothSides { line });
        }
        if l.debit.is_zero() && l.credit.is_zero() {
            return Err(DomainError::EmptyLine { line });
        }
    }

    let (debit, credit) = totals(lines);
    if debit != credit {
        return Err(DomainError::Unbalanced { diff: (debit - credit).abs() });
    }

    Ok(())
}

/// Debit total and credit total.
///
/// Performs no validation, so an unvalidated journal can be summed: the
/// entry screen shows the remaining difference live, and at that moment the
/// journal is not balanced yet.
pub fn totals(lines: &[LineInput]) -> (Money, Money) {
    lines.iter().fold((Money::ZERO, Money::ZERO), |(d, c), l| (d + l.debit, c + l.credit))
}

/// One row of the trial balance.
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct TrialBalanceRow {
    /// Account code.
    pub code: String,
    /// Account name.
    pub name: String,
    /// Total debit movement.
    #[serde(with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub debit: Money,
    /// Total credit movement.
    #[serde(with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub credit: Money,
    /// Balance, signed by the account's normal side. See
    /// [`TrialBalanceRow::new`].
    #[serde(with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub balance: Money,
}

impl TrialBalanceRow {
    /// Compute the balance according to the account's normal side.
    ///
    /// This conversion lives in the domain layer rather than in SQL or ops so
    /// that "assets are debit balances, liabilities are credit balances" has
    /// exactly one implementation. Liabilities are normally credit balances,
    /// hence `credit - debit`, and assets the reverse. A negative result
    /// means the account carries a reverse balance (an overdraft, say) and is
    /// reported as such rather than made absolute.
    pub fn new(
        code: impl Into<String>,
        name: impl Into<String>,
        debit: Money,
        credit: Money,
        normal_side: Side,
    ) -> Self {
        let balance = match normal_side {
            Side::Debit => debit - credit,
            Side::Credit => credit - debit,
        };
        Self { code: code.into(), name: name.into(), debit, credit, balance }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    /// One journal line, for tests.
    fn line(code: &str, debit: Money, credit: Money) -> LineInput {
        LineInput {
            account_code: code.to_string(),
            debit,
            credit,
            memo: None,
            bank_account_id: None,
            dept_code: None,
        }
    }

    #[test]
    fn rejects_fewer_than_two_lines() {
        assert_eq!(validate_entry(&[]).unwrap_err(), DomainError::TooFewLines);
        assert_eq!(
            validate_entry(&[line("6118", dec!(8420), dec!(0))]).unwrap_err(),
            DomainError::TooFewLines
        );
    }

    #[test]
    fn rejects_both_sides_on_one_line() {
        let lines = [line("6118", dec!(8420), dec!(8420)), line("1102", dec!(0), dec!(8420))];
        assert_eq!(validate_entry(&lines).unwrap_err(), DomainError::BothSides { line: 1 });
    }

    #[test]
    fn rejects_a_line_that_is_all_zero() {
        let lines = [
            line("6118", dec!(8420), dec!(0)),
            line("1102", dec!(0), dec!(8420)),
            line("1101", dec!(0), dec!(0)),
        ];
        assert_eq!(validate_entry(&lines).unwrap_err(), DomainError::EmptyLine { line: 3 });
    }

    #[test]
    fn rejects_negative_amounts() {
        let lines = [line("6118", dec!(-8420), dec!(0)), line("1102", dec!(0), dec!(-8420))];
        assert_eq!(validate_entry(&lines).unwrap_err(), DomainError::NegativeAmount { line: 1 });
    }

    /// When a negative and "both sides" both apply, report the negative: that
    /// is what the user actually mistyped.
    #[test]
    fn negative_wins_over_both_sides() {
        let lines = [line("6118", dec!(-500), dec!(300)), line("1102", dec!(0), dec!(200))];
        assert_eq!(validate_entry(&lines).unwrap_err(), DomainError::NegativeAmount { line: 1 });
    }

    /// The difference must be correct and absolute; the error message reads
    /// that number out to the user.
    #[test]
    fn reports_the_exact_imbalance() {
        let lines = [line("6118", dec!(8420), dec!(0)), line("1102", dec!(0), dec!(8400))];
        assert_eq!(validate_entry(&lines).unwrap_err(), DomainError::Unbalanced { diff: dec!(20) });

        let flipped = [line("6118", dec!(8400), dec!(0)), line("1102", dec!(0), dec!(8420))];
        assert_eq!(
            validate_entry(&flipped).unwrap_err(),
            DomainError::Unbalanced { diff: dec!(20) },
            "借小於貸時差額仍應為正 20，不是 -20"
        );
    }

    #[test]
    fn imbalance_message_names_the_amount() {
        let lines = [line("6118", dec!(8420), dec!(0)), line("1102", dec!(0), dec!(8400))];
        assert_eq!(validate_entry(&lines).unwrap_err().to_string(), "借貸不相等，差 20 元");
    }

    /// One debit against several credits: the shape of a payroll voucher
    /// (gross pay versus net plus deductions).
    #[test]
    fn accepts_a_balanced_multi_line_entry() {
        let lines = [
            line("6111", dec!(342653.00), dec!(0)),
            line("1102", dec!(0), dec!(338825.00)),
            line("2117", dec!(0), dec!(3828.00)),
        ];
        assert!(validate_entry(&lines).is_ok());
    }

    /// Different scales with equal values (`8420` versus `8420.00`) must
    /// count as balanced.
    #[test]
    fn scale_does_not_affect_balance() {
        let lines = [line("6118", dec!(8420), dec!(0)), line("1102", dec!(0), dec!(8420.00))];
        assert!(validate_entry(&lines).is_ok());
    }

    #[test]
    fn totals_sum_each_side_independently() {
        let lines = [
            line("6111", dec!(342653), dec!(0)),
            line("1102", dec!(0), dec!(338825)),
            line("2117", dec!(0), dec!(3828)),
        ];
        assert_eq!(totals(&lines), (dec!(342653), dec!(342653)));
    }

    #[test]
    fn totals_of_nothing_is_zero() {
        assert_eq!(totals(&[]), (Money::ZERO, Money::ZERO));
    }

    #[test]
    fn trial_balance_row_follows_the_normal_side() {
        let asset = TrialBalanceRow::new("1102", "銀行存款", dec!(214000), dec!(550953), Side::Debit);
        assert_eq!(asset.balance, dec!(-336953), "資產出現貸餘時要照實回報負數");

        let liability =
            TrialBalanceRow::new("2101", "應付帳款", dec!(156300), dec!(224300), Side::Credit);
        assert_eq!(liability.balance, dec!(68000), "負債貸餘為正");
    }

    /// The eight hours that separate a UTC clock from the business day. Any
    /// instant in that window belongs to a different date depending on which
    /// clock is read, which is exactly the mistake this offset exists to
    /// prevent.
    #[test]
    fn the_business_day_is_not_the_utc_day() {
        let early = DateTime::parse_from_rfc3339("2026-08-06T23:50:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(early.date_naive().to_string(), "2026-08-06");
        assert_eq!(business_time(early).date().to_string(), "2026-08-07");
        assert_eq!(business_time(early).time().to_string(), "07:50:00");
    }

    /// Amounts must be strings in JSON, or the frontend will read them as
    /// IEEE754.
    #[test]
    fn money_serialises_as_a_string() {
        let json = serde_json::to_string(&line("6118", dec!(8420.00), dec!(0))).unwrap();
        assert!(json.contains(r#""debit":"8420.00""#), "實際輸出：{json}");
    }
}
