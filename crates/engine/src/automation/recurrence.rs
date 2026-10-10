//! Bounded, minute-precision calendar recurrence. Unsupported RFC 5545 fields
//! are errors; they are never silently ignored. Gregorian matching is separate
//! from the existing Jiff skip-gap/first-fold instant policy.
use chrono::{Datelike, NaiveDate};
use jiff::{
    Timestamp,
    tz::{AmbiguousOffset, TimeZone},
};
use std::collections::BTreeMap;

pub(super) struct CalendarRule {
    frequency: String,
    interval: i64,
    hours: Vec<i8>,
    minutes: Vec<i8>,
    months: Vec<i8>,
    month_days: Vec<i8>,
    days: Vec<(i8, Option<i8>)>,
    week_start: i8,
    until: Option<i64>,
}

fn weekday(s: &str) -> Result<i8, String> {
    ["MO", "TU", "WE", "TH", "FR", "SA", "SU"]
        .iter()
        .position(|d| *d == s)
        .map(|n| n as i8 + 1)
        .ok_or_else(|| "Use ISO weekdays MO, TU, WE, TH, FR, SA, SU".into())
}
fn numbers(
    fields: &BTreeMap<&str, &str>,
    key: &str,
    min: i8,
    max: i8,
    zero: bool,
) -> Result<Vec<i8>, String> {
    let Some(value) = fields.get(key) else {
        return Ok(vec![]);
    };
    let mut out = Vec::new();
    for s in value.split(',') {
        let n = s
            .parse::<i8>()
            .map_err(|_| format!("Invalid {key}: use whole numbers"))?;
        if !(min..=max).contains(&n) || (!zero && n == 0) {
            return Err(format!(
                "Invalid {key}: values must be {min} to {max}{}",
                if zero { "" } else { " excluding zero" }
            ));
        }
        out.push(n);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

impl CalendarRule {
    pub(super) fn parse(input: &str) -> Result<Self, String> {
        if input.len() > 512 || !input.is_ascii() || input.contains(['\n', '\r']) {
            return Err("Enter a single recurrence rule of at most 512 ASCII bytes".into());
        }
        let upper = input.trim().to_ascii_uppercase();
        let text = upper.strip_prefix("RRULE:").unwrap_or(&upper);
        let mut fields = BTreeMap::new();
        for field in text.split(';') {
            let (key, value) = field.split_once('=').ok_or(
                "Use a recurrence rule such as FREQ=MONTHLY;BYDAY=1MO;BYHOUR=9;BYMINUTE=0",
            )?;
            if ![
                "FREQ",
                "INTERVAL",
                "BYHOUR",
                "BYMINUTE",
                "BYMONTH",
                "BYMONTHDAY",
                "BYDAY",
                "WKST",
                "UNTIL",
            ]
            .contains(&key)
            {
                return Err(format!(
                    "Unsupported recurrence field {key}. Supported: FREQ, INTERVAL, BYHOUR, BYMINUTE, BYMONTH, BYMONTHDAY, BYDAY, WKST, UNTIL"
                ));
            }
            if value.is_empty() || fields.insert(key, value).is_some() {
                return Err(format!("{key} must appear once with a nonempty value"));
            }
        }
        let frequency = fields
            .get("FREQ")
            .ok_or("A custom rule needs FREQ")?
            .to_string();
        if !["MINUTELY", "HOURLY", "DAILY", "WEEKLY", "MONTHLY", "YEARLY"]
            .contains(&frequency.as_str())
        {
            return Err("FREQ must be MINUTELY, HOURLY, DAILY, WEEKLY, MONTHLY, or YEARLY".into());
        }
        let interval = fields
            .get("INTERVAL")
            .copied()
            .unwrap_or("1")
            .parse::<i64>()
            .map_err(|_| "INTERVAL must be a positive whole number")?;
        let fixed = matches!(frequency.as_str(), "MINUTELY" | "HOURLY");
        let max_interval = match frequency.as_str() {
            "MINUTELY" => 525_600,
            "HOURLY" => 8760,
            _ => 100,
        };
        if !(1..=max_interval).contains(&interval) {
            return Err(format!(
                "INTERVAL must be 1 to {max_interval} for {frequency}"
            ));
        }
        if fixed
            && fields
                .keys()
                .any(|k| !["FREQ", "INTERVAL", "UNTIL"].contains(k))
        {
            return Err("MINUTELY and HOURLY custom rules support INTERVAL and UNTIL; use DAILY/WEEKLY/MONTHLY/YEARLY for calendar filters".into());
        }
        let hours = numbers(&fields, "BYHOUR", 0, 23, true)?;
        let minutes = numbers(&fields, "BYMINUTE", 0, 59, true)?;
        let months = numbers(&fields, "BYMONTH", 1, 12, false)?;
        let month_days = numbers(&fields, "BYMONTHDAY", -31, 31, false)?;
        if frequency == "WEEKLY" && !month_days.is_empty() {
            return Err("BYMONTHDAY is not valid for a weekly rule".into());
        }
        let mut days = Vec::new();
        if let Some(value) = fields.get("BYDAY") {
            for token in value.split(',') {
                if token.len() < 2 {
                    return Err("Invalid BYDAY weekday".into());
                }
                let (ordinal, day) = token.split_at(token.len() - 2);
                let day = weekday(day)?;
                let ordinal = if ordinal.is_empty() {
                    None
                } else {
                    let n = ordinal.parse::<i8>().map_err(|_| "Invalid BYDAY ordinal")?;
                    let bound = if frequency == "YEARLY" && months.is_empty() {
                        53
                    } else {
                        5
                    };
                    if !matches!(frequency.as_str(), "MONTHLY" | "YEARLY")
                        || n == 0
                        || i16::from(n).abs() > bound
                    {
                        return Err("Ordinal BYDAY is only valid for MONTHLY/YEARLY and must name an existing positive or negative weekday ordinal".into());
                    }
                    Some(n)
                };
                if !days.contains(&(day, ordinal)) {
                    days.push((day, ordinal));
                }
            }
        }
        let week_start = weekday(fields.get("WKST").copied().unwrap_or("MO"))?;
        let until = fields
            .get("UNTIL")
            .map(|value| {
                chrono::NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%SZ")
                    .map(|t| t.and_utc().timestamp_millis())
                    .map_err(|_| "UNTIL must be a UTC time such as 20351231T235959Z".to_string())
            })
            .transpose()?;
        Ok(Self {
            frequency,
            interval,
            hours,
            minutes,
            months,
            month_days,
            days,
            week_start,
            until,
        })
    }

    pub(super) fn next(
        &self,
        zone: &str,
        anchor_ms: i64,
        after: i64,
    ) -> Result<Option<i64>, String> {
        if self.until.is_some_and(|end| after >= end) {
            return Ok(None);
        }
        if matches!(self.frequency.as_str(), "MINUTELY" | "HOURLY") {
            let step = self.interval
                * if self.frequency == "HOURLY" {
                    3_600_000
                } else {
                    60_000
                };
            let next = if after < anchor_ms {
                anchor_ms
            } else {
                anchor_ms
                    .checked_add(
                        (after.saturating_sub(anchor_ms) / step + 1)
                            .checked_mul(step)
                            .ok_or("Rule interval overflow")?,
                    )
                    .ok_or("Rule interval overflow")?
            };
            return Ok(self.until.is_none_or(|end| next <= end).then_some(next));
        }
        let tz = TimeZone::get(zone).map_err(|e| e.to_string())?;
        let anchor = Timestamp::from_millisecond(anchor_ms)
            .map_err(|e| e.to_string())?
            .to_zoned(tz.clone());
        let start = Timestamp::from_millisecond(after.max(anchor_ms))
            .map_err(|e| e.to_string())?
            .to_zoned(tz.clone());
        let civil = |year: i16, month: i8, day: i8| {
            NaiveDate::from_ymd_opt(year.into(), month as u32, day as u32)
                .ok_or_else(|| "Date outside supported calendar".to_string())
        };
        let anchor_date = civil(anchor.year(), anchor.month(), anchor.day())?;
        let mut day = start.date();
        let hours = if self.hours.is_empty() {
            vec![anchor.hour()]
        } else {
            self.hours.clone()
        };
        let minutes = if self.minutes.is_empty() {
            vec![anchor.minute()]
        } else {
            self.minutes.clone()
        };
        let until_day = self
            .until
            .map(|ms| {
                Timestamp::from_millisecond(ms)
                    .map(|ts| ts.to_zoned(tz.clone()).date())
                    .map_err(|e| e.to_string())
            })
            .transpose()?;
        // Bound the search to 400 calendar years even for empty intersections.
        // Interval combinations whose next date lies beyond this supported
        // horizon are rejected explicitly instead of stalling a sweep.
        for _ in 0..=146_097 {
            if until_day.is_some_and(|end| day > end) {
                return Ok(None);
            }
            let date = civil(day.year(), day.month(), day.day())?;
            if self.matches(date, anchor_date) {
                for hour in &hours {
                    for minute in &minutes {
                        let instant = tz.to_ambiguous_timestamp(day.at(*hour, *minute, 0, 0));
                        if matches!(instant.offset(), AmbiguousOffset::Gap { .. }) {
                            continue;
                        }
                        let ms = instant
                            .earlier()
                            .map_err(|e| e.to_string())?
                            .as_millisecond();
                        if ms >= anchor_ms && ms > after {
                            return Ok(self.until.is_none_or(|end| ms <= end).then_some(ms));
                        }
                    }
                }
            }
            day = match day.tomorrow() {
                Ok(next) => next,
                Err(_) => return Ok(None),
            };
        }
        Err(
            "This recurrence has no occurrence within 400 years; simplify its calendar filters"
                .into(),
        )
    }

    fn matches(&self, date: NaiveDate, anchor: NaiveDate) -> bool {
        let offset = (date - anchor).num_days();
        let weekday = date.weekday().number_from_monday() as i8;
        let period = match self.frequency.as_str() {
            "DAILY" => offset,
            "WEEKLY" => {
                let week = |d: NaiveDate| {
                    i64::from(d.num_days_from_ce())
                        - i64::from(
                            (d.weekday().number_from_monday() as i8 - self.week_start)
                                .rem_euclid(7),
                        )
                };
                (week(date) - week(anchor)) / 7
            }
            "MONTHLY" => {
                i64::from(date.year() - anchor.year()) * 12 + i64::from(date.month())
                    - i64::from(anchor.month())
            }
            "YEARLY" => i64::from(date.year() - anchor.year()),
            _ => return false,
        };
        if period < 0 || period % self.interval != 0 {
            return false;
        }
        if !self.months.is_empty() && !self.months.contains(&(date.month() as i8)) {
            return false;
        }
        let days_in_month = |d: NaiveDate| {
            let next = if d.month() == 12 {
                NaiveDate::from_ymd_opt(d.year() + 1, 1, 1)
            } else {
                NaiveDate::from_ymd_opt(d.year(), d.month() + 1, 1)
            };
            next.and_then(|n| n.pred_opt())
                .map(|n| n.day())
                .unwrap_or(31)
        };
        let last = days_in_month(date);
        if !self.month_days.is_empty()
            && !self.month_days.iter().any(|d| {
                if *d > 0 {
                    date.day() == *d as u32
                } else {
                    i64::from(date.day()) == i64::from(last) + 1 + i64::from(*d)
                }
            })
        {
            return false;
        }
        if !self.days.is_empty()
            && !self.days.iter().any(|(d, ordinal)| {
                if *d != weekday {
                    return false;
                }
                let Some(n) = ordinal else {
                    return true;
                };
                let (index, length) = if self.frequency == "YEARLY" && self.months.is_empty() {
                    (
                        date.ordinal(),
                        NaiveDate::from_ymd_opt(date.year(), 12, 31)
                            .map(|d| d.ordinal())
                            .unwrap_or(365),
                    )
                } else {
                    (date.day(), last)
                };
                if *n > 0 {
                    (index - 1) / 7 + 1 == *n as u32
                } else {
                    (length - index) / 7 + 1 == (-*n) as u32
                }
            })
        {
            return false;
        }
        if self.frequency == "WEEKLY"
            && self.days.is_empty()
            && weekday != anchor.weekday().number_from_monday() as i8
        {
            return false;
        }
        if matches!(self.frequency.as_str(), "MONTHLY" | "YEARLY")
            && self.days.is_empty()
            && self.month_days.is_empty()
            && date.day() != anchor.day()
        {
            return false;
        }
        if self.frequency == "YEARLY"
            && self.months.is_empty()
            && self.days.is_empty()
            && self.month_days.is_empty()
            && date.month() != anchor.month()
        {
            return false;
        }
        true
    }
}
