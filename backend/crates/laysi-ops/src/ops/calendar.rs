//! The work calendar: which dates are worked, and which kind of day off the
//! rest are.
//!
//! Only the dates that differ from the default are stored (Monday to Friday
//! work, Saturday a rest day, Sunday a regular leave day), which is exactly
//! what the published calendar adds: 國定假日, 調整放假 and 補行上班. A year
//! is around twenty rows, so [`WorkSchedule`] carries the whole table and
//! every date question is answered without another query.
//!
//! The calendar is read from what the government publishes rather than
//! shipped as a copy: the dates move every year and one invented here would be
//! wrong in a way nobody could see. [`RefreshWorkCalendar`] fetches it from
//! the open-data platform, and [`seed`] does the same once on the first boot
//! that finds the table empty, so a new installation has a calendar before
//! anyone thinks to ask for one. Both write `government` rows and leave
//! `company` rows alone, so a day the company decided for itself survives.
//!
//! 颱風假 is not in that file and never will be: the office calendar is
//! published a year ahead and a 停止上班 announcement comes the evening
//! before. It is set by hand, as `natural_disaster` -- not as a public
//! holiday, because the Labor Standards Act grants it none of the same
//! things.

use super::attendance::{
    default_day_kind, holds_hours, leave_by_date, leave_kind_label, leave_status_label, DayKind,
    WorkSchedule, DAY_KINDS,
};
use crate::{parse, schema_of, Channel, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveTime};
use laysi_domain::Money;
use laysi_store::LeaveRequest;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Operations provided by this module.
pub fn ops() -> Vec<Box<dyn Operation>> {
    vec![
        Box::new(ListWorkCalendar),
        Box::new(SetWorkCalendarDay),
        Box::new(RefreshWorkCalendar),
    ]
}

const WEEKDAY_LABELS: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];

#[derive(Deserialize, JsonSchema)]
pub struct YearInput {
    /// Calendar year (西元). Defaults to this year.
    #[serde(default)]
    pub year: Option<i32>,
}

struct ListWorkCalendar;

#[async_trait]
impl Operation for ListWorkCalendar {
    fn name(&self) -> &'static str {
        "list_work_calendar"
    }
    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }
    fn summary(&self) -> &'static str {
        "讀工作日曆：一年逐日的工作日、休息日、例假、國定假日與天災停班，含節日名稱與來源"
    }
    fn input_schema(&self) -> Value {
        schema_of::<YearInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: YearInput = parse(input)?;
        let year = i.year.unwrap_or_else(|| ctx.today.year());
        let schedule = WorkSchedule::load(ctx).await?;
        let stated: std::collections::BTreeMap<NaiveDate, String> = ctx
            .store
            .work_calendar()
            .await?
            .into_iter()
            .map(|d| (d.on_date, d.source))
            .collect();

        let mut months = Vec::with_capacity(12);
        let mut workdays = 0;
        let mut holidays = 0;
        for month in 1..=12u32 {
            let first = NaiveDate::from_ymd_opt(year, month, 1)
                .ok_or_else(|| OpError::BadInput(format!("{year} 不是有效的年份。")))?;
            let mut days = Vec::new();
            let mut day = first;
            while day.month() == month {
                let kind = schedule.day_kind(day);
                if kind.works() {
                    workdays += 1;
                }
                if kind == DayKind::Holiday {
                    holidays += 1;
                }
                days.push(json!({
                    "on_date": day,
                    "weekday": WEEKDAY_LABELS[day.weekday().num_days_from_monday() as usize],
                    "kind": kind.code(),
                    "kind_label": kind.label(),
                    "name": schedule.day_name(day),
                    "source": stated.get(&day),
                    "default": kind == default_day_kind(day),
                }));
                let Some(next) = day.succ_opt() else { break };
                day = next;
            }
            months.push(json!({ "month": month, "days": days }));
        }

        let summary =
            format!("讀取 {year} 年工作日曆 · 工作日 {workdays} 天 · 國定假日 {holidays} 天");
        Ok(OpOutcome::new(
            json!({
                "year": year,
                "months": months,
                "workdays": workdays,
                "holidays": holidays,
                "kinds": DAY_KINDS
                    .iter()
                    .map(|k| json!({ "code": k.code(), "label": k.label() }))
                    .collect::<Vec<_>>(),
            }),
            summary,
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct SetWorkCalendarDayInput {
    /// The date to state (YYYY-MM-DD).
    pub on_date: NaiveDate,
    /// `workday`, `rest_day`, `regular_leave`, `holiday` or
    /// `natural_disaster`. Empty puts the date back to what the day of the
    /// week says.
    pub kind: String,
    /// What the day is called, for example 「中秋節」.
    #[serde(default)]
    pub name: Option<String>,
}

struct SetWorkCalendarDay;

#[async_trait]
impl Operation for SetWorkCalendarDay {
    fn name(&self) -> &'static str {
        "set_work_calendar_day"
    }
    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "改工作日曆一天：指定該日的日別，颱風停班亦由此登記；留空則回到星期決定的日別"
    }
    fn input_schema(&self) -> Value {
        schema_of::<SetWorkCalendarDayInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: SetWorkCalendarDayInput = parse(input)?;
        let date = i.on_date;
        // Requests are weighed against the calendar this call is about to
        // write, before writing it, so a failure there changes nothing.
        let mut after = WorkSchedule::load(ctx).await?;
        if i.kind.trim().is_empty() {
            let kind = default_day_kind(date);
            after.calendar.remove(&date);
            let overlaps = leave_overlaps(ctx, &after, &[date]).await?;
            let cleared = ctx.store.clear_work_calendar_day(date).await?;
            let summary = format!(
                "工作日曆 {date} 回到{}{}",
                kind.label(),
                overlap_note(&overlaps)
            );
            return Ok(OpOutcome::new(
                json!({
                    "on_date": date, "kind": kind.code(), "kind_label": kind.label(),
                    "default": true, "changed": cleared, "leave_overlaps": overlaps,
                }),
                summary,
            ));
        }

        let kind = DayKind::from_code(i.kind.trim()).ok_or_else(|| {
            let names = DAY_KINDS
                .iter()
                .map(|k| k.label())
                .collect::<Vec<_>>()
                .join("、");
            OpError::BadInput(format!("日別要填{names}其中一項"))
        })?;
        let name = i.name.unwrap_or_default().trim().to_string();
        after.calendar.insert(date, (kind, name.clone()));
        let overlaps = leave_overlaps(ctx, &after, &[date]).await?;
        ctx.store
            .set_work_calendar_day(date, kind.code(), &name, "company", &ctx.actor)
            .await?;

        let summary = format!(
            "工作日曆 {date} 設為{}{}",
            kind.label(),
            overlap_note(&overlaps)
        );
        Ok(OpOutcome::new(
            json!({
                "on_date": date, "kind": kind.code(), "kind_label": kind.label(),
                "name": name, "source": "company", "default": false, "changed": true,
                "leave_overlaps": overlaps,
            }),
            summary,
        ))
    }
}

/// Leave requests that disagree with `schedule`'s calendar on one of `dates`,
/// for whoever is changing it.
///
/// A request keeps the dates and hours it was sent with: a date declared a
/// day off afterwards still counts as leave, and a date made a working day
/// afterwards is not covered by it. Whether the day is given back is for the
/// company and the employee to settle, and the way to do it is to revoke the
/// request and send it again; this list is how HR learns whom to ask. A
/// request is listed for a date when one side counts working time there and
/// the other does not.
///
/// `None` when the caller could not read leave requests itself: the kinds of
/// leave people took are not part of maintaining a calendar.
async fn leave_overlaps(
    ctx: &OpCtx,
    schedule: &WorkSchedule,
    dates: &[NaiveDate],
) -> Result<Option<Vec<Value>>, OpError> {
    const READ_LEAVE: &str = "list_leave_requests";
    if !ctx
        .store
        .permitted_ops(&ctx.actor_role)
        .await?
        .contains(READ_LEAVE)
        || (ctx.channel == Channel::Mcp && ctx.store.tool_enabled(READ_LEAVE).await? != Some(true))
    {
        return Ok(None);
    }
    let (Some(first), Some(last)) = (dates.iter().min(), dates.iter().max()) else {
        return Ok(Some(Vec::new()));
    };
    let requests = ctx
        .store
        .leave_requests_matching(true, None, Some((*first, *last)))
        .await?;

    let mut out = Vec::new();
    for &date in dates {
        for l in requests.iter().filter(|l| holds_hours(&l.status)) {
            let Some((counted, calendar)) = disagreement(schedule, l, date) else {
                continue;
            };
            out.push(json!({
                "on_date": date,
                "leave_no": l.leave_no,
                "emp_no": l.emp_no,
                "name": l.name,
                "kind_label": leave_kind_label(&l.kind),
                "status": l.status,
                "status_label": leave_status_label(&l.status),
                "from_at": l.from_at,
                "to_at": l.to_at,
                "hours": counted.round_dp(1).normalize().to_string(),
                "calendar_hours": (Money::from(calendar) / Money::from(3600))
                    .round_dp(1)
                    .normalize()
                    .to_string(),
            }));
        }
    }
    Ok(Some(out))
}

/// The hours `l` counts on `date` and the working seconds the calendar now
/// gives that part of it, when exactly one of the two is nothing.
fn disagreement(
    schedule: &WorkSchedule,
    l: &LeaveRequest,
    date: NaiveDate,
) -> Option<(Money, i64)> {
    let open = date.and_time(NaiveTime::MIN);
    let close = open + chrono::Duration::days(1);
    if l.from_at >= close || l.to_at <= open {
        return None;
    }
    let counted = leave_by_date(l)
        .into_iter()
        .find(|p| p.date == date)
        .map_or(Money::ZERO, |p| p.hours);
    let calendar = match schedule.day_kind(date).works() {
        true => schedule
            .leave_seconds(l.from_at.max(open), l.to_at.min(close))
            .unwrap_or(0),
        false => 0,
    };
    (counted.is_zero() != (calendar == 0)).then_some((counted, calendar))
}

/// The tail the audit summary gets when a change touches leave requests.
fn overlap_note(overlaps: &Option<Vec<Value>>) -> String {
    let requests: std::collections::BTreeSet<&str> = overlaps
        .iter()
        .flatten()
        .filter_map(|o| o["leave_no"].as_str())
        .collect();
    match requests.len() {
        0 => String::new(),
        n => format!(" · 與 {n} 張休假單不一致"),
    }
}

/// One row of the published file, once the columns have been found.
struct GovRow {
    date: NaiveDate,
    off: bool,
    note: String,
}

/// Read the government calendar CSV.
///
/// The published columns are 西元日期／星期／是否放假／備註, where 是否放假 is
/// `2` for a day off and `0` for a working day; the English headers of the
/// open-data variant name the same three things. Columns are found by header
/// rather than by position, because the order has moved between years.
fn parse_gov_calendar(text: &str) -> Result<Vec<GovRow>, OpError> {
    let cell = |line: &str| -> Vec<String> {
        line.split(',')
            .map(|c| c.trim().trim_matches('"').to_string())
            .collect()
    };
    let mut lines = text
        .trim_start_matches('\u{feff}')
        .lines()
        .filter(|l| !l.trim().is_empty());
    let header = cell(
        lines
            .next()
            .ok_or_else(|| OpError::BadInput("檔案是空的。".into()))?,
    );
    let find = |names: &[&str]| header.iter().position(|h| names.contains(&h.as_str()));
    let date_col = find(&["西元日期", "date", "Date"])
        .ok_or_else(|| OpError::BadInput("查無日期欄位，請確認檔案是政府行事曆的 CSV。".into()))?;
    let off_col = find(&["是否放假", "isHoliday", "IsHoliday"])
        .ok_or_else(|| OpError::BadInput("查無放假欄位，請確認檔案是政府行事曆的 CSV。".into()))?;
    let note_col = find(&["備註", "description", "Description", "holidayCategory"]);

    let mut out = Vec::new();
    for line in lines {
        let cells = cell(line);
        let Some(raw) = cells.get(date_col) else {
            continue;
        };
        let date = NaiveDate::parse_from_str(raw, "%Y%m%d")
            .or_else(|_| NaiveDate::parse_from_str(raw, "%Y-%m-%d"))
            .or_else(|_| NaiveDate::parse_from_str(raw, "%Y/%m/%d"));
        let Ok(date) = date else { continue };
        let flag = cells.get(off_col).map(String::as_str).unwrap_or("");
        out.push(GovRow {
            date,
            off: matches!(flag, "2" | "1" | "是" | "true" | "True"),
            note: note_col
                .and_then(|c| cells.get(c))
                .map(|s| s.to_string())
                .unwrap_or_default(),
        });
    }
    if out.is_empty() {
        return Err(OpError::BadInput("檔案裡沒有可讀的日期。".into()));
    }
    Ok(out)
}

/// What one published row means, or `None` when it agrees with the day of the
/// week and so needs no row of its own.
fn deviation(row: &GovRow) -> Option<(DayKind, String)> {
    let default = default_day_kind(row.date);
    let kind = if row.off {
        // A day off with a name is a public holiday; one without is the week
        // off the calendar was going to give anyway.
        if row.note.is_empty() && !default.works() {
            return None;
        }
        DayKind::Holiday
    } else {
        DayKind::Workday
    };
    (kind != default || !row.note.is_empty()).then(|| (kind, row.note.clone()))
}

/// The office calendar on the government open-data platform. One CSV per ROC
/// year, plus a Google Calendar variant of each that carries no 是否放假
/// column. Corrections are appended rather than replacing the row they
/// correct, so the last file for a year is the current one.
const DATASET_URL: &str = "https://data.gov.tw/api/v2/rest/dataset/14718";

/// Years are counted from 1911 in the file descriptions.
const ROC_EPOCH: i32 = 1911;

#[derive(Deserialize, JsonSchema)]
pub struct RefreshInput {
    /// Calendar year (西元) to fetch. Defaults to this year.
    #[serde(default)]
    pub year: Option<i32>,
}

struct RefreshWorkCalendar;

/// One published file, as the platform describes it.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Distribution {
    #[serde(default)]
    resource_description: String,
    #[serde(default)]
    resource_format: String,
    #[serde(default)]
    resource_character_encoding: String,
    #[serde(default)]
    resource_download_url: String,
}

/// Pick the file for one ROC year.
///
/// The Google Calendar variant is a different shape with no 是否放假 column,
/// and a file the platform declares as anything but UTF-8 is left to the
/// manual path rather than decoded here. Later entries win: a correction is
/// appended after the file it corrects.
fn choose(files: &[Distribution], roc: i32) -> Option<&Distribution> {
    let prefix = format!("{roc}年");
    files.iter().rfind(|d| {
        d.resource_description.starts_with(&prefix)
            && !d.resource_description.contains("Google")
            && d.resource_format.eq_ignore_ascii_case("csv")
            && d.resource_character_encoding.eq_ignore_ascii_case("utf-8")
            && d.resource_download_url.starts_with("https://")
    })
}

async fn get(url: &str) -> Result<String, OpError> {
    super::fetch_text(url, "政府資料開放平臺").await
}

#[async_trait]
impl Operation for RefreshWorkCalendar {
    fn name(&self) -> &'static str {
        "refresh_work_calendar"
    }
    fn risk(&self) -> Risk {
        Risk::NeedsConfirm
    }
    fn summary(&self) -> &'static str {
        "更新工作日曆：向政府資料開放平臺取該年度的辦公日曆表並寫入，自行設定過的日期保留不動"
    }
    fn input_schema(&self) -> Value {
        schema_of::<RefreshInput>()
    }

    async fn call(&self, ctx: &OpCtx, input: Value) -> Result<OpOutcome, OpError> {
        let i: RefreshInput = parse(input)?;
        let year = i.year.unwrap_or_else(|| ctx.today.year());
        let fetched = fetch(year).await?;

        // The calendar the import is about to write, worked out first so a
        // failure weighing requests against it changes nothing: every
        // published day replaces what is there unless the company set that
        // date by hand, as `import_work_calendar` does.
        let mut after = WorkSchedule::load(ctx).await?;
        let by_hand: BTreeSet<NaiveDate> = ctx
            .store
            .work_calendar()
            .await?
            .into_iter()
            .filter(|d| d.source == "company")
            .map(|d| d.on_date)
            .collect();
        let changed = import_into(&mut after.calendar, &by_hand, &fetched.days);
        let overlaps = leave_overlaps(ctx, &after, &changed).await?;

        let (written, kept) = ctx
            .store
            .import_work_calendar(&fetched.days, &ctx.actor)
            .await?;

        let summary = format!(
            "更新工作日曆 {year} · 讀取 {} 天 · 寫入 {written} 天 · 保留自訂 {kept} 天{}",
            fetched.read,
            overlap_note(&overlaps)
        );
        Ok(OpOutcome::new(
            json!({
                "year": year,
                "source": fetched.source,
                "read": fetched.read,
                "written": written,
                "kept": kept,
                "leave_overlaps": overlaps,
            }),
            summary,
        ))
    }
}

/// Apply published days to `calendar` the way `import_work_calendar` writes
/// them -- each replaces what is there unless the company set that date by
/// hand -- and return the dates whose kind of day changed.
fn import_into(
    calendar: &mut BTreeMap<NaiveDate, (DayKind, String)>,
    by_hand: &BTreeSet<NaiveDate>,
    days: &[(NaiveDate, String, String)],
) -> Vec<NaiveDate> {
    let kind_in = |calendar: &BTreeMap<NaiveDate, (DayKind, String)>, date: NaiveDate| {
        calendar
            .get(&date)
            .map_or_else(|| default_day_kind(date), |(k, _)| *k)
    };
    let mut changed = Vec::new();
    for (date, code, name) in days {
        let Some(kind) = DayKind::from_code(code).filter(|_| !by_hand.contains(date)) else {
            continue;
        };
        if kind_in(calendar, *date) != kind {
            changed.push(*date);
        }
        calendar.insert(*date, (kind, name.clone()));
    }
    changed
}

/// One year, as the platform served it.
pub(crate) struct Fetched {
    source: String,
    read: usize,
    days: Vec<(NaiveDate, String, String)>,
}

/// Fetch and parse one year from the open-data platform.
async fn fetch(year: i32) -> Result<Fetched, OpError> {
    let roc = year - ROC_EPOCH;
    let meta: Value = serde_json::from_str(&get(DATASET_URL).await?).map_err(|_| {
        OpError::BadInput("政府資料開放平臺的回應格式無法解讀，請稍後再試。".into())
    })?;
    let files: Vec<Distribution> =
        serde_json::from_value(meta["result"]["distribution"].clone()).unwrap_or_default();
    let file = choose(&files, roc).ok_or_else(|| {
        OpError::BadInput(format!(
            "政府資料開放平臺上還沒有 {year} 年（民國 {roc} 年）的辦公日曆表。"
        ))
    })?;

    let rows = parse_gov_calendar(&get(&file.resource_download_url).await?)?;
    Ok(Fetched {
        source: file.resource_description.clone(),
        read: rows.len(),
        days: rows
            .iter()
            .filter(|r| r.date.year() == year)
            .filter_map(|r| deviation(r).map(|(k, name)| (r.date, k.code().to_string(), name)))
            .collect(),
    })
}

/// Fill an empty calendar on the first boot that can reach the platform.
///
/// This year and the next, because an installation set up in December would
/// otherwise open January with nothing in it. Best effort throughout: a
/// failure logs and leaves the table empty, and the next boot tries again, so
/// an installation that had no way out to the internet on its first day is not
/// left without a calendar forever. Never blocks the boot.
pub async fn seed(store: &laysi_store::Store, today: NaiveDate) {
    match store.work_calendar().await {
        Ok(days) if !days.is_empty() => return,
        Err(e) => {
            tracing::warn!(error = %e, "reading the work calendar failed, skipping the automatic refresh");
            return;
        }
        _ => {}
    }
    for year in [today.year(), today.year() + 1] {
        match fetch(year).await {
            Ok(f) => match store.import_work_calendar(&f.days, "").await {
                Ok((written, _)) => {
                    tracing::info!(year, source = %f.source, written, "work calendar refreshed automatically")
                }
                Err(e) => tracing::warn!(year, error = %e, "writing the work calendar failed"),
            },
            Err(e) => tracing::info!(year, reason = %e, "work calendar was not refreshed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    const SAMPLE: &str = "西元日期,星期,是否放假,備註\n\
        20260101,四,2,開國紀念日\n\
        20260102,五,0,\n\
        20260103,六,2,\n\
        20260104,日,2,\n\
        20260216,一,2,調整放假\n\
        20260221,六,0,補行上班\n";

    #[test]
    fn the_published_columns_are_found_by_name() {
        let rows = parse_gov_calendar(SAMPLE).unwrap();
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0].date, d(2026, 1, 1));
        assert!(rows[0].off);
        assert!(!rows[1].off);
    }

    /// Only the days that differ from the day of the week are stored: an
    /// ordinary Saturday needs no row, and neither does an ordinary Friday.
    #[test]
    fn an_ordinary_weekend_is_not_written_down() {
        let rows = parse_gov_calendar(SAMPLE).unwrap();
        let kept: Vec<(NaiveDate, DayKind, String)> = rows
            .iter()
            .filter_map(|r| deviation(r).map(|(k, n)| (r.date, k, n)))
            .collect();
        assert_eq!(
            kept.iter().map(|(date, _, _)| *date).collect::<Vec<_>>(),
            [d(2026, 1, 1), d(2026, 2, 16), d(2026, 2, 21)],
            "只有國定假日、調整放假與補行上班要存"
        );
        assert_eq!(kept[0].1, DayKind::Holiday);
        assert_eq!(kept[2].1, DayKind::Workday, "補行上班的星期六是工作日");
        assert_eq!(kept[2].2, "補行上班");
    }

    /// The published files start with a byte order mark, and the header is
    /// matched by name, so it has to go before the first column is read.
    #[test]
    fn the_byte_order_mark_the_platform_serves_is_not_part_of_the_header() {
        let rows = parse_gov_calendar(&format!("\u{feff}{SAMPLE}")).unwrap();
        assert_eq!(rows.len(), 6);
    }

    fn dist(desc: &str, format: &str, encoding: &str) -> Distribution {
        Distribution {
            resource_description: desc.into(),
            resource_format: format.into(),
            resource_character_encoding: encoding.into(),
            resource_download_url: format!("https://example.gov.tw/{desc}.csv"),
        }
    }

    /// The platform keeps every year in one dataset and appends corrections
    /// after the file they correct, so the last entry for a year wins. The
    /// Google Calendar variant is a different shape with no 是否放假 column.
    #[test]
    fn the_latest_readable_file_for_the_year_is_chosen() {
        let files = vec![
            dist("114年中華民國政府行政機關辦公日曆表", "CSV", "UTF-8"),
            dist("115年中華民國政府行政機關辦公日曆表", "CSV", "UTF-8"),
            dist(
                "115年中華民國政府行政機關辦公日曆表_Google行事曆專用",
                "CSV",
                "UTF-8",
            ),
            dist(
                "115年中華民國政府行政機關辦公日曆表(1150310更新)",
                "CSV",
                "UTF-8",
            ),
        ];
        assert_eq!(
            choose(&files, 115).unwrap().resource_description,
            "115年中華民國政府行政機關辦公日曆表(1150310更新)"
        );
        assert_eq!(
            choose(&files, 114).unwrap().resource_description,
            "114年中華民國政府行政機關辦公日曆表"
        );
        assert!(choose(&files, 116).is_none(), "還沒公布的年度沒有檔案");
        assert!(
            choose(&[dist("115年辦公日曆表", "CSV", "BIG5")], 115).is_none(),
            "不是 UTF-8 的檔案交給手動匯入"
        );
    }

    /// 颱風假 never arrives this way: the file is published a year ahead and
    /// the announcement comes the evening before. Nothing the parser produces
    /// may claim to be one.
    #[test]
    fn the_published_file_never_produces_a_natural_disaster_day() {
        let rows = parse_gov_calendar(SAMPLE).unwrap();
        for row in &rows {
            assert_ne!(
                deviation(row).map(|(k, _)| k),
                Some(DayKind::NaturalDisaster)
            );
        }
    }

    #[test]
    fn a_file_that_is_not_the_calendar_says_so() {
        assert!(parse_gov_calendar("a,b,c\n1,2,3\n").is_err());
        assert!(parse_gov_calendar("").is_err());
        assert!(parse_gov_calendar("西元日期,是否放假\n").is_err());
    }

    /// The calendar an import is weighed against before it is written: a
    /// published day lands unless the company set that date by hand, and only
    /// the dates whose kind of day moves are reported.
    #[test]
    fn an_import_is_applied_in_memory_as_the_store_writes_it() {
        let mut calendar = BTreeMap::new();
        calendar.insert(d(2026, 9, 28), (DayKind::NaturalDisaster, "颱風".into()));
        calendar.insert(d(2026, 10, 10), (DayKind::Holiday, "國慶日".into()));
        let by_hand = BTreeSet::from([d(2026, 9, 28)]);
        let published = [
            (d(2026, 9, 28), "holiday".to_string(), "教師節".to_string()),
            (d(2026, 10, 10), "holiday".to_string(), "國慶日".to_string()),
            (
                d(2026, 10, 9),
                "holiday".to_string(),
                "調整放假".to_string(),
            ),
            (
                d(2026, 10, 17),
                "workday".to_string(),
                "補行上班".to_string(),
            ),
        ];
        let changed = import_into(&mut calendar, &by_hand, &published);
        assert_eq!(changed, [d(2026, 10, 9), d(2026, 10, 17)]);
        assert_eq!(
            calendar[&d(2026, 9, 28)].0,
            DayKind::NaturalDisaster,
            "自訂的日子保留"
        );
        assert_eq!(calendar[&d(2026, 10, 17)].0, DayKind::Workday);
    }

    fn office() -> WorkSchedule {
        let t = |h: u32| NaiveTime::from_hms_opt(h, 0, 0);
        WorkSchedule {
            starts_at: t(9),
            ends_at: t(17),
            grace_minutes: 0,
            grace_stated: true,
            flex_minutes: 0,
            break_start: t(12),
            break_end: t(13),
            calendar: Default::default(),
        }
    }

    /// A request as it is stored when sent under `schedule`.
    fn sent(schedule: &WorkSchedule, from: &str, to: &str) -> LeaveRequest {
        let at = |v: &str| chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M").unwrap();
        let (from_at, to_at) = (at(from), at(to));
        let day_seconds = schedule.leave_seconds_by_date(from_at, to_at).unwrap();
        let seconds: i64 = day_seconds.iter().map(|(_, s)| s).sum();
        LeaveRequest {
            leave_no: "LV2607-003".into(),
            employee_id: 2,
            emp_no: "E002".into(),
            name: "測試員工".into(),
            dept_label: None,
            kind: "annual".into(),
            from_at,
            to_at,
            from_on: from_at.date(),
            to_on: to_at.date(),
            hours: Money::from(seconds) / Money::from(3600),
            days: Money::from(seconds) / Money::from(7 * 3600),
            day_seconds,
            reason: String::new(),
            status: "approved".into(),
            decided_by: String::new(),
            decided_role: String::new(),
            decided_at: None,
            decision_note: String::new(),
            cancelled: None,
            created_at: chrono::Utc::now(),
            created_by: String::new(),
        }
    }

    /// A day declared off after a request was sent is a day the request still
    /// counts and the calendar no longer does; a Saturday made a working day
    /// is the other way round. Either is listed, and nothing else is.
    #[test]
    fn a_request_is_listed_where_it_and_the_calendar_part_ways() {
        let mut schedule = office();
        let three_days = sent(&schedule, "2026-07-27T09:00", "2026-07-29T17:00");
        let weekend = sent(&schedule, "2026-07-31T13:00", "2026-08-03T12:00");

        schedule
            .calendar
            .insert(d(2026, 7, 28), (DayKind::NaturalDisaster, "颱風".into()));
        schedule
            .calendar
            .insert(d(2026, 8, 1), (DayKind::Workday, "補行上班".into()));
        schedule
            .calendar
            .insert(d(2026, 8, 2), (DayKind::Holiday, String::new()));

        let hours = |found: Option<(Money, i64)>| found.map(|(h, s)| (h.round_dp(2), s / 3600));
        assert_eq!(
            hours(disagreement(&schedule, &three_days, d(2026, 7, 28))),
            Some((Money::from(7), 0))
        );
        assert_eq!(disagreement(&schedule, &three_days, d(2026, 7, 27)), None);
        assert_eq!(disagreement(&schedule, &three_days, d(2026, 7, 30)), None);
        assert_eq!(
            hours(disagreement(&schedule, &weekend, d(2026, 8, 1))),
            Some((Money::ZERO, 7))
        );
        assert_eq!(
            disagreement(&schedule, &weekend, d(2026, 8, 2)),
            None,
            "週日改成國定假日，兩邊都不計"
        );
    }
}
