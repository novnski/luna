//! Text for provider-reported native child activity (CC-08), shared by the
//! Agents surface and a spawn chip's inspection tooltip. Every value is what
//! the provider reported; nothing here is estimated or aggregated.

use luna_doc::SubagentActivity;

/// `95s` → `1m 35s`; sub-second runs read as `<1s`.
pub(crate) fn duration_label(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0 => "<1s".into(),
        1..=59 => format!("{secs}s"),
        _ if secs < 3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

fn count(n: u64, one: &str, many: &str) -> String {
    let unit = if n == 1 { one } else { many };
    format!("{} {unit}", group(n))
}

fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// The model to show: the one the child's own turns reported, else the
/// model its spawn requested.
pub(crate) fn model_label<'a>(
    activity: Option<&'a SubagentActivity>,
    requested: Option<&'a str>,
) -> Option<&'a str> {
    activity
        .and_then(|activity| activity.model.as_deref())
        .or(requested)
}

/// Compact metrics row: model · usage · tool uses · elapsed · background.
pub(crate) fn metrics(activity: Option<&SubagentActivity>, requested: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = model_label(activity, requested)
        .map(str::to_owned)
        .into_iter()
        .collect();
    let Some(activity) = activity else {
        return out;
    };
    if let (Some(input), Some(output)) = (activity.input_tokens, activity.output_tokens) {
        out.push(format!("{input} in · {output} out"));
    } else if let Some(total) = activity.total_tokens {
        out.push(count(total, "token", "tokens"));
    }
    if let Some(tools) = activity.tool_uses {
        out.push(count(tools, "tool use", "tool uses"));
    }
    if let Some(ms) = activity.duration_ms {
        out.push(duration_label(ms));
    }
    if activity.background == Some(true) {
        out.push("background".into());
    }
    out
}

/// One-line live progress for a running child: its summary, else the tool
/// it last used.
pub(crate) fn progress_line(activity: Option<&SubagentActivity>) -> Option<String> {
    let activity = activity?;
    activity.summary.clone().or_else(|| {
        activity
            .last_tool
            .as_ref()
            .map(|tool| format!("Last tool: {tool}"))
    })
}

/// Inspection-tooltip lines for the fields beyond input/output usage.
pub(crate) fn inspection_lines(activity: &SubagentActivity) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(model) = &activity.model {
        lines.push(format!("Reported model: {model}"));
    }
    let mut usage = Vec::new();
    if let Some(total) = activity.total_tokens {
        usage.push(count(total, "token", "tokens"));
    }
    if let Some(tools) = activity.tool_uses {
        usage.push(count(tools, "tool use", "tool uses"));
    }
    if let Some(ms) = activity.duration_ms {
        usage.push(duration_label(ms));
    }
    if !usage.is_empty() {
        lines.push(format!("Reported task usage: {}", usage.join(", ")));
    }
    if activity.background == Some(true) {
        lines.push("Runs in the background".into());
    }
    if let Some(progress) = progress_line(Some(activity)) {
        lines.push(format!("Progress: {progress}"));
    }
    lines
}
