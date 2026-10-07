//! Attendance, clock-in records, leave requests and approvals.
//!
//! Punches are the only record of a month. [`PunchSummary`] derives worked
//! and overtime *hours* from the clock and never turns them into an amount:
//! overtime pay comes from an approved request, priced by the ruleset of the
//! day worked (0057).
//!
//! Working hours are company policy, so they are read from `company_setting`
//! at call time, never held in this file. When `work_start` is blank the
//! answer to "was this person late" is *not asked*, not "no".
//!
//! The database stores only approved annual leave days. The entitlement is
//! derived from the `annual_leave` ruleset and completed months of service;
//! for departed employees service stops at the termination date.
//!
//! [`DecideLeaveRequest`] does not read the status first at the operation
//! layer. Whether the request is still pending is decided by the store during
//! the update, so two managers acting at once cannot both win.

use crate::ops::employees::{service_months, BASIS_HOURLY, INSURED_OWNER};
use crate::ops::insurance::ruleset_json;
use crate::ops::settings::{deciding_as, own_request_refusal, own_revoke_refusal};
use crate::{parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::{
    DateTime, Datelike, FixedOffset, Months, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc,
    Weekday,
};
use laysi_domain::{business_time, Money};
use laysi_regulation::Ruleset;
use laysi_store::{
    leave_payroll_span, overtime_payroll_span, overtime_status_label, payroll_blocker,
    AttendancePunch, LeaveRequest, OvertimeRequest, PayrollPeriodState, PunchCorrection,
};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

/// How many leave requests a query returns when it does not say.
///
/// Not everything: a company that has been running for years accumulates
/// thousands, and the screen shows the recent ones. The caller asks for more
/// by passing `limit`.
const DEFAULT_LEAVE_LIMIT: i64 = 500;
use std::collections::BTreeMap;

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(ListAttendancePunches),
        Box::new(CorrectPunch),
        Box::new(ListLeaveRequests),
        Box::new(DecideLeaveRequest),
        Box::new(RevokeLeaveRequest),
        Box::new(ListOvertimeRequests),
        Box::new(DecideOvertimeRequest),
        Box::new(RevokeOvertimeRequest),
        Box::new(ListPunchCorrections),
        Box::new(DecidePunchCorrection),
    ]
}

fn valid_month(month: &str) -> bool {
    chrono::NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d").is_ok()
}

/// The first and last date of a month already checked by [`valid_month`].
pub(crate) fn month_span(month: &str) -> (NaiveDate, NaiveDate) {
    let start = NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d")
        .expect("呼叫前已經以 valid_month 檢查過");
    let end = start
        .checked_add_months(Months::new(1))
        .and_then(|next| next.pred_opt())
        .unwrap_or(start);
    (start, end)
}

// Clock-in records

/// The two things a punch can be.
pub(crate) const PUNCH_KINDS: &[&str] = &["in", "out"];

pub(crate) fn punch_kind_label(kind: &str) -> &'static str {
    if kind == "in" {
        "上班"
    } else {
        "下班"
    }
}

/// Wall-clock format for a punch. Seconds are shown because they are stored:
/// truncating the display would make two punches a minute apart look
/// identical on screen while the record says otherwise.
pub(crate) const PUNCH_TIME: &str = "%H:%M:%S";

/// Turn a wall-clock time on a working day into the instant it names.
///
/// The offset is a fixed one rather than a named zone, so this never returns
/// an ambiguous or missing local time the way a daylight-saving zone would.
pub fn business_instant(day: NaiveDate, at: NaiveTime, offset: FixedOffset) -> DateTime<Utc> {
    offset
        .from_local_datetime(&day.and_time(at))
        .single()
        .expect("固定時區的本地時間必然唯一對應一個瞬間")
        .with_timezone(&Utc)
}

/// When the working day starts and ends, as the company has stated it.
///
/// Every field is optional because a fresh install has stated nothing. The
/// alternative — assuming 09:00 to 18:00 — would be this system inventing a
/// fact about someone else's company and then judging people against it.
pub(crate) struct WorkSchedule {
    pub starts_at: Option<NaiveTime>,
    pub ends_at: Option<NaiveTime>,
    /// Minutes after `starts_at` that still count as on time.
    pub grace_minutes: i64,
    /// Whether that number was stated, as opposed to falling back to zero.
    pub grace_stated: bool,
    /// How much later than `starts_at` the day may begin, with the expected
    /// end shifting by the same amount. Zero means no flexitime.
    ///
    /// Distinct from `grace_minutes`, and the distinction is the point: grace
    /// forgives a late arrival and still expects the same departure,
    /// flexitime moves the whole day and keeps its length.
    pub flex_minutes: i64,
    /// The unpaid break inside the working day. Both ends or neither: the
    /// two settings are written one at a time, and half a range is not a
    /// break.
    pub break_start: Option<NaiveTime>,
    pub break_end: Option<NaiveTime>,
    /// The dates that differ from the Monday-to-Friday default, and nothing
    /// else. Around twenty rows a year, so the whole table is carried here
    /// rather than queried per date.
    pub calendar: BTreeMap<NaiveDate, (DayKind, String)>,
}

/// What one date is, for leave, attendance and overtime.
///
/// The default is the week the Labor Standards Act describes for a five-day
/// week: Monday to Friday work, Saturday a rest day, Sunday a regular leave
/// day. `work_calendar` states only the dates that differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayKind {
    Workday,
    RestDay,
    RegularLeave,
    Holiday,
    /// A day the local authority stopped work on. Not a statutory holiday:
    /// the Labor Standards Act says nothing about it, so nothing here may
    /// treat it as one beyond it not being a working day.
    NaturalDisaster,
}

impl DayKind {
    pub fn code(self) -> &'static str {
        match self {
            DayKind::Workday => "workday",
            DayKind::RestDay => "rest_day",
            DayKind::RegularLeave => "regular_leave",
            DayKind::Holiday => "holiday",
            DayKind::NaturalDisaster => "natural_disaster",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DayKind::Workday => "工作日",
            DayKind::RestDay => "休息日",
            DayKind::RegularLeave => "例假",
            DayKind::Holiday => "國定假日",
            DayKind::NaturalDisaster => "天災停班",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        DAY_KINDS.iter().copied().find(|k| k.code() == code)
    }

    pub fn works(self) -> bool {
        self == DayKind::Workday
    }

    /// The `overtime.toml` day type this is priced under.
    //
    // ponytail: every day off is priced as a rest day. Articles 39 and 40 pay
    // a doubled day rather than an hourly band, which the payload has no shape
    // for; adding it means a new field on day_types and a daily wage, both
    // dated regulation values. A 天災停班 day has no statutory rate at all --
    // it is what the two sides agree -- so it stays here until the payload can
    // say "as agreed".
    fn overtime_code(self) -> &'static str {
        match self {
            DayKind::Workday => "weekday",
            _ => "rest_day",
        }
    }
}

pub const DAY_KINDS: [DayKind; 5] = [
    DayKind::Workday,
    DayKind::RestDay,
    DayKind::RegularLeave,
    DayKind::Holiday,
    DayKind::NaturalDisaster,
];

/// The kind a date has when the calendar says nothing about it.
pub fn default_day_kind(day: NaiveDate) -> DayKind {
    match day.weekday() {
        Weekday::Sat => DayKind::RestDay,
        Weekday::Sun => DayKind::RegularLeave,
        _ => DayKind::Workday,
    }
}

impl WorkSchedule {
    pub async fn load(ctx: &OpCtx) -> Result<Self, OpError> {
        let settings = ctx.store.company_settings().await?;
        let value = |code: &str| {
            settings
                .iter()
                .find(|s| s.code == code)
                .map(|s| s.value.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let time =
            |code: &str| value(code).and_then(|v| NaiveTime::parse_from_str(&v, "%H:%M").ok());
        let minutes = |code: &str| value(code).and_then(|v| v.parse::<i64>().ok());
        let grace = minutes("late_grace_minutes");
        Ok(Self {
            starts_at: time("work_start"),
            ends_at: time("work_end"),
            grace_minutes: grace.unwrap_or(0).max(0),
            grace_stated: grace.is_some(),
            flex_minutes: minutes("flex_window_minutes").unwrap_or(0).max(0),
            break_start: time("break_start"),
            break_end: time("break_end"),
            calendar: ctx
                .store
                .work_calendar()
                .await?
                .into_iter()
                .filter_map(|d| DayKind::from_code(&d.kind).map(|k| (d.on_date, (k, d.name))))
                .collect(),
        })
    }

    /// What this date is. The calendar states only the exceptions.
    pub fn day_kind(&self, day: NaiveDate) -> DayKind {
        self.calendar
            .get(&day)
            .map_or_else(|| default_day_kind(day), |(k, _)| *k)
    }

    /// The name the calendar gives this date, for a screen to print.
    pub fn day_name(&self, day: NaiveDate) -> Option<&str> {
        self.calendar
            .get(&day)
            .map(|(_, name)| name.as_str())
            .filter(|n| !n.is_empty())
    }

    /// The `overtime.toml` day type a date is priced under.
    pub fn overtime_day_type(&self, day: NaiveDate) -> &'static str {
        self.day_kind(day).overtime_code()
    }

    /// The calendar dates priced differently from their day of the week, so
    /// a screen can show the day type before the request is sent.
    pub fn overtime_exceptions(&self) -> Vec<Value> {
        self.calendar
            .iter()
            .filter(|(day, (kind, _))| {
                kind.overtime_code() != default_day_kind(**day).overtime_code()
            })
            .map(|(day, (kind, name))| {
                json!({ "date": day, "day_type": kind.overtime_code(), "name": name })
            })
            .collect()
    }

    pub fn flexible(&self) -> bool {
        self.flex_minutes > 0
    }

    /// The break as a range, or `None` when it is not stated or does not
    /// describe one. A half-entered break reads as no break: the two settings
    /// are saved one at a time, so the state between them is normal.
    fn break_window(&self) -> Option<(NaiveTime, NaiveTime)> {
        let (start, end) = (self.break_start?, self.break_end?);
        (end > start).then_some((start, end))
    }

    /// Seconds of the break that fall inside a range, counted once for every
    /// date the range touches.
    pub fn break_seconds(&self, from: NaiveDateTime, to: NaiveDateTime) -> i64 {
        let Some((start, end)) = self.break_window() else {
            return 0;
        };
        let mut total = 0;
        let mut day = from.date();
        while day <= to.date() {
            let open = day.and_time(start).max(from);
            let close = day.and_time(end).min(to);
            total += (close - open).num_seconds().max(0);
            let Some(next) = day.succ_opt() else {
                break;
            };
            day = next;
        }
        total
    }

    /// The latest an arrival may be and still count as on time, before grace.
    pub fn latest_start(&self) -> Option<NaiveTime> {
        Some(self.starts_at? + chrono::Duration::minutes(self.flex_minutes))
    }

    /// How much this day is shifted by, given when the person arrived.
    ///
    /// Never negative: arriving early does not shorten the day. Nobody was
    /// asked to come early, and letting it count would turn a favour into an
    /// obligation to leave later than agreed.
    fn shift_minutes(&self, arrived: Option<NaiveDateTime>) -> i64 {
        let (Some(starts_at), Some(arrived)) = (self.starts_at, arrived) else {
            return 0;
        };
        let start = arrived.date().and_time(starts_at);
        (arrived - start).num_minutes().clamp(0, self.flex_minutes)
    }

    /// When this person's day is expected to end, or `None` without an end
    /// time. Under flexitime it moves with the arrival; otherwise it is the
    /// same for everybody.
    pub fn expected_end(
        &self,
        date: NaiveDate,
        arrived: Option<NaiveDateTime>,
    ) -> Option<NaiveDateTime> {
        Some(date.and_time(self.ends_at?) + chrono::Duration::minutes(self.shift_minutes(arrived)))
    }

    /// The moment after which an arrival counts as late: the start, plus the
    /// flexitime window, plus grace.
    fn arrival_deadline(&self, date: NaiveDate) -> Option<NaiveDateTime> {
        Some(
            date.and_time(self.starts_at?)
                + chrono::Duration::minutes(self.flex_minutes + self.grace_minutes),
        )
    }

    /// Whether an arrival was late, or `None` when no start time is stated.
    pub fn late(&self, arrived: NaiveDateTime) -> Option<bool> {
        Some(arrived > self.arrival_deadline(arrived.date())?)
    }

    /// How far past the deadline an arrival was, or `None` when no start time
    /// is stated. Zero when on time.
    pub fn late_by_seconds(&self, arrived: NaiveDateTime) -> Option<i64> {
        Some(
            (arrived - self.arrival_deadline(arrived.date())?)
                .num_seconds()
                .max(0),
        )
    }

    /// Attendance beyond the normal working day, or `None` without both work
    /// times or without an arrival. The break is taken out first, so time in
    /// the middle of the day is not paid twice.
    ///
    /// Measured against the length of the day rather than the moment it ends,
    /// because those two answers differ whenever someone arrives before the
    /// day opens: an attendance record is what the hours are read from, and
    /// the alternative reports a fifteen-hour day as an hour and a half.
    /// Whether any of it is paid is a separate question with a separate
    /// answer -- an approved overtime request, priced by the article 24
    /// multipliers in the rule library.
    //
    // ponytail: the normal day is the company window, not the statutory eight
    // hours, so a window longer than that reports too little overtime. The cap
    // is a dated regulation value and belongs in overtime.toml beside the
    // multipliers before it can be applied here.
    pub fn overtime_seconds(
        &self,
        arrived: Option<NaiveDateTime>,
        left: NaiveDateTime,
    ) -> Option<i64> {
        let arrived = arrived?;
        // Nothing is owed on a day nobody was asked to work, so the whole of
        // the attendance is beyond the normal day.
        let normal = if self.day_kind(arrived.date()).works() {
            self.daily_seconds()?
        } else {
            0
        };
        let attended = (left - arrived).num_seconds().max(0);
        Some((attended - self.break_seconds(arrived, left) - normal).max(0))
    }

    /// Whether overtime can be derived at all: it is measured against the
    /// length of the working day, and without both times there is none.
    pub fn counts_overtime(&self) -> bool {
        self.daily_seconds().is_some()
    }

    /// How long a working day is, break excluded, or `None` without both
    /// times.
    pub fn daily_seconds(&self) -> Option<i64> {
        let (starts_at, ends_at) = (self.starts_at?, self.ends_at?);
        let paused = self.break_window().map_or(0, |(start, end)| {
            (end.min(ends_at) - start.max(starts_at))
                .num_seconds()
                .max(0)
        });
        Some((ends_at - starts_at).num_seconds() - paused)
    }

    /// Working seconds inside a leave range.
    ///
    /// Each date the range touches contributes the overlap between the range
    /// and that date's working window, and a day the calendar does not call a
    /// working day contributes nothing. Evenings, the week off and public
    /// holidays are not leave, so a Friday afternoon through Monday morning is
    /// worth the two half days it covers.
    ///
    /// `None` without both work times. The window is the only thing that
    /// turns a range into hours and there is nothing to fall back on that
    /// would not amount to inventing a working day.
    pub fn leave_seconds(&self, from: NaiveDateTime, to: NaiveDateTime) -> Option<i64> {
        let (starts_at, ends_at) = (self.starts_at?, self.ends_at?);
        let mut total = 0;
        let mut day = from.date();
        while day <= to.date() {
            if self.day_kind(day).works() {
                let open = day.and_time(starts_at).max(from);
                let close = day.and_time(ends_at).min(to);
                total += (close - open).num_seconds().max(0) - self.break_seconds(open, close);
            }
            day = day.succ_opt()?;
        }
        Some(total)
    }

    pub fn json(&self) -> Value {
        let hhmm = |t: NaiveTime| t.format("%H:%M").to_string();
        json!({
            "work_start": self.starts_at.map(hhmm),
            "work_end": self.ends_at.map(hhmm),
            "break_start": self.break_window().map(|(start, _)| hhmm(start)),
            "break_end": self.break_window().map(|(_, end)| hhmm(end)),
            "grace_minutes": self.grace_minutes,
            "grace_stated": self.grace_stated,
            "flex_minutes": self.flex_minutes,
            "flexible": self.flexible(),
            "latest_start": self.latest_start().map(hhmm),
            "judges_lateness": self.starts_at.is_some(),
            "counts_overtime": self.counts_overtime(),
            "source": "company_setting 的 work_start／work_end／break_start／break_end／flex_window_minutes／late_grace_minutes",
            "note": match (self.starts_at, self.ends_at) {
                (None, None) => "尚未設定上下班時間，因此只記錄打卡時間，不判定遲到也不計加班時數。",
                (None, Some(_)) => "尚未設定上班時間，因此不判定遲到，也不計加班時數。",
                (Some(_), None) => "尚未設定下班時間，因此不計加班時數。",
                (Some(_), Some(_)) if self.flexible() =>
                    "採彈性工時：在彈性區間內開始上班都算準時，下班時間同步順延，工作總時數不變。",
                (Some(_), Some(_)) => "遲到與加班時數依公司設定的上下班時間判定。",
            },
        })
    }
}

/// What article 32 allows, as the overtime ruleset states it: extended time in
/// one day, by the kind of day it is, and extended time in one month. Punches
/// are the record an inspection reads, so the hours derived from them are
/// worth nothing unless something compares them against these two numbers.
pub(crate) struct OvertimeLimits {
    day_types: Vec<laysi_regulation::OvertimeDayType>,
    monthly_hours: Money,
}

impl OvertimeLimits {
    pub fn resolve(as_of: NaiveDate) -> Result<Self, OpError> {
        let rs = laysi_regulation::resolve("overtime", as_of)?;
        Ok(Self {
            day_types: laysi_regulation::overtime_day_types(rs)?,
            monthly_hours: laysi_regulation::overtime_monthly_max_hours(rs)?,
        })
    }

    /// The kind of day a date is, on the same reading the overtime request
    /// screen uses: the work calendar decides it.
    fn day_type(
        &self,
        schedule: &WorkSchedule,
        date: NaiveDate,
    ) -> Option<&laysi_regulation::OvertimeDayType> {
        let code = schedule.overtime_day_type(date);
        self.day_types.iter().find(|t| t.code == code)
    }

    fn over_daily(&self, schedule: &WorkSchedule, date: NaiveDate, seconds: i64) -> bool {
        self.day_type(schedule, date)
            .is_some_and(|t| Decimal::from(seconds) > t.max_hours * Decimal::from(3600))
    }

    fn over_monthly(&self, seconds: i64) -> bool {
        Decimal::from(seconds) > self.monthly_hours * Decimal::from(3600)
    }
}

/// One working day of one person, built from that day's punches.
#[derive(Default)]
pub(crate) struct PunchDay {
    pub date: NaiveDate,
    pub arrived: Option<NaiveDateTime>,
    pub left: Option<NaiveDateTime>,
    pub arrived_source: Option<String>,
    pub left_source: Option<String>,
    pub note: String,
    /// Leave requests, pending or approved, that cover part of this date.
    /// Filled by [`mark_days`]; empty for a day read on its own.
    pub leaves: Vec<DayLeave>,
    /// A working day with no punch and no leave on file, once it is over.
    pub absent: bool,
}

/// The part of one leave request that falls on one date.
pub(crate) struct DayLeave {
    pub leave_no: String,
    pub kind: String,
    pub status: String,
    /// Working seconds of the request on this date, or `None` without both
    /// work times.
    pub seconds: Option<i64>,
}

/// Employment types absence is not judged for. A part timer works the days
/// agreed with them, and the roster does not record which those are, so every
/// other weekday would read as a missed day.
//
// ponytail: part timers are skipped wholesale until #3 records agreed working
// days; then absence can be judged against those instead.
const NO_ABSENCE_JUDGED: &[&str] = &["part_time"];

/// Whether a missing day counts as absence for this person under this
/// schedule. Without a start time the company has not said when a day
/// begins, which is the same reason lateness is not judged then. An owner
/// insured as one is the employer, not someone whose attendance is kept.
pub(crate) fn judges_absence(
    schedule: &WorkSchedule,
    employment_type: &str,
    insured_as: &str,
) -> bool {
    schedule.starts_at.is_some()
        && !NO_ABSENCE_JUDGED.contains(&employment_type)
        && insured_as != INSURED_OWNER
}

/// Attach leave to each day and mark the days nobody accounted for.
///
/// `leaves` are one person's requests; only those still holding their hours
/// are attached (see [`holds_hours`]). A day is
/// absent when the calendar calls it a working day, it ended before `today`,
/// and it has neither a punch, a leave request, nor a punch correction under
/// review (`awaiting_fix`). A request still waiting for a decision counts:
/// whether it will be granted is a question for the approver, and calling
/// the day absent in the meantime answers it for them.
pub(crate) fn mark_days(
    days: &mut [PunchDay],
    leaves: &[&LeaveRequest],
    awaiting_fix: &[NaiveDate],
    schedule: &WorkSchedule,
    judge_absence: bool,
    today: NaiveDate,
) {
    for day in days.iter_mut() {
        let (open, close) = (
            day.date.and_time(NaiveTime::MIN),
            day.date.and_time(NaiveTime::MIN) + chrono::Duration::days(1),
        );
        day.leaves = leaves
            .iter()
            .filter(|l| holds_hours(&l.status) && l.from_at < close && l.to_at > open)
            .map(|l| DayLeave {
                leave_no: l.leave_no.clone(),
                kind: l.kind.clone(),
                status: l.status.clone(),
                seconds: schedule.leave_seconds(l.from_at.max(open), l.to_at.min(close)),
            })
            // A range across a weekend or a holiday takes nothing from those
            // dates, and printing it there reads as leave taken on a day off.
            .filter(|l| l.seconds != Some(0))
            .collect();
        day.absent = judge_absence
            && day.date < today
            && schedule.day_kind(day.date).works()
            && day.arrived.is_none()
            && day.left.is_none()
            && day.leaves.is_empty()
            && !awaiting_fix.contains(&day.date);
    }
}

impl PunchDay {
    /// Attendance with the break taken out. A day holds one punch pair, so
    /// the break sits inside it whenever the pair covers it.
    pub fn worked_seconds(&self, schedule: &WorkSchedule) -> Option<i64> {
        let (arrived, left) = (self.arrived?, self.left?);
        let attended = (left - arrived).num_seconds().max(0);
        Some((attended - schedule.break_seconds(arrived, left)).max(0))
    }

    /// A day with an arrival and no departure. Common and worth naming: it is
    /// the state a person is in right now, and also the state a forgotten
    /// punch leaves behind.
    pub fn open(&self) -> bool {
        self.arrived.is_some() && self.left.is_none()
    }

    pub fn json(&self, schedule: &WorkSchedule, limits: &OvertimeLimits) -> Value {
        let clock = |t: Option<NaiveDateTime>| t.map(|t| t.format(PUNCH_TIME).to_string());
        let late = self.arrived.and_then(|a| schedule.late(a));
        let overtime = self
            .left
            .and_then(|l| schedule.overtime_seconds(self.arrived, l));
        let day_type = limits.day_type(schedule, self.date);
        let kind = schedule.day_kind(self.date);
        json!({
            "date": self.date,
            "weekday": weekday_label(self.date),
            "day_kind": kind.code(),
            "day_type_label": kind.label(),
            "day_name": schedule.day_name(self.date),
            "works": kind.works(),
            "leaves": self.leaves.iter().map(|l| json!({
                "leave_no": l.leave_no,
                "kind": l.kind,
                "kind_label": leave_kind_label(&l.kind),
                "status": l.status,
                "status_label": leave_status_label(&l.status),
                "hours": l.seconds.map(|s| hours(s).to_string()),
            })).collect::<Vec<_>>(),
            "absent": self.absent,
            "in_at": clock(self.arrived),
            "out_at": clock(self.left),
            "in_source": self.arrived_source,
            "out_source": self.left_source,
            "manual": self.arrived_source.as_deref() == Some("manual")
                || self.left_source.as_deref() == Some("manual"),
            "worked_seconds": self.worked_seconds(schedule),
            "worked_hours": self.worked_seconds(schedule).map(|s| hours(s).to_string()),
            "open": self.open(),
            "late": late,
            "late_seconds": self.arrived.and_then(|a| schedule.late_by_seconds(a)),
            "expected_end": schedule
                .expected_end(self.date, self.arrived)
                .map(|t| t.format(PUNCH_TIME).to_string()),
            "overtime_seconds": overtime,
            "overtime_limit_hours": day_type.map(|t| t.max_hours.normalize().to_string()),
            "over_overtime_limit": overtime.is_some_and(|s| limits.over_daily(schedule, self.date, s)),
            "note": self.note,
        })
    }
}

fn weekday_label(date: NaiveDate) -> &'static str {
    match date.weekday() {
        chrono::Weekday::Mon => "一",
        chrono::Weekday::Tue => "二",
        chrono::Weekday::Wed => "三",
        chrono::Weekday::Thu => "四",
        chrono::Weekday::Fri => "五",
        chrono::Weekday::Sat => "六",
        chrono::Weekday::Sun => "日",
    }
}

/// Seconds as hours to one decimal place.
fn hours(seconds: i64) -> Decimal {
    (Decimal::from(seconds) / Decimal::from(3600)).round_dp(1)
}

/// Group one person's punches into days.
///
/// The rows arrive ordered by day and then by kind, so an arrival is always
/// seen before the departure it belongs to.
pub(crate) fn punch_days(punches: &[AttendancePunch], offset: FixedOffset) -> Vec<PunchDay> {
    let mut days: Vec<PunchDay> = Vec::new();
    for punch in punches {
        let local = business_time(punch.punched_at, offset);
        if days.last().map(|d| d.date) != Some(punch.punched_on) {
            days.push(PunchDay {
                date: punch.punched_on,
                ..Default::default()
            });
        }
        let day = days.last_mut().expect("剛才推入了一天");
        if punch.kind == "in" {
            day.arrived = Some(local);
            day.arrived_source = Some(punch.source.clone());
        } else {
            day.left = Some(local);
            day.left_source = Some(punch.source.clone());
        }
        if !punch.note.is_empty() {
            if !day.note.is_empty() {
                day.note.push_str(" · ");
            }
            day.note.push_str(&punch.note);
        }
    }
    days
}

/// Every day of `month` between `first` and `last`, with the days that have
/// punches in place.
///
/// `punch_days` only knows the days that produced a punch, so a day nobody
/// clocked in on has no row at all — and that is exactly the day a correction
/// is asked for. `first` and `last` keep the range inside the employment and
/// out of the future.
pub(crate) fn month_days(
    punched: Vec<PunchDay>,
    month: &str,
    first: NaiveDate,
    last: NaiveDate,
) -> Vec<PunchDay> {
    let mut by_date: BTreeMap<NaiveDate, PunchDay> =
        punched.into_iter().map(|day| (day.date, day)).collect();
    if let Ok(start) = NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d") {
        let end = start
            .checked_add_months(Months::new(1))
            .and_then(|next| next.pred_opt())
            .unwrap_or(start);
        let mut date = start.max(first);
        let stop = end.min(last);
        while date <= stop {
            by_date.entry(date).or_insert_with(|| PunchDay {
                date,
                ..Default::default()
            });
            match date.succ_opt() {
                Some(next) => date = next,
                None => break,
            }
        }
    }
    by_date.into_values().collect()
}

/// What a month of punches adds up to.
///
/// Everything here is derived from the punches themselves.
pub(crate) struct PunchSummary {
    pub days_with_arrival: i64,
    pub complete_days: i64,
    pub open_days: i64,
    pub worked_seconds: i64,
    pub late_days: Option<i64>,
    pub overtime_seconds: Option<i64>,
    pub manual_days: i64,
    /// Working days with no punch and no leave, as [`mark_days`] judged them.
    pub absent_days: i64,
    /// Days whose overtime is past what that kind of day allows.
    pub over_limit_days: i64,
    /// Whether the month's overtime is past the monthly ceiling.
    pub over_monthly_limit: bool,
    pub monthly_max_hours: Money,
}

pub(crate) fn summarise(
    days: &[PunchDay],
    schedule: &WorkSchedule,
    limits: &OvertimeLimits,
) -> PunchSummary {
    let overtime = |d: &PunchDay| d.left.and_then(|l| schedule.overtime_seconds(d.arrived, l));
    let overtime_seconds = schedule
        .daily_seconds()
        .map(|_| days.iter().filter_map(overtime).sum());
    PunchSummary {
        days_with_arrival: days.iter().filter(|d| d.arrived.is_some()).count() as i64,
        complete_days: days
            .iter()
            .filter_map(|d| d.worked_seconds(schedule))
            .count() as i64,
        open_days: days.iter().filter(|d| d.open()).count() as i64,
        worked_seconds: days.iter().filter_map(|d| d.worked_seconds(schedule)).sum(),
        late_days: schedule.starts_at.map(|_| {
            days.iter()
                .filter(|d| d.arrived.and_then(|a| schedule.late(a)) == Some(true))
                .count() as i64
        }),
        overtime_seconds,
        manual_days: days
            .iter()
            .filter(|d| {
                d.arrived_source.as_deref() == Some("manual")
                    || d.left_source.as_deref() == Some("manual")
            })
            .count() as i64,
        absent_days: days.iter().filter(|d| d.absent).count() as i64,
        over_limit_days: days
            .iter()
            .filter(|d| overtime(d).is_some_and(|s| limits.over_daily(schedule, d.date, s)))
            .count() as i64,
        over_monthly_limit: overtime_seconds.is_some_and(|s| limits.over_monthly(s)),
        monthly_max_hours: limits.monthly_hours,
    }
}

impl PunchSummary {
    pub fn json(&self) -> Value {
        json!({
            "days_with_arrival": self.days_with_arrival,
            "complete_days": self.complete_days,
            "open_days": self.open_days,
            "worked_seconds": self.worked_seconds,
            "worked_hours": hours(self.worked_seconds).to_string(),
            "late_days": self.late_days,
            "overtime_seconds": self.overtime_seconds,
            "overtime_hours": self.overtime_seconds.map(|s| hours(s).to_string()),
            "manual_days": self.manual_days,
            "absent_days": self.absent_days,
            "over_limit_days": self.over_limit_days,
            "over_monthly_limit": self.over_monthly_limit,
            "monthly_max_hours": self.monthly_max_hours.normalize().to_string(),
            "overtime_pay_computed": false,
            "overtime_note": "加班費請送出加班申請。",
        })
    }
}

/// Values accepted by `leave_request.kind`. A migration test compares this
/// against the CHECK verbatim, so a new leave type cannot quietly fall
/// through to "other" on screen.
pub(crate) const LEAVE_KINDS: &[&str] = &[
    "annual",
    "sick",
    "personal",
    "official",
    "marriage",
    "bereavement",
    "maternity",
    "menstrual",
];

pub(crate) fn valid_leave_kind(kind: &str) -> bool {
    LEAVE_KINDS.contains(&kind)
}

pub(crate) fn leave_kind_label(kind: &str) -> &'static str {
    match kind {
        "annual" => "特休",
        "sick" => "病假",
        "personal" => "事假",
        "official" => "公假",
        "marriage" => "婚假",
        "bereavement" => "喪假",
        "maternity" => "產假",
        "menstrual" => "生理假",
        _ => "其他",
    }
}

pub(crate) use laysi_store::leave_status_label;

/// Whether a leave or overtime request still claims its hours: waiting for a
/// decision, or approved. Rejected, withdrawn and revoked requests have given
/// them back, the same line the overlap constraints draw.
pub(crate) fn holds_hours(status: &str) -> bool {
    matches!(status, "pending" | "approved")
}

#[derive(Clone, Copy)]
struct AnnualBalance {
    entitlement: Money,
    taken: Money,
}

impl AnnualBalance {
    fn remaining(self) -> Money {
        self.entitlement - self.taken
    }
}

fn annual_entitlement(
    rs: &Ruleset,
    hired_on: NaiveDate,
    earned_through: NaiveDate,
) -> Result<Money, laysi_regulation::RegulationError> {
    annual_entitlement_for_service(rs, service_months(hired_on, earned_through))
}

fn annual_entitlement_for_service(
    rs: &Ruleset,
    months: u32,
) -> Result<Money, laysi_regulation::RegulationError> {
    Ok(Money::from(laysi_regulation::annual_leave_days(
        rs, months,
    )?))
}

fn leave_json(request: &LeaveRequest, balance: Option<AnnualBalance>) -> Value {
    json!({
        "leave_no": request.leave_no,
        "emp_no": request.emp_no,
        "name": request.name,
        "dept_label": request.dept_label,
        "kind": request.kind,
        "kind_label": leave_kind_label(&request.kind),
        "from_at": request.from_at,
        "to_at": request.to_at,
        "from_on": request.from_on,
        "to_on": request.to_on,
        "hours": request.hours.to_string(),
        "days": request.days.to_string(),
        "reason": request.reason,
        "status": request.status,
        "status_label": leave_status_label(&request.status),
        "decided_by": request.decided_by,
        "decided_role": request.decided_role,
        "decided_at": request.decided_at,
        "decision_note": request.decision_note,
        "cancelled": request.cancelled,
        "created_at": request.created_at,
        "created_by": request.created_by,
        "annual_leave": balance.map(|b| json!({
            "entitlement": b.entitlement.to_string(),
            "taken": b.taken.to_string(),
            "remaining": b.remaining().to_string(),
        })),
    })
}

// Punches, company-wide

struct ListAttendancePunches;

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListAttendancePunchesInput {
    /// Month (YYYY-MM). Defaults to this month.
    #[serde(default)]
    pub month: Option<String>,
    /// Employee number. Without it the reply is one summary row per person;
    /// with it, the day-by-day detail for that person as well.
    #[serde(default)]
    pub emp_no: Option<String>,
}

#[async_trait]
impl Operation for ListAttendancePunches {
    fn name(&self) -> &'static str {
        "list_attendance_punches"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀某個月全公司的打卡紀錄：逐人統計（實際打滿的天數、時數、遲到天數、加班時數）。指定員工編號時另附該員的逐日明細。上下班時間取自公司設定，未設定時不判定遲到。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<ListAttendancePunchesInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListAttendancePunchesInput = parse(input)?;
        let month = i
            .month
            .unwrap_or_else(|| ctx.today.format("%Y-%m").to_string());
        if !valid_month(&month) {
            return Err(OpError::BadInput(format!(
                "月份要寫成 YYYY-MM，收到的是「{month}」"
            )));
        }
        let only = match &i.emp_no {
            None => None,
            Some(emp_no) => Some(ctx.store.employee(emp_no).await?),
        };

        let (start, end) = month_span(&month);
        let schedule = WorkSchedule::load(ctx).await?;
        let limits = OvertimeLimits::resolve(ctx.today)?;
        let only_id = only.as_ref().map(|e| e.id);
        let punches = ctx.store.punches(&month, only_id).await?;
        let leaves = ctx
            .store
            .leave_requests_matching(true, only_id, Some((start, end)))
            .await?;
        // Only the ones under review: a handful company-wide at any time.
        let awaiting = ctx.store.punch_corrections(false, only_id).await?;
        // The real date, as for the punches themselves: a day is only missed
        // once it is over, and `LAYSI_TODAY` does not move the clock.
        let today = business_time(Utc::now(), ctx.offset).date();

        // Everybody employed during the month, not only those who punched:
        // somebody who never clocked in is the row this screen exists for.
        let staff = ctx
            .store
            .employees(true, None)
            .await?
            .into_iter()
            .filter(|e| only_id.is_none_or(|id| id == e.id))
            .filter(|e| {
                (e.hired_on <= end && e.left_on.is_none_or(|left| left >= start))
                    || punches.iter().any(|p| p.employee_id == e.id)
            });

        let mut rows = Vec::new();
        let mut detail = Value::Null;
        for e in staff {
            let theirs: Vec<AttendancePunch> = punches
                .iter()
                .filter(|p| p.employee_id == e.id)
                .cloned()
                .collect();
            let last = e.left_on.map_or(today, |left| left.min(today));
            let mut days = month_days(punch_days(&theirs, ctx.offset), &month, e.hired_on, last);
            let requests: Vec<&LeaveRequest> =
                leaves.iter().filter(|l| l.employee_id == e.id).collect();
            let fixes: Vec<NaiveDate> = awaiting
                .iter()
                .filter(|c| c.employee_id == e.id)
                .map(|c| c.punched_on)
                .collect();
            let judge = judges_absence(&schedule, &e.employment_type, &e.insured_as);
            mark_days(&mut days, &requests, &fixes, &schedule, judge, today);
            rows.push(json!({
                "emp_no": e.emp_no,
                "name": e.name,
                "dept_label": e.dept_label,
                "summary": summarise(&days, &schedule, &limits).json(),
            }));
            if only_id == Some(e.id) {
                detail = json!(days
                    .iter()
                    .map(|d| d.json(&schedule, &limits))
                    .collect::<Vec<_>>());
            }
        }

        let total =
            |key: &str| -> i64 { rows.iter().filter_map(|r| r["summary"][key].as_i64()).sum() };
        let (open_days, absent_days) = (total("open_days"), total("absent_days"));
        let summary = format!(
            "讀 {month} 出勤 · {} 人 · {open_days} 天只打上班沒打下班 · {absent_days} 天沒有打卡也沒有休假",
            rows.len()
        );

        Ok(OpOutcome::new(
            json!({
                "month": month,
                "months": ctx.store.punch_months().await?,
                "schedule": schedule.json(),
                "rows": rows,
                "employee": only.as_ref().map(|e| json!({ "emp_no": e.emp_no, "name": e.name })),
                "days": detail,
                "open_days": open_days,
                "absent_days": absent_days,
            }),
            summary,
        ))
    }
}

// Correcting a punch

struct CorrectPunch;

#[derive(Deserialize, JsonSchema)]
pub struct CorrectPunchInput {
    /// Whose record to correct.
    pub emp_no: String,
    /// The working day.
    pub punched_on: NaiveDate,
    /// `in` or `out`.
    pub kind: String,
    /// Wall-clock time on that day in the company's own time zone, `HH:MM:SS`.
    /// `HH:MM` is accepted and stored as second zero, because a correction is
    /// a remembered time and the second is not known; `source` already marks
    /// the row as entered by hand.
    pub at: String,
    /// Why it is being entered by hand. Kept on the record.
    #[serde(default)]
    pub note: Option<String>,
}

#[async_trait]
impl Operation for CorrectPunch {
    fn name(&self) -> &'static str {
        "correct_punch"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "補登或更正某人某天的上班或下班時間，來源記為補登並留下操作者。時間接受 HH:MM:SS，只給到分鐘時秒數記為零。同一時段已有紀錄時覆蓋原值，原時刻留在補登紀錄。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<CorrectPunchInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: CorrectPunchInput = parse(input)?;
        if !PUNCH_KINDS.contains(&i.kind.as_str()) {
            return Err(OpError::BadInput(format!(
                "打卡別只接受 in（上班）或 out（下班），收到「{}」",
                i.kind
            )));
        }
        let text = i.at.trim();
        let at = NaiveTime::parse_from_str(text, "%H:%M:%S")
            .or_else(|_| NaiveTime::parse_from_str(text, "%H:%M"))
            .map_err(|_| {
                OpError::BadInput(format!(
                    "時間要寫成 HH:MM:SS 或 HH:MM，收到的是「{}」",
                    i.at
                ))
            })?;
        if i.punched_on > ctx.today {
            return Err(OpError::BadInput("不能為未來的日期補登打卡".into()));
        }

        let employee = ctx.store.employee(&i.emp_no).await?;
        let punch = ctx
            .store
            .record_punch(
                laysi_store::NewPunch {
                    employee_id: employee.id,
                    punched_on: i.punched_on,
                    punched_at: business_instant(i.punched_on, at, ctx.offset),
                    kind: &i.kind,
                    source: "manual",
                    note: i.note.as_deref().unwrap_or("").trim(),
                    actor: &ctx.actor,
                    offset: ctx.offset,
                },
                Some(&ctx.actor_role),
            )
            .await?;

        let schedule = WorkSchedule::load(ctx).await?;
        let month = i.punched_on.format("%Y-%m").to_string();
        let days = punch_days(
            &ctx.store.punches(&month, Some(employee.id)).await?,
            ctx.offset,
        );
        let day = days.iter().find(|d| d.date == i.punched_on);
        let limits = OvertimeLimits::resolve(ctx.today)?;

        let summary = format!(
            "{} {} 的{}卡補登為 {} · 由 {} 手動輸入",
            employee.emp_no,
            i.punched_on,
            punch_kind_label(&i.kind),
            business_time(punch.punched_at, ctx.offset).format(PUNCH_TIME),
            ctx.actor
        );

        Ok(OpOutcome::new(
            json!({
                "employee": { "emp_no": employee.emp_no, "name": employee.name },
                "day": day.map(|d| d.json(&schedule, &limits)),
                "schedule": schedule.json(),
                "source": "manual",
                "note": "補登的打卡在明細上標示為手動輸入，並記錄補登的人。",
            }),
            summary,
        ))
    }
}

// Leave

struct ListLeaveRequests;

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListLeaveRequestsInput {
    /// Include requests already decided or taken back; by default only
    /// pending ones.
    #[serde(default)]
    pub include_done: bool,
    /// Maximum rows. Omit for no additional limit.
    #[serde(default)]
    pub limit: Option<i64>,
    /// Month (YYYY-MM). Only requests touching that month; omit for all.
    #[serde(default)]
    pub month: Option<String>,
    /// Employee number. Only that person's requests; omit for everybody.
    #[serde(default)]
    pub emp_no: Option<String>,
}

#[async_trait]
impl Operation for ListLeaveRequests {
    fn name(&self) -> &'static str {
        "list_leave_requests"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀休假單與逐人特休餘額。額度依查詢當日適用的特休規則與年資現算，系統只留已核准的實休天數。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<ListLeaveRequestsInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListLeaveRequestsInput = parse(input)?;
        if i.limit.is_some_and(|limit| limit <= i64::default()) {
            return Err(OpError::BadInput("查詢筆數要大於零".into()));
        }

        if let Some(month) = i.month.as_deref().filter(|m| !valid_month(m)) {
            return Err(OpError::BadInput(format!(
                "月份要寫成 YYYY-MM，收到的是「{month}」"
            )));
        }
        let only = match &i.emp_no {
            None => None,
            Some(emp_no) => Some(ctx.store.employee(emp_no).await?.id),
        };
        let mut requests = match (i.month.as_deref(), only) {
            (None, None) => {
                ctx.store
                    .leave_requests(i.include_done, i.limit.unwrap_or(DEFAULT_LEAVE_LIMIT))
                    .await?
            }
            (month, _) => {
                ctx.store
                    .leave_requests_matching(i.include_done, only, month.map(month_span))
                    .await?
            }
        };
        if let Some(limit) = i.limit {
            requests.truncate(limit as usize);
        }
        // Narrowing to a month must not hide a request waiting for a
        // decision: this list is where it gets one.
        let listed_pending = requests.iter().filter(|r| r.status == "pending").count() as i64;
        let other_pending = (ctx.store.pending_leave_count().await? - listed_pending).max(0);
        let staff = ctx.store.employees(true, None).await?;
        let rs = laysi_regulation::resolve("annual_leave", ctx.today)?;
        let taken = ctx.store.annual_leave_taken(ctx.today.year()).await?;
        let taken_by_employee: BTreeMap<i32, Money> = taken.into_iter().collect();
        let mut balances = BTreeMap::new();

        for employee in &staff {
            let earned_through = employee
                .left_on
                .map_or(ctx.today, |left| left.min(ctx.today));
            let entitlement = annual_entitlement(rs, employee.hired_on, earned_through)?;
            let used = taken_by_employee
                .get(&employee.id)
                .copied()
                .unwrap_or(Money::ZERO);
            balances.insert(
                employee.id,
                AnnualBalance {
                    entitlement,
                    taken: used,
                },
            );
        }

        // Whether this identity can decide each request, answered here so the
        // screen can draw the button dead with the reason beside it. Same
        // separation of duties as the expense flow.
        let deciding_as = deciding_as(ctx).await?;
        let paid = ctx.store.payroll_period_states().await?;
        let rows = requests
            .iter()
            .map(|request| {
                let mut row = leave_json(request, balances.get(&request.employee_id).copied());
                let own = deciding_as.is_some() && deciding_as == Some(request.employee_id);
                row["can_decide"] = json!(request.status == "pending" && !own);
                row["decide_blocked_reason"] = json!(own.then(|| {
                    own_request_refusal(&ctx.actor_role, &format!("{} ", request.leave_no))
                }));
                let span = leave_payroll_span(request.from_on, request.to_on);
                payroll_answers(
                    ctx,
                    &mut row,
                    &request.status,
                    own,
                    &request.leave_no,
                    span,
                    &paid,
                );
                row
            })
            .collect::<Vec<_>>();
        let annual_balances = staff
            .iter()
            .filter(|employee| employee.status != "left")
            .filter_map(|employee| {
                let balance = balances.get(&employee.id)?;
                Some(json!({
                    "emp_no": employee.emp_no,
                    "name": employee.name,
                    "dept_label": employee.dept_label,
                    "entitlement": balance.entitlement.to_string(),
                    "taken": balance.taken.to_string(),
                    "remaining": balance.remaining().to_string(),
                }))
            })
            .collect::<Vec<_>>();
        let pending = requests
            .iter()
            .filter(|request| request.status == "pending")
            .count();

        let summary = format!(
            "讀休假單 · {} 張（{pending} 張等簽）· 特休額度依特休規則 {} 現算",
            rows.len(),
            rs.version
        );
        Ok(OpOutcome::new(
            json!({
                "requests": rows,
                "pending_count": pending,
                "other_pending": other_pending,
                "annual_leave_balances": annual_balances,
                "annual_leave_year": ctx.today.year(),
                "annual_leave_ruleset": ruleset_json(rs),
                "annual_leave_quota_stored": false,
                "annual_leave_note": "已休只計已核准的天數；離職員工的年資停在離職日。",
            }),
            summary,
        ))
    }
}

struct DecideLeaveRequest;

#[derive(Deserialize, JsonSchema)]
pub struct DecideLeaveRequestInput {
    /// Leave request number.
    pub leave_no: String,
    /// `true` approves, `false` rejects.
    pub approve: bool,
    /// Note for the requester.
    #[serde(default)]
    pub note: Option<String>,
}

#[async_trait]
impl Operation for DecideLeaveRequest {
    fn name(&self) -> &'static str {
        "decide_leave_request"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "准假或駁回一張仍在等簽核的休假單，留下決定者、身分與說明。狀態不是待簽核時一律拒絕；單據所在月份到當年年底之間有已計算薪資的期別時不能准假，駁回不受影響；申請人不得核准本人送出的單據，公司設定允許時負責人例外。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<DecideLeaveRequestInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: DecideLeaveRequestInput = parse(input)?;
        let request = ctx
            .store
            .decide_leave_request(
                &i.leave_no,
                i.approve,
                &ctx.actor,
                &ctx.actor_role,
                deciding_as(ctx).await?,
                i.note.as_deref().unwrap_or(""),
            )
            .await
            .map_err(|e| own_request_error(ctx, e))?;

        let action = if i.approve { "准假" } else { "駁回" };
        let summary = format!(
            "{} · {} {} {} {} 日 · 由 {}（{}）{action}",
            request.leave_no,
            request.emp_no,
            request.name,
            leave_kind_label(&request.kind),
            request.days.normalize(),
            ctx.actor,
            crate::role_name(ctx).await
        );
        Ok(OpOutcome::new(
            json!({ "request": leave_json(&request, None), "action": request.status }),
            summary,
        ))
    }
}

struct RevokeLeaveRequest;

#[derive(Deserialize, JsonSchema)]
pub struct RevokeLeaveRequestInput {
    /// Leave request number. Only an approved one can be revoked.
    pub leave_no: String,
    /// Why the approval is undone. Required; the requester reads it.
    pub reason: String,
}

#[async_trait]
impl Operation for RevokeLeaveRequest {
    fn name(&self) -> &'static str {
        "revoke_leave_request"
    }

    fn risk(&self) -> Risk {
        // The inverse of approving, and the same tier.
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "撤銷一張已核准的休假單，理由必填，留下撤銷者與身分；撤銷後該段時間可以重新申請。單據所在月份到當年年底之間有已計算薪資的期別時拒絕；申請人不得撤銷本人的單，公司設定允許時負責人例外。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<RevokeLeaveRequestInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: RevokeLeaveRequestInput = parse(input)?;
        let reason = revoke_reason(&i.reason)?;
        let request = ctx
            .store
            .revoke_leave_request(
                &i.leave_no,
                &ctx.actor,
                &ctx.actor_role,
                deciding_as(ctx).await?,
                reason,
            )
            .await
            .map_err(|e| own_request_error(ctx, e))?;

        let summary = format!(
            "{} · {} {} {} {} 日 · 由 {}（{}）撤銷 · 理由：{reason}",
            request.leave_no,
            request.emp_no,
            request.name,
            leave_kind_label(&request.kind),
            request.days.normalize(),
            ctx.actor,
            crate::role_name(ctx).await
        );
        Ok(OpOutcome::new(
            json!({ "request": leave_json(&request, None) }),
            summary,
        ))
    }
}

// Punch corrections
//
// The employee proposes, and either the company reviews it or the company
// setting lets it through. Until it is approved nothing that reads the
// working day can see it: the row lives in `punch_correction`, and only an
// approval writes an `attendance_punch`.
//
// HR's own `correct_punch` writes the punch straight away, and lands a row
// here already approved. Both directions overwrite whatever the slot held,
// and `replaced_at` is the only place that value survives.

pub(crate) fn punch_correction_status_label(status: &str) -> &'static str {
    match status {
        "submitted" => "待審核",
        "approved" => "已核准",
        _ => "已駁回",
    }
}

pub(crate) fn punch_correction_json(c: &PunchCorrection, offset: FixedOffset) -> Value {
    json!({
        "id": c.id,
        "emp_no": c.emp_no,
        "name": c.name,
        "dept_label": c.dept_label,
        "punched_on": c.punched_on,
        "kind": c.kind,
        "kind_label": punch_kind_label(&c.kind),
        "at": business_time(c.punched_at, offset).format(PUNCH_TIME).to_string(),
        "reason": c.reason,
        "status": c.status,
        "status_label": punch_correction_status_label(&c.status),
        // Empty when the setting let it through, so the screen can say so
        // rather than naming a person who made no decision.
        "decided_by": c.decided_by,
        "decided_role": c.decided_role,
        "decided_at": c.decided_at,
        "decision_note": c.decision_note,
        // Absent when the slot was empty: the amendment added a time rather
        // than changing one.
        "replaced_at": c
            .replaced_at
            .map(|at| business_time(at, offset).format(PUNCH_TIME).to_string()),
        "hr_entered": c.hr_entered,
        "created_by": c.created_by,
        "created_at": c.created_at,
    })
}

/// How a punch-correction refusal names the request, which has no number.
const PUNCH_SUBJECT: &str = "這張補登申請";

/// The store's refusal of a requester points at the account screen, which is
/// not the owner's way out; the message is rebuilt for the caller's role.
fn own_request_error(ctx: &OpCtx, e: laysi_store::StoreError) -> OpError {
    use laysi_store::StoreError;
    match e {
        StoreError::LeaveDecidedByRequester { no } => {
            OpError::BadInput(own_request_refusal(&ctx.actor_role, &format!("{no} ")))
        }
        StoreError::PunchCorrectionDecidedByRequester => {
            OpError::BadInput(own_request_refusal(&ctx.actor_role, PUNCH_SUBJECT))
        }
        StoreError::RevokedByRequester { no } => {
            OpError::BadInput(own_revoke_refusal(&ctx.actor_role, &format!("{no} ")))
        }
        e => e.into(),
    }
}

/// Whether this caller can approve or revoke a request, answered on its list
/// row so the screen draws the button or the reason instead of it. Approving
/// is asked of a pending request the caller may decide, revoking of an
/// approved one. Each reason is the sentence the operation itself would
/// refuse with, built from the same store function.
///
/// Rejecting is not asked: it changes nothing payroll reads, so a computed
/// period leaves it open while it closes approval.
fn payroll_answers(
    ctx: &OpCtx,
    row: &mut Value,
    status: &str,
    own: bool,
    no: &str,
    (first, last): (String, String),
    paid: &[PayrollPeriodState],
) {
    let span = (first.as_str(), last.as_str());
    let refusal = |action| payroll_blocker(paid, span, action).map(|e| e.to_string());
    let approve_blocked = match status {
        "pending" if !own => refusal("核准"),
        _ => None,
    };
    row["can_approve"] = json!(status == "pending" && !own && approve_blocked.is_none());
    row["approve_blocked_reason"] = json!(approve_blocked);

    let revoke_blocked = match status {
        "approved" if own => Some(own_revoke_refusal(&ctx.actor_role, &format!("{no} "))),
        "approved" => refusal("撤銷"),
        _ => None,
    };
    row["can_revoke"] = json!(status == "approved" && revoke_blocked.is_none());
    row["revoke_blocked_reason"] = json!(revoke_blocked);
}

/// The reason a revocation must carry: the requester reads it, and it is the
/// only account of why an approval stopped counting.
fn revoke_reason(reason: &str) -> Result<&str, OpError> {
    match reason.trim() {
        "" => Err(OpError::BadInput("請填寫撤銷理由".into())),
        reason => Ok(reason),
    }
}

/// Why a correction cannot be approved, or `None` when it can.
///
/// `punch_in_tx` refuses a departure on a day with no arrival, so the screen
/// has to know before it draws the button. Rejecting stays open either way:
/// a request that cannot be written may well be the one that should go.
fn approve_blocked(kind: &str, day_has_in: bool) -> Option<&'static str> {
    (kind == "out" && !day_has_in).then_some("當天沒有上班卡，請先核准或補登同一天的上班卡。")
}

/// The two answers to `punch_correction_policy`, in the words the screen
/// prints. Company policy, so the choice is a `company_setting` row and never
/// a constant in a calculation; the values live here so the setting can only
/// be stored as one of the two things it means.
pub(crate) const CORRECTION_AUTO: &str = "自動通過";
pub(crate) const CORRECTION_REVIEW: &str = "人資審核";
pub(crate) const CORRECTION_POLICIES: [&str; 2] = [CORRECTION_REVIEW, CORRECTION_AUTO];

pub(crate) async fn correction_auto_approves(ctx: &OpCtx) -> Result<bool, OpError> {
    Ok(ctx
        .store
        .company_settings()
        .await?
        .iter()
        .find(|s| s.code == "punch_correction_policy")
        .map(|s| s.value.trim() == CORRECTION_AUTO)
        .unwrap_or(false))
}

// Overtime requests

/// The ordinary hourly wage the overtime multipliers apply to.
///
/// An hourly rate is that wage already. Dividing it by the ruleset's monthly
/// hours, as a monthly salary must be, would price an hour of overtime below
/// an hour of ordinary time.
pub(crate) fn ordinary_hourly_wage(
    rs: &laysi_regulation::Ruleset,
    basis: &str,
    base_salary: Money,
) -> Result<Money, laysi_regulation::RegulationError> {
    match basis {
        BASIS_HOURLY => Ok(base_salary),
        _ => laysi_regulation::ordinary_hourly_wage(rs, base_salary),
    }
}

/// One overtime request, with the pay derived from the ruleset in force on
/// the day worked.
///
/// The amount is never stored. It is hours times a multiplier with an
/// effective date, so it is computed on every read for the same reason the
/// withholding tax is: one question, one answer.
pub(crate) fn overtime_json(o: &OvertimeRequest) -> Result<Value, OpError> {
    let rs = laysi_regulation::resolve("overtime", o.worked_on)?;
    let label = laysi_regulation::overtime_day_types(rs)?
        .into_iter()
        .find(|t| t.code == o.day_type)
        .map(|t| t.label)
        .unwrap_or_else(|| o.day_type.clone());
    let pay = (o.base_salary > Money::ZERO)
        .then(|| {
            let wage = ordinary_hourly_wage(rs, &o.pay_basis, o.base_salary)?;
            laysi_regulation::overtime_pay(rs, wage, &o.day_type, o.hours)
        })
        .transpose()?;

    Ok(json!({
        "request_no": o.request_no,
        "emp_no": o.emp_no,
        "name": o.name,
        "dept_label": o.dept_label,
        "day_type": o.day_type,
        "day_type_label": label,
        "from_at": o.from_at,
        "to_at": o.to_at,
        "worked_on": o.worked_on,
        "hours": o.hours.to_string(),
        "pay": pay.map(|p| p.to_string()),
        "pay_blocked_reason": pay.is_none().then_some("尚未設定底薪，設定後才算得出加班費"),
        "reason": o.reason,
        "status": o.status,
        "status_label": overtime_status_label(&o.status),
        "decided_by": o.decided_by,
        "decided_role": o.decided_role,
        "decided_at": o.decided_at,
        "decision_note": o.decision_note,
        "cancelled": o.cancelled,
        "created_at": o.created_at,
        "created_by": o.created_by,
    }))
}

/// This month's approved overtime per person, against the monthly ceiling.
pub(crate) fn overtime_month_totals(
    rows: &[OvertimeRequest],
    ceiling: Money,
) -> Result<Vec<Value>, OpError> {
    let mut totals: BTreeMap<&str, (&OvertimeRequest, Money, Money)> = BTreeMap::new();
    for o in rows.iter().filter(|o| o.status == "approved") {
        let pay = (o.base_salary > Money::ZERO)
            .then(|| {
                let rs = laysi_regulation::resolve("overtime", o.worked_on)?;
                let wage = ordinary_hourly_wage(rs, &o.pay_basis, o.base_salary)?;
                laysi_regulation::overtime_pay(rs, wage, &o.day_type, o.hours)
            })
            .transpose()?
            .unwrap_or(Money::ZERO);
        let entry = totals
            .entry(&o.emp_no)
            .or_insert((o, Money::ZERO, Money::ZERO));
        entry.1 += o.hours;
        entry.2 += pay;
    }
    Ok(totals
        .values()
        .map(|(o, hours, pay)| {
            json!({
                "emp_no": o.emp_no,
                "name": o.name,
                "dept_label": o.dept_label,
                "hours": hours.normalize().to_string(),
                "pay": pay.to_string(),
                "over_ceiling": *hours > ceiling,
            })
        })
        .collect())
}

struct ListOvertimeRequests;

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListOvertimeRequestsInput {
    /// Month as `YYYY-MM`. Omitted reads the current month.
    #[serde(default)]
    pub month: Option<String>,
    /// Include the ones already decided. Omitted shows only what is waiting.
    #[serde(default)]
    pub include_done: Option<bool>,
}

#[async_trait]
impl Operation for ListOvertimeRequests {
    fn name(&self) -> &'static str {
        "list_overtime_requests"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀一個月的加班申請與逐人已核准時數。加班費依加班日適用的加班規則現算，不存欄位；月上限同樣來自規則庫。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<ListOvertimeRequestsInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListOvertimeRequestsInput = parse(input)?;
        let month = i
            .month
            .unwrap_or_else(|| ctx.today.format("%Y-%m").to_string());
        if !valid_month(&month) {
            return Err(OpError::BadInput(format!(
                "月份格式為 YYYY-MM，收到「{month}」"
            )));
        }

        let all = ctx
            .store
            .overtime_requests(true, None, Some(&month))
            .await?;
        let rs = laysi_regulation::resolve("overtime", ctx.today)?;
        let ceiling = laysi_regulation::overtime_monthly_max_hours(rs)?;
        let include_done = i.include_done.unwrap_or(false);
        let deciding_as = deciding_as(ctx).await?;
        let paid = ctx.store.payroll_period_states().await?;

        let rows = all
            .iter()
            .filter(|o| include_done || o.status == "pending")
            .map(|o| {
                let mut row = overtime_json(o)?;
                let own = deciding_as.is_some() && deciding_as == Some(o.employee_id);
                row["can_decide"] = json!(o.status == "pending" && !own);
                row["decide_blocked_reason"] = json!(own.then(|| {
                    own_request_refusal(&ctx.actor_role, &format!("{} ", o.request_no))
                }));
                let span = overtime_payroll_span(o.worked_on);
                payroll_answers(ctx, &mut row, &o.status, own, &o.request_no, span, &paid);
                Ok(row)
            })
            .collect::<Result<Vec<_>, OpError>>()?;

        let pending = all.iter().filter(|o| o.status == "pending").count();
        let other_pending = ctx
            .store
            .overtime_requests(false, None, None)
            .await?
            .len()
            .saturating_sub(pending);

        let summary = format!(
            "讀 {month} 加班申請 · {} 張（{pending} 張等簽）· 加班費依加班規則 {} 現算",
            rows.len(),
            rs.version
        );
        Ok(OpOutcome::new(
            json!({
                "month": month,
                "requests": rows,
                "pending_count": pending,
                "other_month_pending": other_pending,
                "month_totals": overtime_month_totals(&all, ceiling)?,
                "monthly_max_hours": ceiling.normalize().to_string(),
                "day_types": laysi_regulation::overtime_day_types(rs)?
                    .iter()
                    .map(|t| json!({
                        "code": t.code,
                        "label": t.label,
                        "max_hours": t.max_hours.normalize().to_string(),
                    }))
                    .collect::<Vec<_>>(),
                "ruleset": ruleset_json(rs),
            }),
            summary,
        ))
    }
}

struct DecideOvertimeRequest;

#[derive(Deserialize, JsonSchema)]
pub struct DecideOvertimeRequestInput {
    /// Overtime request number.
    pub request_no: String,
    /// `true` approves, `false` rejects.
    pub approve: bool,
    /// Note for the requester.
    #[serde(default)]
    pub note: Option<String>,
}

#[async_trait]
impl Operation for DecideOvertimeRequest {
    fn name(&self) -> &'static str {
        "decide_overtime_request"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "核准或駁回一張仍在等簽核的加班申請，留下決定者、身分與說明。核准後該時數會列入當期薪資的加班費。狀態不是等簽核時一律拒絕；加班日所在期別已計算薪資時不能核准，駁回不受影響；申請人不得核准本人送出的申請，公司設定允許時負責人例外。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<DecideOvertimeRequestInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: DecideOvertimeRequestInput = parse(input)?;
        let request = ctx
            .store
            .decide_overtime_request(
                &i.request_no,
                i.approve,
                &ctx.actor,
                &ctx.actor_role,
                deciding_as(ctx).await?,
                i.note.as_deref().unwrap_or(""),
            )
            .await
            .map_err(|e| own_request_error(ctx, e))?;

        let action = if i.approve { "核准" } else { "駁回" };
        let summary = format!(
            "{} · {} {} 加班 {} 小時 · 由 {}（{}）{action}",
            request.request_no,
            request.emp_no,
            request.name,
            request.hours.normalize(),
            ctx.actor,
            crate::role_name(ctx).await
        );
        Ok(OpOutcome::new(
            json!({
                "request": overtime_json(&request)?,
                "action": action,
            }),
            summary,
        ))
    }
}

struct RevokeOvertimeRequest;

#[derive(Deserialize, JsonSchema)]
pub struct RevokeOvertimeRequestInput {
    /// Overtime request number. Only an approved one can be revoked.
    pub request_no: String,
    /// Why the approval is undone. Required; the requester reads it.
    pub reason: String,
}

#[async_trait]
impl Operation for RevokeOvertimeRequest {
    fn name(&self) -> &'static str {
        "revoke_overtime_request"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "撤銷一張已核准的加班申請，理由必填，留下撤銷者與身分；撤銷後不再列入薪資的加班費，該段時間可以重新申請。加班日所在期別已計算薪資時拒絕；申請人不得撤銷本人的單，公司設定允許時負責人例外。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<RevokeOvertimeRequestInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: RevokeOvertimeRequestInput = parse(input)?;
        let reason = revoke_reason(&i.reason)?;
        let request = ctx
            .store
            .revoke_overtime_request(
                &i.request_no,
                &ctx.actor,
                &ctx.actor_role,
                deciding_as(ctx).await?,
                reason,
            )
            .await
            .map_err(|e| own_request_error(ctx, e))?;

        let summary = format!(
            "{} · {} {} 加班 {} 小時 · 由 {}（{}）撤銷 · 理由：{reason}",
            request.request_no,
            request.emp_no,
            request.name,
            request.hours.normalize(),
            ctx.actor,
            crate::role_name(ctx).await
        );
        Ok(OpOutcome::new(
            json!({ "request": overtime_json(&request)? }),
            summary,
        ))
    }
}

struct ListPunchCorrections;

#[derive(Deserialize, JsonSchema)]
pub struct ListPunchCorrectionsInput {
    /// Include the ones already decided. Omitted shows only what is waiting.
    #[serde(default)]
    pub include_done: Option<bool>,
}

#[async_trait]
impl Operation for ListPunchCorrections {
    fn name(&self) -> &'static str {
        "list_punch_corrections"
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn summary(&self) -> &'static str {
        "讀打卡補登紀錄：申請人、日期與時段、補登時間、被覆蓋的原時刻、理由與目前狀態。核准後才寫入打卡紀錄。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<ListPunchCorrectionsInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: ListPunchCorrectionsInput = parse(input)?;
        let include_done = i.include_done.unwrap_or(false);
        let corrections = ctx.store.punch_corrections(include_done, None).await?;
        let auto = correction_auto_approves(ctx).await?;

        // Whether this identity can decide each request, answered here so the
        // screen draws the reason instead of a button that must be refused.
        let deciding_as = deciding_as(ctx).await?;
        let rows = corrections
            .iter()
            .map(|c| {
                let mut row = punch_correction_json(c, ctx.offset);
                let own = deciding_as.is_some() && deciding_as == Some(c.employee_id);
                row["can_decide"] = json!(c.status == "submitted" && !own);
                row["decide_blocked_reason"] =
                    json!(own.then(|| own_request_refusal(&ctx.actor_role, PUNCH_SUBJECT)));
                let blocked = approve_blocked(&c.kind, c.day_has_in);
                row["can_approve"] = json!(blocked.is_none());
                row["approve_blocked_reason"] = json!(blocked);
                row
            })
            .collect::<Vec<_>>();
        let waiting = corrections
            .iter()
            .filter(|c| c.status == "submitted")
            .count();

        let summary = format!(
            "讀補登紀錄 · {} 筆（{waiting} 張待審核）· {}",
            rows.len(),
            if auto {
                CORRECTION_AUTO
            } else {
                CORRECTION_REVIEW
            }
        );
        Ok(OpOutcome::new(
            json!({
                "corrections": rows,
                "waiting_count": waiting,
                "auto_approve": auto,
                "policy_note": match auto {
                    true => "補登申請自動通過，時間直接寫進打卡紀錄，來源標示為補登。",
                    false => "核准後時間才寫進打卡紀錄，來源標示為補登。",
                },
            }),
            summary,
        ))
    }
}

struct DecidePunchCorrection;

#[derive(Deserialize, JsonSchema)]
pub struct DecidePunchCorrectionInput {
    /// Which request.
    pub id: i32,
    /// True approves and writes the punch; false turns it down.
    pub approve: bool,
    /// What to tell the employee. Kept on the record.
    #[serde(default)]
    pub note: Option<String>,
}

#[async_trait]
impl Operation for DecidePunchCorrection {
    fn name(&self) -> &'static str {
        "decide_punch_correction"
    }

    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }

    fn summary(&self) -> &'static str {
        "核准或駁回一張待審核的補登申請。核准會在同一個交易寫進打卡紀錄，來源記為補登並留下決定者。申請人不得決定本人送出的申請，公司設定允許時負責人例外。"
    }

    fn input_schema(&self) -> Value {
        schema_of::<DecidePunchCorrectionInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: DecidePunchCorrectionInput = parse(input)?;
        let decided = ctx
            .store
            .decide_punch_correction(laysi_store::DecidePunchCorrection {
                id: i.id,
                approve: i.approve,
                actor: &ctx.actor,
                actor_role: &ctx.actor_role,
                actor_employee_id: deciding_as(ctx).await?,
                note: i.note.as_deref().unwrap_or("").trim(),
                offset: ctx.offset,
            })
            .await
            .map_err(|e| own_request_error(ctx, e))?;

        let summary = format!(
            "{} {} 的{}卡補登 {} · {} 決定",
            decided.emp_no,
            decided.punched_on,
            punch_kind_label(&decided.kind),
            punch_correction_status_label(&decided.status),
            ctx.actor
        );
        Ok(OpOutcome::new(
            json!({
                "correction": punch_correction_json(&decided, ctx.offset),
                "punch_written": i.approve,
            }),
            summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::employees::INSURED_EMPLOYEE;
    use std::collections::BTreeSet;

    /// The offset the tests are written against. The numbers below are wall
    /// clock times in it, so it has to be the one the install ships on.
    fn tz() -> FixedOffset {
        laysi_domain::default_business_offset()
    }

    fn punch(day: &str, kind: &str, at: &str, source: &str) -> AttendancePunch {
        let punched_on = NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap();
        let time = NaiveTime::parse_from_str(at, "%H:%M:%S")
            .or_else(|_| NaiveTime::parse_from_str(at, "%H:%M"))
            .unwrap();
        AttendancePunch {
            id: 1,
            employee_id: 1,
            emp_no: "E003".into(),
            name: "黃柏叡".into(),
            dept_label: Some("業務部".into()),
            punched_on,
            punched_at: business_instant(punched_on, time, tz()),
            kind: kind.into(),
            source: source.into(),
            note: String::new(),
            created_by: "黃柏叡".into(),
        }
    }

    fn schedule(start: Option<&str>, end: Option<&str>, grace: i64) -> WorkSchedule {
        flex_schedule(start, end, grace, 0)
    }

    fn flex_schedule(
        start: Option<&str>,
        end: Option<&str>,
        grace: i64,
        flex: i64,
    ) -> WorkSchedule {
        let time = |v: Option<&str>| v.map(|v| NaiveTime::parse_from_str(v, "%H:%M").unwrap());
        WorkSchedule {
            starts_at: time(start),
            ends_at: time(end),
            grace_minutes: grace,
            grace_stated: true,
            flex_minutes: flex,
            break_start: None,
            break_end: None,
            calendar: BTreeMap::new(),
        }
    }

    /// The whole point of the calendar: a public holiday inside a leave range
    /// is not charged against the entitlement, and a 補行上班 Saturday is.
    #[test]
    fn leave_skips_the_days_the_calendar_calls_days_off() {
        let day = |d: &str, t: &str| {
            NaiveDateTime::parse_from_str(&format!("{d}T{t}"), "%Y-%m-%dT%H:%M:%S").unwrap()
        };
        let on = |d: &str| NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap();
        let mut sched = schedule(Some("09:00"), Some("18:00"), 0);

        // 2026-06-18 is a Thursday and 06-19 the Friday after it.
        assert_eq!(
            sched.leave_seconds(day("2026-06-18", "09:00:00"), day("2026-06-19", "18:00:00")),
            Some(18 * 3600),
            "日曆還沒說之前，兩天都是工作日"
        );

        sched
            .calendar
            .insert(on("2026-06-19"), (DayKind::Holiday, "端午節".into()));
        assert_eq!(
            sched.leave_seconds(day("2026-06-18", "09:00:00"), day("2026-06-19", "18:00:00")),
            Some(9 * 3600),
            "跨到國定假日的休假不扣那一天"
        );
        assert_eq!(sched.day_name(on("2026-06-19")), Some("端午節"));

        // 2026-02-21 is a Saturday the government moved back to work.
        assert_eq!(
            sched.leave_seconds(day("2026-02-21", "09:00:00"), day("2026-02-21", "18:00:00")),
            Some(0)
        );
        sched
            .calendar
            .insert(on("2026-02-21"), (DayKind::Workday, "補行上班".into()));
        assert_eq!(
            sched.leave_seconds(day("2026-02-21", "09:00:00"), day("2026-02-21", "18:00:00")),
            Some(9 * 3600),
            "補行上班的星期六請假要扣"
        );
        assert_eq!(sched.overtime_day_type(on("2026-02-21")), "weekday");

        // 2026-10-10 is a Saturday, priced as a rest day either way.
        sched
            .calendar
            .insert(on("2026-10-10"), (DayKind::Holiday, "國慶日".into()));
        let codes: Vec<(String, String)> = sched
            .overtime_exceptions()
            .iter()
            .map(|e| (e["date"].to_string(), e["day_type"].to_string()))
            .collect();
        assert_eq!(
            codes,
            [
                ("\"2026-02-21\"".into(), "\"weekday\"".into()),
                ("\"2026-06-19\"".into(), "\"rest_day\"".into()),
            ],
            "只列出日別與星期幾推得的不同的日期"
        );
    }

    /// Nothing was asked of anyone on a day off, so the whole of the
    /// attendance is beyond the normal day rather than the part past its
    /// length.
    #[test]
    fn attendance_on_a_day_off_is_overtime_from_the_first_second() {
        let day = |d: &str, t: &str| {
            NaiveDateTime::parse_from_str(&format!("{d}T{t}"), "%Y-%m-%dT%H:%M:%S").unwrap()
        };
        let sched = schedule(Some("09:00"), Some("18:00"), 0);

        // 2026-08-21 Friday: four hours is short of the nine-hour day.
        assert_eq!(
            sched.overtime_seconds(
                Some(day("2026-08-21", "09:00:00")),
                day("2026-08-21", "13:00:00")
            ),
            Some(0)
        );
        // 2026-08-22 is the Saturday after it.
        assert_eq!(
            sched.overtime_seconds(
                Some(day("2026-08-22", "09:00:00")),
                day("2026-08-22", "13:00:00")
            ),
            Some(4 * 3600),
            "休息日出勤四小時就是四小時加班"
        );
    }

    /// A leave range is worth the working time inside it and nothing else:
    /// the evening between two days is not leave, the weekend is not leave,
    /// and hours before the day opens or after it closes are not leave.
    #[test]
    fn leave_is_worth_the_working_hours_it_covers() {
        let day = |d: &str, t: &str| {
            NaiveDateTime::parse_from_str(&format!("{d}T{t}"), "%Y-%m-%dT%H:%M:%S").unwrap()
        };
        let sched = schedule(Some("09:00"), Some("18:00"), 0);
        assert_eq!(sched.daily_seconds(), Some(9 * 3600));

        // 2026-08-21 is a Friday, 08-24 the following Monday.
        assert_eq!(
            sched.leave_seconds(day("2026-08-21", "13:30:00"), day("2026-08-21", "18:00:00")),
            Some(4 * 3600 + 1800),
            "一個下午"
        );
        assert_eq!(
            sched.leave_seconds(day("2026-08-21", "07:00:00"), day("2026-08-21", "22:00:00")),
            Some(9 * 3600),
            "上班時間之外的部分不算"
        );
        assert_eq!(
            sched.leave_seconds(day("2026-08-21", "13:00:00"), day("2026-08-24", "12:00:00")),
            Some(5 * 3600 + 3 * 3600),
            "週五下午加週一上午，中間的週末不算"
        );
        assert_eq!(
            sched.leave_seconds(day("2026-08-22", "09:00:00"), day("2026-08-23", "18:00:00")),
            Some(0),
            "整段都在週末"
        );
        assert_eq!(
            schedule(None, None, 0)
                .leave_seconds(day("2026-08-21", "09:00:00"), day("2026-08-21", "18:00:00")),
            None,
            "沒有上下班時間就算不出時數"
        );
    }

    /// Grace forgives, flexitime moves. The same 09:45 arrival is late under
    /// an hour of grace and on time under an hour of flexitime, and only
    /// flexitime changes when the day is expected to end.
    #[test]
    fn flexitime_moves_the_day_where_grace_only_forgives() {
        let at = |t: &str| business_time(punch("2026-08-06", "in", t, "self").punched_at, tz());
        let day = NaiveDate::from_ymd_opt(2026, 8, 6).unwrap();
        let arrived = at("09:45:00");

        let grace = schedule(Some("09:00"), Some("18:00"), 60);
        assert_eq!(grace.late(arrived), Some(false), "一小時寬限內不算遲到");
        assert_eq!(
            grace.expected_end(day, Some(arrived)),
            Some(day.and_hms_opt(18, 0, 0).unwrap()),
            "寬限不改下班時間"
        );

        let flex = flex_schedule(Some("09:00"), Some("18:00"), 0, 60);
        assert_eq!(flex.late(arrived), Some(false));
        assert_eq!(
            flex.expected_end(day, Some(arrived)),
            Some(day.and_hms_opt(18, 45, 0).unwrap()),
            "彈性把整天往後移，時數不變"
        );
        assert_eq!(
            flex.latest_start().map(|t| t.to_string()),
            Some("10:00:00".into())
        );
    }

    fn limits() -> OvertimeLimits {
        OvertimeLimits::resolve(NaiveDate::from_ymd_opt(2026, 8, 6).unwrap()).unwrap()
    }

    /// A punch record is what an inspection reads, so overtime the law does
    /// not allow has to be marked where the hours are printed. The maxima are
    /// the ruleset's: change the weekday one and the same day stops failing.
    #[test]
    fn a_day_past_the_statutory_maximum_is_counted_on_the_summary() {
        let sched = schedule(Some("09:00"), Some("18:00"), 0);
        // 2026-08-06 is a Thursday, so four hours of extended time are allowed.
        let over = punch_days(
            &[
                punch("2026-08-06", "in", "09:00", "self"),
                punch("2026-08-06", "out", "23:00", "self"),
            ],
            tz(),
        );
        let stats = summarise(&over, &sched, &limits());
        assert_eq!(stats.overtime_seconds, Some(5 * 3600));
        assert_eq!(stats.over_limit_days, 1, "平日加班五小時超過單日上限");
        assert!(!stats.over_monthly_limit, "五小時還不到月上限");

        let exact = punch_days(
            &[
                punch("2026-08-06", "in", "09:00", "self"),
                punch("2026-08-06", "out", "22:00", "self"),
            ],
            tz(),
        );
        assert_eq!(
            summarise(&exact, &sched, &limits()).over_limit_days,
            0,
            "剛好做滿上限不算超過"
        );
    }

    fn break_schedule(start: &str, end: &str, pause: (&str, &str)) -> WorkSchedule {
        let time = |v: &str| NaiveTime::parse_from_str(v, "%H:%M").unwrap();
        WorkSchedule {
            break_start: Some(time(pause.0)),
            break_end: Some(time(pause.1)),
            ..schedule(Some(start), Some(end), 0)
        }
    }

    /// The window caps lateness in one direction only, and arriving early
    /// still never moves the expected end. The hours are a separate question:
    /// attendance before the day opens is attendance, and what comes out of it
    /// is the stated break, not the part of the day nobody asked for.
    #[test]
    fn the_flex_window_caps_lateness_and_attendance_carries_the_hours() {
        let at = |t: &str| business_time(punch("2026-08-06", "in", t, "self").punched_at, tz());
        let day = NaiveDate::from_ymd_opt(2026, 8, 6).unwrap();
        let flex = flex_schedule(Some("09:00"), Some("18:00"), 5, 60);

        assert_eq!(flex.late(at("10:05:00")), Some(false), "區間加寬限的邊界");
        assert_eq!(flex.late(at("10:05:01")), Some(true));

        let early = at("08:00:00");
        assert_eq!(
            flex.expected_end(day, Some(early)),
            Some(day.and_hms_opt(18, 0, 0).unwrap()),
            "早到不會提早下班"
        );
        assert_eq!(
            flex.overtime_seconds(Some(early), at("18:00:00")),
            Some(3600),
            "八點到、十八點走，比九小時的正常工時多做一小時"
        );

        let late_start = at("10:00:00");
        assert_eq!(
            flex.overtime_seconds(Some(late_start), at("19:00:00")),
            Some(0),
            "10 點到、19 點走，剛好做滿，不是加班"
        );
        assert_eq!(
            flex.overtime_seconds(Some(late_start), at("19:30:00")),
            Some(30 * 60)
        );
    }

    /// The break comes out of both sides of the comparison: out of the day it
    /// is the length of, and out of the attendance that is measured against
    /// it. A day worked exactly as agreed is therefore never overtime, and the
    /// hour taken for lunch is never worked hours.
    #[test]
    fn the_break_leaves_both_the_working_day_and_the_hours_worked() {
        let at = |t: &str| business_time(punch("2026-08-06", "in", t, "self").punched_at, tz());
        let sched = break_schedule("09:00", "18:00", ("12:00", "13:00"));
        assert_eq!(sched.daily_seconds(), Some(8 * 3600));

        assert_eq!(
            sched.overtime_seconds(Some(at("09:00:00")), at("18:00:00")),
            Some(0),
            "照上下班時間出勤不是加班"
        );
        assert_eq!(
            sched.overtime_seconds(Some(at("09:00:00")), at("19:30:00")),
            Some(90 * 60)
        );
        assert_eq!(
            sched.overtime_seconds(Some(at("08:00:00")), at("18:00:00")),
            Some(3600),
            "早到的一小時算，中午那一小時不算"
        );

        let days = punch_days(
            &[
                punch("2026-08-06", "in", "09:00:00", "self"),
                punch("2026-08-06", "out", "18:00:00", "self"),
            ],
            tz(),
        );
        assert_eq!(
            days[0].worked_seconds(&sched),
            Some(8 * 3600),
            "工作時數不含休息"
        );
        assert_eq!(
            sched.leave_seconds(at("09:00:00"), at("18:00:00")),
            Some(8 * 3600),
            "有休息時間，整天假就是八小時"
        );
    }

    /// A punch is stored as an instant. Reading it back must give the time
    /// the person saw on the wall, not the time on a UTC server clock, and
    /// the second must survive both directions.
    #[test]
    fn a_punch_reads_back_at_the_time_it_was_made() {
        let day = punch("2026-08-06", "in", "08:55:37", "self");
        assert_eq!(
            business_time(day.punched_at, tz())
                .format(PUNCH_TIME)
                .to_string(),
            "08:55:37"
        );
        assert_eq!(
            day.punched_at.format(PUNCH_TIME).to_string(),
            "00:55:37",
            "存的是 UTC 瞬間"
        );
    }

    /// The record keeps seconds, so the second decides a borderline arrival
    /// rather than being rounded away.
    #[test]
    fn the_second_decides_a_borderline_arrival() {
        let sched = schedule(Some("09:00"), Some("18:00"), 5);
        let at = |t: &str| business_time(punch("2026-08-06", "in", t, "self").punched_at, tz());
        assert_eq!(
            sched.late(at("09:05:00")),
            Some(false),
            "剛好等於寬限不算遲到"
        );
        assert_eq!(sched.late(at("09:05:01")), Some(true), "多一秒就是遲到");
        assert_eq!(sched.late_by_seconds(at("09:05:01")), Some(1));
        assert_eq!(sched.late_by_seconds(at("09:00:00")), Some(0));

        let days = punch_days(
            &[
                punch("2026-08-06", "in", "09:00:00", "self"),
                punch("2026-08-06", "out", "18:00:30", "self"),
            ],
            tz(),
        );
        assert_eq!(days[0].worked_seconds(&sched), Some(9 * 3600 + 30));
        assert_eq!(
            sched.overtime_seconds(days[0].arrived, days[0].left.unwrap()),
            Some(30)
        );
    }

    /// Lateness is a question the system refuses to answer until the company
    /// has said when the day starts. `None` is not `false`.
    #[test]
    fn a_departure_cannot_be_approved_onto_a_day_with_no_arrival() {
        assert!(approve_blocked("out", false).is_some());
        assert_eq!(approve_blocked("out", true), None);
        // An arrival stands on its own, whichever way the day looks.
        assert_eq!(approve_blocked("in", false), None);
        assert_eq!(approve_blocked("in", true), None);
    }

    #[test]
    fn lateness_is_unanswered_rather_than_false_without_a_start_time() {
        let arrived = business_time(punch("2026-08-06", "in", "10:30", "self").punched_at, tz());
        assert_eq!(schedule(None, None, 0).late(arrived), None);
        assert_eq!(schedule(Some("09:00"), None, 0).late(arrived), Some(true));
    }

    /// The grace period comes from company settings, so the same arrival is
    /// late at one company and not at another with no change here.
    #[test]
    fn the_grace_period_decides_a_borderline_arrival() {
        let arrived = business_time(punch("2026-08-06", "in", "09:05", "self").punched_at, tz());
        assert_eq!(schedule(Some("09:00"), None, 0).late(arrived), Some(true));
        assert_eq!(
            schedule(Some("09:00"), None, 5).late(arrived),
            Some(false),
            "剛好等於寬限不算遲到"
        );
        assert_eq!(schedule(Some("09:00"), None, 10).late(arrived), Some(false));
    }

    /// Days are built from the punch rows, and a day with only an arrival
    /// stays open rather than counting as a worked day.
    #[test]
    fn an_unfinished_day_is_open_not_a_worked_day() {
        let sched = schedule(Some("09:00"), Some("18:00"), 5);
        let days = punch_days(
            &[
                punch("2026-08-05", "in", "08:58", "self"),
                punch("2026-08-05", "out", "19:30", "self"),
                punch("2026-08-06", "in", "09:20", "self"),
            ],
            tz(),
        );
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].worked_seconds(&sched), Some(632 * 60));
        assert!(!days[0].open());
        assert_eq!(days[1].worked_seconds(&sched), None);
        assert!(days[1].open());

        let stats = summarise(&days, &sched, &limits());
        assert_eq!(stats.days_with_arrival, 2);
        assert_eq!(stats.complete_days, 1, "只打了上班的那天不算工作日");
        assert_eq!(stats.open_days, 1);
        assert_eq!(stats.late_days, Some(1), "9:20 超過 9:00 加五分鐘");
        assert_eq!(
            stats.overtime_seconds,
            Some(92 * 60),
            "8:58 到 19:30 共 632 分鐘，扣掉九小時的正常工時"
        );
    }

    /// A day nobody clocked in on still needs a row: it is the day a
    /// correction is asked for. The range stops at employment and at today,
    /// and a punch outside that range is kept rather than dropped.
    #[test]
    fn a_month_lists_every_day_not_only_the_days_with_punches() {
        let day = |text: &str| NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap();
        let punched = punch_days(
            &[
                punch("2026-08-01", "in", "09:00", "self"),
                punch("2026-08-05", "in", "09:00", "self"),
                punch("2026-08-05", "out", "18:00", "self"),
            ],
            tz(),
        );
        let days = month_days(punched, "2026-08", day("2026-08-03"), day("2026-08-07"));

        let dates: Vec<String> = days.iter().map(|d| d.date.to_string()).collect();
        assert_eq!(
            dates,
            [
                "2026-08-01",
                "2026-08-03",
                "2026-08-04",
                "2026-08-05",
                "2026-08-06",
                "2026-08-07"
            ],
            "到職前的打卡留著，到職日到今天之間每天都有一列，今天之後沒有"
        );
        let fifth = days.iter().find(|d| d.date == day("2026-08-05")).unwrap();
        assert_eq!(
            fifth.worked_seconds(&schedule(Some("09:00"), Some("18:00"), 0)),
            Some(9 * 3600)
        );
        let sixth = days.iter().find(|d| d.date == day("2026-08-06")).unwrap();
        assert!(sixth.arrived.is_none() && sixth.left.is_none());
        assert!(!sixth.open(), "沒有上班卡的一天不是未打下班");

        let stats = summarise(&days, &schedule(Some("09:00"), Some("18:00"), 0), &limits());
        assert_eq!(stats.complete_days, 1, "空白的日子不算工作日");
        assert_eq!(stats.open_days, 1, "只有 08-01 那天是未打下班");
    }

    /// Every value in the `leave_request.kind` CHECK needs its own label. If a
    /// kind is added to the schema and this does not follow, the screen quietly
    /// shows "other", so the two sets are compared verbatim.
    #[test]
    fn every_leave_kind_in_the_database_check_has_a_label() {
        let check = include_str!("../../../../migrations/0001_initial.sql")
            .split_once("CONSTRAINT leave_request_kind_check")
            .expect("0001 缺少 leave_request.kind 的 CHECK")
            .1
            .split_once("])))")
            .expect("leave_request.kind CHECK 沒有結尾")
            .0;
        let migration_kinds: BTreeSet<&str> = check.split('\'').skip(1).step_by(2).collect();
        let labelled_kinds: BTreeSet<&str> = LEAVE_KINDS.iter().copied().collect();

        assert_eq!(
            migration_kinds, labelled_kinds,
            "資料庫合法假別與操作層標籤集合不一致"
        );
        for kind in migration_kinds {
            assert_ne!(
                leave_kind_label(kind),
                "其他",
                "{kind} 是資料庫合法值，不能顯示成其他"
            );
        }
    }

    /// Leave entitlement comes only from the ruleset payload. Replace the
    /// payload for a band and the same service length must give a different
    /// answer; the operation layer may not hide a default entitlement.
    #[test]
    fn annual_leave_entitlement_changes_with_the_ruleset_payload() {
        let rs = laysi_regulation::all()
            .iter()
            .find(|ruleset| ruleset.domain == "annual_leave")
            .expect("規則庫缺少 annual_leave");
        let tiers = rs
            .payload
            .get("tiers")
            .and_then(Value::as_array)
            .expect("annual_leave payload 缺少 tiers");
        let first = tiers.first().expect("annual_leave tiers 是空的");
        let months = first
            .get("months")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<u32>().ok())
            .expect("第一個 tiers.months 不是整數字串");
        let original_days = first
            .get("days")
            .and_then(Value::as_str)
            .expect("第一個 tiers.days 不是字串");
        let replacement = tiers
            .iter()
            .skip(1)
            .filter_map(|tier| tier.get("days").and_then(Value::as_str))
            .find(|days| *days != original_days)
            .expect("需要另一個不同的 tiers.days 才能驗證 payload 可替換");

        let before = annual_entitlement_for_service(rs, months).unwrap();
        let mut changed = rs.clone();
        changed.payload["tiers"][0]["days"] = Value::String(replacement.to_string());
        let after = annual_entitlement_for_service(&changed, months).unwrap();

        assert_ne!(before, after);
    }

    fn leave(no: &str, kind: &str, status: &str, from: &str, to: &str) -> LeaveRequest {
        let at = |v: &str| NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M").unwrap();
        let (from_at, to_at) = (at(from), at(to));
        LeaveRequest {
            leave_no: no.into(),
            employee_id: 1,
            emp_no: "E003".into(),
            name: "黃柏叡".into(),
            dept_label: None,
            kind: kind.into(),
            from_at,
            to_at,
            from_on: from_at.date(),
            to_on: to_at.date(),
            hours: Money::ZERO,
            days: Money::ZERO,
            reason: String::new(),
            status: status.into(),
            decided_by: String::new(),
            decided_role: String::new(),
            decided_at: None,
            decision_note: String::new(),
            cancelled: None,
            created_at: Utc::now(),
            created_by: String::new(),
        }
    }

    /// Absence is a working day that is over, with neither a punch nor a
    /// leave request that has not been turned down.
    #[test]
    fn a_missed_day_is_a_working_day_with_nothing_on_file() {
        let on = |d: &str| NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap();
        let mut sched = schedule(Some("09:00"), Some("17:00"), 0);
        sched.break_start = NaiveTime::from_hms_opt(12, 0, 0);
        sched.break_end = NaiveTime::from_hms_opt(13, 0, 0);
        sched
            .calendar
            .insert(on("2026-06-19"), (DayKind::Holiday, "端午節".into()));
        let punched = punch_days(
            &[
                punch("2026-06-15", "in", "08:55", "self"),
                punch("2026-06-15", "out", "17:05", "self"),
            ],
            tz(),
        );
        let requests = [
            leave(
                "LV1",
                "personal",
                "approved",
                "2026-06-17T13:00",
                "2026-06-17T17:00",
            ),
            leave(
                "LV2",
                "sick",
                "pending",
                "2026-06-18T09:00",
                "2026-06-18T17:00",
            ),
            leave(
                "LV3",
                "sick",
                "rejected",
                "2026-06-16T09:00",
                "2026-06-16T17:00",
            ),
            leave(
                "LV6",
                "personal",
                "withdrawn",
                "2026-06-16T09:00",
                "2026-06-16T12:00",
            ),
            leave(
                "LV7",
                "annual",
                "revoked",
                "2026-06-16T13:00",
                "2026-06-16T17:00",
            ),
            leave(
                "LV4",
                "annual",
                "approved",
                "2026-06-23T13:00",
                "2026-06-24T12:00",
            ),
            leave(
                "LV5",
                "annual",
                "approved",
                "2026-06-19T09:00",
                "2026-06-21T17:00",
            ),
        ];
        let mut days = month_days(punched, "2026-06", on("2026-06-15"), on("2026-06-24"));
        let requests = requests.iter().collect::<Vec<_>>();
        let absent = |days: &[PunchDay]| -> Vec<String> {
            days.iter()
                .filter(|d| d.absent)
                .map(|d| d.date.to_string())
                .collect()
        };
        mark_days(&mut days, &requests, &[], &sched, true, on("2026-06-24"));

        // 06-16 has only requests that gave their hours back (rejected,
        // withdrawn, revoked) and 06-22 nothing at all; the
        // holiday, the weekend and today are not missed days.
        assert_eq!(absent(&days), ["2026-06-16", "2026-06-22"]);

        // A correction under review is the person saying they were there.
        mark_days(
            &mut days,
            &requests,
            &[on("2026-06-22")],
            &sched,
            true,
            on("2026-06-24"),
        );
        assert_eq!(absent(&days), ["2026-06-16"]);
        let hours_on = |d: &str| {
            days.iter()
                .find(|day| day.date == on(d))
                .unwrap()
                .leaves
                .iter()
                .map(|l| (l.leave_no.clone(), l.seconds))
                .collect::<Vec<_>>()
        };
        assert_eq!(hours_on("2026-06-17"), [("LV1".into(), Some(4 * 3600))]);
        assert_eq!(hours_on("2026-06-18"), [("LV2".into(), Some(7 * 3600))]);
        assert_eq!(hours_on("2026-06-23"), [("LV4".into(), Some(4 * 3600))]);
        assert_eq!(hours_on("2026-06-24"), [("LV4".into(), Some(3 * 3600))]);
        assert!(
            hours_on("2026-06-16").is_empty(),
            "駁回、撤回與撤銷的單不算請假"
        );
        for off in ["2026-06-19", "2026-06-20", "2026-06-21"] {
            assert!(hours_on(off).is_empty(), "{off} 不是工作日，休假單不佔那天");
        }

        mark_days(&mut days, &[], &[], &sched, false, on("2026-06-24"));
        assert!(days.iter().all(|d| !d.absent), "不判定時一天都不算曠職");
    }

    #[test]
    fn absence_is_not_judged_without_a_start_time_for_part_timers_or_owners() {
        let office = schedule(Some("09:00"), Some("17:00"), 0);
        assert!(judges_absence(&office, "full_time", INSURED_EMPLOYEE));
        assert!(judges_absence(&office, "contract", INSURED_EMPLOYEE));
        assert!(!judges_absence(&office, "part_time", INSURED_EMPLOYEE));
        assert!(!judges_absence(&office, "full_time", INSURED_OWNER));
        let no_start = schedule(None, Some("17:00"), 0);
        assert!(!judges_absence(&no_start, "full_time", INSURED_EMPLOYEE));
    }

    /// The label a screen prints is the kind of day, not the overtime price:
    /// a Sunday is a regular leave day and a holiday is named.
    #[test]
    fn a_day_is_labelled_by_its_kind_and_named_by_the_calendar() {
        let on = |d: &str| NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap();
        let mut sched = schedule(Some("09:00"), Some("17:00"), 0);
        sched
            .calendar
            .insert(on("2026-06-19"), (DayKind::Holiday, "端午節".into()));
        let limits = OvertimeLimits::resolve(on("2026-06-19")).unwrap();
        let json_of = |d: &str| {
            PunchDay {
                date: on(d),
                ..Default::default()
            }
            .json(&sched, &limits)
        };
        assert_eq!(json_of("2026-06-21")["day_type_label"], "例假");
        assert_eq!(json_of("2026-06-20")["day_type_label"], "休息日");
        assert_eq!(json_of("2026-06-19")["day_type_label"], "國定假日");
        assert_eq!(json_of("2026-06-19")["day_name"], "端午節");
        assert_eq!(json_of("2026-06-19")["works"], false);
        assert_eq!(json_of("2026-06-18")["works"], true);
    }
}
