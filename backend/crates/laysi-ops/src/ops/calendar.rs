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

use super::attendance::{default_day_kind, DayKind, WorkSchedule, DAY_KINDS};
use crate::{parse, schema_of, OpCtx, OpError, OpOutcome, Operation, Risk};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

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
        if i.kind.trim().is_empty() {
            let cleared = ctx.store.clear_work_calendar_day(date).await?;
            let kind = default_day_kind(date);
            let summary = format!("工作日曆 {date} 回到{}", kind.label());
            return Ok(OpOutcome::new(
                json!({
                    "on_date": date, "kind": kind.code(), "kind_label": kind.label(),
                    "default": true, "changed": cleared,
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
        ctx.store
            .set_work_calendar_day(date, kind.code(), &name, "company", &ctx.actor)
            .await?;

        let summary = format!("工作日曆 {date} 設為{}", kind.label());
        Ok(OpOutcome::new(
            json!({
                "on_date": date, "kind": kind.code(), "kind_label": kind.label(),
                "name": name, "source": "company", "default": false, "changed": true,
            }),
            summary,
        ))
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
    let date_col = find(&["西元日期", "date", "Date"]).ok_or_else(|| {
        OpError::BadInput("找不到日期欄位，請確認檔案是政府行事曆的 CSV。".into())
    })?;
    let off_col = find(&["是否放假", "isHoliday", "IsHoliday"]).ok_or_else(|| {
        OpError::BadInput("找不到放假欄位，請確認檔案是政府行事曆的 CSV。".into())
    })?;
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| OpError::BadInput(format!("連線設定失敗：{e}")))?;
    let response =
        client.get(url).send().await.map_err(|_| {
            OpError::BadInput("連不上政府資料開放平臺，請確認對外連線後再試。".into())
        })?;
    if !response.status().is_success() {
        return Err(OpError::BadInput(format!(
            "政府資料開放平臺回應 {}，請稍後再試。",
            response.status().as_u16()
        )));
    }
    response
        .text()
        .await
        .map_err(|_| OpError::BadInput("政府資料開放平臺的回應讀不完整。".into()))
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
        let (written, kept) = ctx
            .store
            .import_work_calendar(&fetched.days, &ctx.actor)
            .await?;

        let summary = format!(
            "更新工作日曆 {year} · 讀取 {} 天 · 寫入 {written} 天 · 保留自訂 {kept} 天",
            fetched.read
        );
        Ok(OpOutcome::new(
            json!({
                "year": year,
                "source": fetched.source,
                "read": fetched.read,
                "written": written,
                "kept": kept,
            }),
            summary,
        ))
    }
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
            tracing::warn!(error = %e, "讀取工作日曆失敗，略過自動更新");
            return;
        }
        _ => {}
    }
    for year in [today.year(), today.year() + 1] {
        match fetch(year).await {
            Ok(f) => match store.import_work_calendar(&f.days, "").await {
                Ok((written, _)) => {
                    tracing::info!(year, source = %f.source, written, "工作日曆已自動更新")
                }
                Err(e) => tracing::warn!(year, error = %e, "工作日曆寫入失敗"),
            },
            Err(e) => tracing::info!(year, reason = %e, "工作日曆未自動更新"),
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
}
