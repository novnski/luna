//! Calendar calculations shared by previews and the host-owned scheduler.
use jiff::{
    Timestamp,
    tz::{AmbiguousOffset, TimeZone},
};
use luna_proto::{AutomationDefinition, AutomationTrigger};
mod recurrence;
use recurrence::CalendarRule;

pub fn validate_definition(d: &AutomationDefinition) -> Result<(), String> {
    if d.name.trim().is_empty()
        || d.name.len() > 160
        || d.prompt.trim().is_empty()
        || d.prompt.len() > 32_768
    {
        return Err("Enter a name (up to 160 bytes) and prompt (up to 32 KiB)".into());
    }
    if !(0..=86_400_000).contains(&d.max_lateness_ms) {
        return Err("Missed-run tolerance must be between zero and 24 hours".into());
    }
    TimeZone::get(&d.timezone).map_err(|e| e.to_string())?;
    if d.source_task_id
        .as_ref()
        .is_some_and(|id| id.is_empty() || id.len() > 128)
    {
        return Err("Invalid source conversation".into());
    }
    if d.archive_successful_runs
        && matches!(d.target, luna_proto::AutomationTarget::Existing { .. })
    {
        return Err("Automatic archiving is only available for a new conversation each run".into());
    }
    if let Some(execution) = &d.execution {
        if execution.harness.is_retired() {
            return Err("Choose Codex, Claude Code, or OpenCode".into());
        }
        if execution.model.as_ref().is_some_and(|model| {
            model.trim().is_empty()
                || model.trim() != model
                || model.len() > 512
                || model.chars().any(char::is_control)
        }) || execution
            .provider_account_id
            .as_ref()
            .is_some_and(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
            || execution.model_options.len() > 32
            || serde_json::to_vec(&execution.model_options)
                .map_err(|e| e.to_string())?
                .len()
                > 8192
        {
            return Err("Choose valid, bounded provider/model settings".into());
        }
    }
    match &d.trigger {
        AutomationTrigger::Once { at_ms } => {
            Timestamp::from_millisecond(*at_ms).map_err(|e| e.to_string())?;
        }
        AutomationTrigger::Interval {
            anchor_ms,
            every_minutes,
        } => {
            Timestamp::from_millisecond(*anchor_ms).map_err(|e| e.to_string())?;
            if !(1..=525_600).contains(every_minutes) {
                return Err("Interval must be 1 minute to 1 year".into());
            }
        }
        AutomationTrigger::Calendar {
            hour,
            minute,
            weekdays,
        } => {
            if !(0..=23).contains(hour)
                || !(0..=59).contains(minute)
                || weekdays.len() > 7
                || weekdays.iter().any(|d| !(1..=7).contains(d))
            {
                return Err("Invalid local time or weekdays".into());
            }
        }
        AutomationTrigger::RRule { rule, anchor_ms } => {
            Timestamp::from_millisecond(*anchor_ms).map_err(|e| e.to_string())?;
            CalendarRule::parse(rule)?;
        }
        AutomationTrigger::PrWatch {
            repository,
            number,
            poll_minutes,
            wake_budget,
        } => {
            let parts: Vec<_> = repository.split('/').collect();
            if parts.len() != 2
                || parts.iter().any(|p| {
                    p.is_empty()
                        || p.starts_with('.')
                        || !p
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                })
                || *number == 0
                || !(1..=1440).contains(poll_minutes)
                || !(1..=100).contains(wake_budget)
            {
                return Err("Use an owner/repository, positive PR number, 1–1440 minute poll and 1–100 wake budget".into());
            }
        }
    }
    Ok(())
}
pub fn preview_times(
    trigger: &AutomationTrigger,
    timezone: &str,
    after: i64,
    count: usize,
) -> Result<Vec<i64>, String> {
    let mut result = Vec::new();
    let mut cursor = after;
    for _ in 0..count.min(3) {
        let Some(next) = next_after(trigger, timezone, cursor)? else {
            break;
        };
        result.push(next);
        cursor = next;
    }
    Ok(result)
}
pub(crate) fn next_after(
    trigger: &AutomationTrigger,
    timezone: &str,
    after: i64,
) -> Result<Option<i64>, String> {
    match trigger {
        AutomationTrigger::Once { at_ms } => Ok((*at_ms > after).then_some(*at_ms)),
        AutomationTrigger::Interval {
            anchor_ms,
            every_minutes,
        } => {
            let interval = i64::from(*every_minutes) * 60_000;
            if interval == 0 {
                return Err("Interval must be positive".into());
            }
            let next = if after < *anchor_ms {
                *anchor_ms
            } else {
                anchor_ms
                    .checked_add(
                        (after.saturating_sub(*anchor_ms) / interval + 1)
                            .checked_mul(interval)
                            .ok_or("Interval overflow")?,
                    )
                    .ok_or("Interval overflow")?
            };
            Ok(Some(next))
        }
        AutomationTrigger::PrWatch { poll_minutes, .. } => {
            Ok(after.checked_add(i64::from(*poll_minutes) * 60_000))
        }
        AutomationTrigger::RRule { rule, anchor_ms } => {
            CalendarRule::parse(rule)?.next(timezone, *anchor_ms, after)
        }
        AutomationTrigger::Calendar {
            hour,
            minute,
            weekdays,
        } => {
            if !(0..=23).contains(hour) || !(0..=59).contains(minute) {
                return Err("Invalid local time".into());
            }
            let tz = TimeZone::get(timezone).map_err(|e| e.to_string())?;
            let mut day = Timestamp::from_millisecond(after)
                .map_err(|e| e.to_string())?
                .to_zoned(tz.clone())
                .date();
            // At most one week plus one DST gap; bounded even for malformed input.
            for _ in 0..15 {
                if weekdays.is_empty() || weekdays.contains(&day.weekday().to_monday_one_offset()) {
                    let ambiguous = tz.to_ambiguous_timestamp(day.at(*hour, *minute, 0, 0));
                    if !matches!(ambiguous.offset(), AmbiguousOffset::Gap { .. }) {
                        let instant = ambiguous
                            .earlier()
                            .map_err(|e| e.to_string())?
                            .as_millisecond();
                        if instant > after {
                            return Ok(Some(instant));
                        }
                    }
                }
                day = day.tomorrow().map_err(|e| e.to_string())?;
            }
            Err("No occurrence within 15 days; check the weekdays and timezone".into())
        }
    }
}
/// Coalesce a sleep interval to its latest due slot with a bounded calendar walk.
pub(crate) fn due_slot(
    d: &AutomationDefinition,
    due: i64,
    now: i64,
) -> Result<(i64, u64, Option<i64>), String> {
    match d.trigger {
        AutomationTrigger::Once { .. } => Ok((due, 0, None)),
        AutomationTrigger::Interval { every_minutes, .. } => {
            let n = now.saturating_sub(due) / (i64::from(every_minutes) * 60_000);
            let slot = due + n * i64::from(every_minutes) * 60_000;
            Ok((slot, n as u64, next_after(&d.trigger, &d.timezone, now)?))
        }
        AutomationTrigger::PrWatch { .. } => {
            Ok((due, 0, next_after(&d.trigger, &d.timezone, now)?))
        }
        AutomationTrigger::Calendar { .. } | AutomationTrigger::RRule { .. } => {
            let mut slot = due;
            let mut n = 0;
            // Bound the walk to 3,660 calendar occurrences. Very old definitions
            // pause with a durable failure instead of blocking every later sweep.
            while let Some(next) = next_after(&d.trigger, &d.timezone, slot)? {
                if next > now {
                    return Ok((slot, n, Some(next)));
                }
                slot = next;
                n += 1;
                if n > 3660 {
                    return Err("Schedule has more than 3,660 missed calendar occurrences; edit it before resuming".into());
                }
            }
            Ok((slot, n, None))
        }
    }
}

/// Shared concise card text for saved agent-tool results.
pub fn describe_trigger(d: &AutomationDefinition) -> String {
    match &d.trigger {
        AutomationTrigger::Once { at_ms } => {
            format!("Once, {}", format_in_zone(*at_ms, &d.timezone))
        }
        AutomationTrigger::Interval { every_minutes, .. } => {
            format!("Every {every_minutes} minutes")
        }
        AutomationTrigger::Calendar {
            hour,
            minute,
            weekdays,
        } => {
            let labels = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
            let days = if weekdays.is_empty() {
                "Every day".into()
            } else {
                weekdays
                    .iter()
                    .filter_map(|d| labels.get((*d as usize).wrapping_sub(1)).copied())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!("{days} at {hour:02}:{minute:02} ({})", d.timezone)
        }
        AutomationTrigger::RRule { rule, .. } => {
            format!("{} ({})", rule.trim_start_matches("RRULE:"), d.timezone)
        }
        AutomationTrigger::PrWatch {
            repository,
            number,
            poll_minutes,
            ..
        } => format!("Watch {repository}#{number}, every {poll_minutes} minutes"),
    }
}

/// Device IANA zone for date entry; callers may fall back to an explicit UTC label.
pub fn system_timezone() -> Option<String> {
    jiff::tz::TimeZone::try_system()
        .ok()?
        .iana_name()
        .map(str::to_owned)
}
/// One-shot entry refuses ambiguity. Recurrences separately use the documented
/// skip-gap / earliest-fold policy, visible in their preview.
pub fn zoned_ms(
    zone: &str,
    year: i16,
    month: i8,
    day: i8,
    hour: i8,
    minute: i8,
) -> Result<i64, String> {
    let date = jiff::civil::Date::new(year, month, day).map_err(|e| e.to_string())?;
    let time = jiff::civil::Time::new(hour, minute, 0, 0).map_err(|e| e.to_string())?;
    let tz = TimeZone::get(zone).map_err(|e| e.to_string())?;
    tz.to_ambiguous_timestamp(date.to_datetime(time))
        .unambiguous()
        .map(|t| t.as_millisecond())
        .map_err(|e| format!("Choose an unambiguous local time: {e}"))
}
pub fn format_in_zone(ms: i64, zone: &str) -> String {
    match (Timestamp::from_millisecond(ms), TimeZone::get(zone)) {
        (Ok(t), Ok(tz)) => t.to_zoned(tz).to_string(),
        _ => "Time unavailable — check the timezone".into(),
    }
}
