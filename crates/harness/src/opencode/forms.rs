//! Forms: questions, MCP elicitation and provider pickers (OC-05).
//!
//! OpenCode 2 asks through `form.created {form}` (no top-level `sessionID`;
//! the owner is `form.sessionID`, or the `"global"` sentinel for MCP
//! elicitation) and is answered with `POST /api/session/{s}/form/{id}/reply
//! {answer}` or cancelled with `DELETE /api/session/{s}/form/{id}?message=`
//! (`oc2:schema/src/form.ts`, `oc2:core/src/form.ts`). A cancel WITH a
//! message is feedback the asker continues with; without one the native
//! question tool ends the step.
//!
//! Forms back LUNA's shared surfaces, never a new dialog (PD-11):
//! - questions (`metadata.kind: "question"`), the websearch provider picker
//!   and other plain text/choice forms → the shared question panel, like
//!   Claude's AskUserQuestion and Codex's request_user_input (options with
//!   descriptions, a typed answer when the field takes custom values).
//! - MCP elicitation (`metadata.kind: "mcp-elicitation"`) and typed forms
//!   (numbers, booleans, string formats and lengths, multiselect item
//!   counts) → the shared MCP form contract (`luna_proto::McpInput`,
//!   validated by `mcp_response`); an `external` field is URL mode, with the
//!   URL kept out of documents.
//! - `hidden` fields use their default; `when` fields are offered as
//!   optional and dropped when inactive, as OpenCode requires.
//! - anything LUNA can't show is cancelled at once with a message saying
//!   why, so the asker continues (OCF-56). A form never hangs.
//!
//! Attribution: a form owned by this run's session or one of its subagent
//! sessions shows on this chat (child forms name the sub-agent). The
//! `"global"` owner (MCP elicitation OpenCode can't attribute) shows on this
//! chat while its turn is active — the run owns a private server for one
//! location — and is cancelled with a message otherwise (OCF-158).

use std::collections::HashSet;
use std::sync::Mutex;

use luna_proto::{McpChoice, McpFieldSchema, McpInput, McpInputKind, UserInputDetails};

use super::*;
use crate::CancellationToken;

/// OpenCode's owner sentinel for MCP elicitation it can't attribute.
pub(super) const GLOBAL_FORM_OWNER: &str = "global";

/// The MCP form contract's request size limit.
const MAX_FORM_JSON: usize = 64 * 1024;

const DISMISSED_FEEDBACK: &str = "The user dismissed this question in LUNA.";
const DECLINED_FEEDBACK: &str = "The user declined this request in LUNA.";
const UNANSWERED_FEEDBACK: &str = "LUNA closed this form without an answer.";

/// What a form's answer becomes on the wire.
#[derive(Debug, PartialEq)]
pub(super) enum FormReply {
    Answer(serde_json::Map<String, Value>),
    /// `DELETE …/form/{id}`, with optional feedback for the asker.
    Cancel(Option<String>),
}

#[derive(Clone, Debug)]
struct FormOption {
    value: String,
    label: String,
    description: String,
}

#[derive(Clone, Debug)]
struct When {
    key: String,
    eq: bool,
    value: Value,
}

#[derive(Clone, Debug)]
enum FieldKind {
    String {
        format: Option<String>,
        min_length: Option<u64>,
        max_length: Option<u64>,
        pattern: Option<String>,
        options: Vec<FormOption>,
        custom: bool,
    },
    Number {
        integer: bool,
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
    Boolean,
    Multiselect {
        options: Vec<FormOption>,
        min_items: Option<u64>,
        max_items: Option<u64>,
        custom: bool,
    },
    External {
        url: String,
    },
}

/// One `Form.Field`.
#[derive(Clone, Debug)]
struct FormField {
    key: String,
    kind: FieldKind,
    title: Option<String>,
    description: Option<String>,
    required: bool,
    hidden: bool,
    when: Vec<When>,
    default: Option<Value>,
    placeholder: Option<String>,
    raw: Value,
}

impl FormField {
    fn parse(raw: &Value) -> Result<Self, String> {
        let key = raw
            .get("key")
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty() && key.len() <= 128)
            .ok_or("a field has no usable key")?
            .to_owned();
        let text = |name: &str| {
            raw.get(name)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let count = |name: &str| raw.get(name).and_then(Value::as_u64);
        let bound = |name: &str| raw.get(name).and_then(Value::as_f64);
        let options = || -> Result<Vec<FormOption>, String> {
            raw.get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .map(|option| {
                            let value = option
                                .get("value")
                                .and_then(Value::as_str)
                                .ok_or(format!("field `{key}` has an option without a value"))?;
                            Ok(FormOption {
                                value: value.to_owned(),
                                label: option
                                    .get("label")
                                    .and_then(Value::as_str)
                                    .filter(|label| !label.is_empty())
                                    .unwrap_or(value)
                                    .to_owned(),
                                description: option
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                            })
                        })
                        .collect()
                })
                .unwrap_or(Ok(Vec::new()))
        };
        let custom = raw.get("custom").and_then(Value::as_bool).unwrap_or(false);
        let kind = match raw.get("type").and_then(Value::as_str) {
            Some("string") => FieldKind::String {
                format: text("format"),
                min_length: count("minLength"),
                max_length: count("maxLength"),
                pattern: text("pattern"),
                options: options()?,
                custom,
            },
            Some(kind @ ("number" | "integer")) => FieldKind::Number {
                integer: kind == "integer",
                minimum: bound("minimum"),
                maximum: bound("maximum"),
            },
            Some("boolean") => FieldKind::Boolean,
            Some("multiselect") => FieldKind::Multiselect {
                options: options()?,
                min_items: count("minItems"),
                max_items: count("maxItems"),
                custom,
            },
            Some("external") => FieldKind::External {
                url: text("url").ok_or(format!("website field `{key}` has no URL"))?,
            },
            other => {
                return Err(format!(
                    "field `{key}` has a type LUNA can't show ({})",
                    other.unwrap_or("none")
                ));
            }
        };
        let when = raw
            .get("when")
            .and_then(Value::as_array)
            .map(|conditions| {
                conditions
                    .iter()
                    .map(|condition| {
                        Some(When {
                            key: condition.get("key")?.as_str()?.to_owned(),
                            eq: condition.get("op")?.as_str()? == "eq",
                            value: condition.get("value")?.clone(),
                        })
                    })
                    .collect::<Option<Vec<_>>>()
                    .ok_or(format!("field `{key}` has a condition LUNA can't read"))
            })
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            key,
            kind,
            title: text("title"),
            description: text("description"),
            required: raw.get("required").and_then(Value::as_bool).unwrap_or(false),
            hidden: raw.get("hidden").and_then(Value::as_bool).unwrap_or(false),
            when,
            default: raw.get("default").filter(|value| !value.is_null()).cloned(),
            placeholder: text("placeholder"),
            raw: raw.clone(),
        })
    }

    fn name(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.key)
    }

    /// The shared question panel takes text and choices without per-field
    /// validation: plain strings and multiselects only.
    fn panel_compatible(&self) -> bool {
        match &self.kind {
            FieldKind::String {
                format,
                min_length,
                max_length,
                pattern,
                ..
            } => format.is_none() && min_length.is_none() && max_length.is_none() && pattern.is_none(),
            FieldKind::Multiselect {
                min_items,
                max_items,
                ..
            } => min_items.is_none() && max_items.is_none(),
            _ => false,
        }
    }

    /// "(only when Size is large)" for a conditional field.
    fn condition_note(&self, fields: &[FormField]) -> Option<String> {
        if self.when.is_empty() {
            return None;
        }
        let parts: Vec<String> = self
            .when
            .iter()
            .map(|when| {
                let name = fields
                    .iter()
                    .find(|field| field.key == when.key)
                    .map(FormField::name)
                    .unwrap_or(&when.key);
                let value = match &when.value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                format!("{name} {} {value}", if when.eq { "is" } else { "is not" })
            })
            .collect();
        Some(format!("Only needed when {}.", parts.join(" and ")))
    }

    /// The shared MCP form contract's schema for this field.
    fn mcp_schema(&self) -> Result<McpFieldSchema, String> {
        let unsupported = |why: &str| format!("field `{}` {why}", self.key);
        let choices = |options: &[FormOption]| -> Vec<McpChoice> {
            options
                .iter()
                .map(|option| McpChoice {
                    value: option.value.clone(),
                    label: option_label(options, option),
                })
                .collect()
        };
        let mut schema = McpFieldSchema {
            value_type: String::new(),
            // A conditional field may be inactive; required-ness is checked
            // against the final answer instead.
            required: self.required && self.when.is_empty(),
            choices: Vec::new(),
            default: self.default.clone(),
            min_length: None,
            max_length: None,
            minimum: None,
            maximum: None,
            min_items: None,
            max_items: None,
            format: None,
        };
        match &self.kind {
            FieldKind::String {
                format,
                min_length,
                max_length,
                pattern,
                options,
                custom,
            } => {
                if pattern.is_some() {
                    return Err(unsupported("has a pattern constraint LUNA can't check"));
                }
                schema.value_type = "string".into();
                // Custom values with suggestions: a free-text field (the
                // suggestions are listed in the prompt).
                if !*custom {
                    schema.choices = choices(options);
                }
                schema.format = format.clone();
                schema.min_length = *min_length;
                schema.max_length = *max_length;
            }
            FieldKind::Number {
                integer,
                minimum,
                maximum,
            } => {
                schema.value_type = if *integer { "integer" } else { "number" }.into();
                schema.minimum = *minimum;
                schema.maximum = *maximum;
            }
            FieldKind::Boolean => schema.value_type = "boolean".into(),
            FieldKind::Multiselect {
                options,
                min_items,
                max_items,
                custom,
            } => {
                if *custom {
                    return Err(unsupported("accepts custom entries LUNA can't collect in a typed form"));
                }
                schema.value_type = "array".into();
                schema.choices = choices(options);
                schema.min_items = *min_items;
                schema.max_items = *max_items;
            }
            FieldKind::External { .. } => {
                return Err(unsupported("mixes a website step with other fields"));
            }
        }
        schema
            .validated()
            .map_err(|why| unsupported(&format!("can't be shown: {why}")))
    }
}

/// A choice label, disambiguated by its value when two options share one.
fn option_label(options: &[FormOption], option: &FormOption) -> String {
    if options
        .iter()
        .filter(|other| other.label == option.label)
        .count()
        > 1
    {
        format!("{} ({})", option.label, option.value)
    } else {
        option.label.clone()
    }
}

#[derive(Clone, Debug, PartialEq)]
enum PlanShape {
    /// The shared question panel.
    Panel,
    /// The shared MCP form contract (fields plus an Accept/Decline/Cancel
    /// action).
    Typed,
    /// MCP URL mode: one `external` field answered `true` on Accept.
    Url { key: String },
    /// Every field is hidden: reply with the defaults, nothing to show.
    Silent,
}

/// How one OpenCode form is shown and answered.
#[derive(Clone, Debug)]
pub(super) struct FormPlan {
    pub(super) form_id: String,
    pub(super) questions: Vec<UserInputQuestion>,
    shape: PlanShape,
    fields: Vec<FormField>,
    /// The asker's own fallback when the user gives no answer at all.
    dismissed: Option<String>,
}

/// Plan a form for LUNA's shared surfaces. `thread` is the session the
/// prompt is bound to; `subagent` attributes a child session's form. An
/// `Err` is the reason the form is cancelled instead of shown.
pub(super) fn plan_form(
    form: &Value,
    thread: &str,
    subagent: Option<&str>,
) -> Result<FormPlan, String> {
    if form.to_string().len() > MAX_FORM_JSON {
        return Err("the form is larger than LUNA's 64 KiB limit".into());
    }
    let form_id = form
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 256)
        .ok_or("the form has no usable id")?
        .to_owned();
    let title = form
        .get("title")
        .and_then(Value::as_str)
        .filter(|title| !title.is_empty())
        .unwrap_or("OpenCode")
        .to_owned();
    let metadata = form.get("metadata").cloned().unwrap_or(Value::Null);
    let kind = metadata.get("kind").and_then(Value::as_str).unwrap_or("");
    let fields = form
        .get("fields")
        .and_then(Value::as_array)
        .filter(|fields| !fields.is_empty())
        .ok_or("the form has no fields")?
        .iter()
        .map(FormField::parse)
        .collect::<Result<Vec<_>, _>>()?;
    if fields.len() > luna_proto::MCP_MAX_FIELDS {
        return Err(format!(
            "the form has more than {} fields",
            luna_proto::MCP_MAX_FIELDS
        ));
    }
    if fields.iter().any(|field| match &field.kind {
        FieldKind::String { options, .. } | FieldKind::Multiselect { options, .. } => {
            options.len() > luna_proto::MCP_MAX_OPTIONS
        }
        _ => false,
    }) {
        return Err(format!(
            "a field has more than {} options",
            luna_proto::MCP_MAX_OPTIONS
        ));
    }
    let message = metadata
        .get("message")
        .and_then(Value::as_str)
        .filter(|message| !message.is_empty())
        .map(|message| clip(message, luna_proto::MCP_MAX_TEXT))
        .unwrap_or_else(|| title.clone());
    let server_name = metadata
        .get("server")
        .and_then(Value::as_str)
        .filter(|server| !server.is_empty())
        .map(|server| clip(server, 128))
        .unwrap_or_else(|| "OpenCode".into());
    let is_mcp = kind == "mcp-elicitation";
    // A hidden field with nothing to fall back on must still be asked.
    let visible: Vec<&FormField> = fields
        .iter()
        .filter(|field| {
            !field.hidden
                || (field.default.is_none()
                    && field.required
                    && !matches!(field.kind, FieldKind::External { .. }))
        })
        .collect();
    let externals = fields
        .iter()
        .filter(|field| matches!(field.kind, FieldKind::External { .. }))
        .count();
    let header_suffix = subagent
        .map(|label| format!(" · sub-agent {label}"))
        .unwrap_or_default();

    let shape = if externals > 0 {
        if fields.len() != 1 {
            return Err("the form mixes a website step with other fields".into());
        }
        PlanShape::Url {
            key: fields[0].key.clone(),
        }
    } else if visible.is_empty() {
        PlanShape::Silent
    } else if !is_mcp && visible.iter().all(|field| field.panel_compatible()) {
        PlanShape::Panel
    } else {
        PlanShape::Typed
    };

    let binding = McpInput {
        server_name: server_name.clone(),
        provider_request_id: form_id.clone(),
        thread_id: thread.to_owned(),
        // OpenCode forms are not turn-scoped on the wire; the form id is the
        // binding, and the adapter closes it when the run's turn ends.
        turn_id: form_id.clone(),
        // No LUNA deadline (Codex parity): a slow website flow must not be
        // cancelled by LUNA. The answer, OpenCode, Stop or the turn ends it.
        expires_at_ms: luna_proto::APPROVAL_NO_EXPIRY,
        kind: McpInputKind::FormAction,
        launch_url: None,
    };
    let mcp_question = |id: String,
                        header: String,
                        question: String,
                        options: Vec<String>,
                        kind: McpInputKind,
                        multi_select: bool| {
        let mut meta = binding.clone();
        meta.kind = kind;
        UserInputQuestion {
            id,
            header,
            question,
            options,
            multi_select,
            approval: None,
            mcp: Some(meta),
            input: None,
        }
    };
    let action_options = || vec!["Accept".into(), "Decline".into(), "Cancel".into()];
    let action_header = if is_mcp {
        format!("MCP · {server_name}{header_suffix}")
    } else {
        format!("{title}{header_suffix}")
    };

    let questions = match &shape {
        PlanShape::Silent => Vec::new(),
        PlanShape::Url { key } => {
            let FieldKind::External { url } = &fields[0].kind else {
                unreachable!("URL plans hold one external field");
            };
            if url.len() > luna_proto::MCP_MAX_TEXT {
                return Err("the website address is too long".into());
            }
            let destination = luna_proto::mcp_url_destination(url).map_err(str::to_owned)?;
            let elicitation_id = metadata
                .get("elicitationID")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 256)
                .unwrap_or(&form_id)
                .to_owned();
            let message = message.replace(url.as_str(), &destination);
            let mut question = mcp_question(
                format!("form:{form_id}:{key}"),
                action_header,
                format!(
                    "{message}\nOpen {destination} to continue this request, then choose Accept, \
                     Decline or Cancel. Opening the website does not submit a response."
                ),
                action_options(),
                McpInputKind::Url {
                    destination,
                    elicitation_id,
                },
                false,
            );
            if let Some(meta) = question.mcp.as_mut() {
                meta.launch_url = Some(url.clone());
            }
            vec![question]
        }
        PlanShape::Panel => visible
            .iter()
            .map(|field| {
                let (options, multi_select, custom) = match &field.kind {
                    FieldKind::String {
                        options, custom, ..
                    } => (options.as_slice(), false, *custom),
                    FieldKind::Multiselect {
                        options, custom, ..
                    } => (options.as_slice(), true, *custom),
                    _ => unreachable!("panel plans hold strings and multiselects"),
                };
                let mut question = field
                    .description
                    .clone()
                    .or_else(|| field.title.clone())
                    .unwrap_or_else(|| title.clone());
                if let Some(note) = field.condition_note(&fields) {
                    question.push_str(&format!("\n{note}"));
                }
                if let Some(default) = field.default.as_ref().map(value_text) {
                    question.push_str(&format!("\nDefault: {default}"));
                } else if let Some(placeholder) = &field.placeholder {
                    question.push_str(&format!("\nFor example: {placeholder}"));
                }
                UserInputQuestion {
                    id: format!("form:{form_id}:{}", field.key),
                    header: format!(
                        "{}{header_suffix}",
                        field.title.as_deref().unwrap_or(&title)
                    ),
                    question: clip(&question, luna_proto::MCP_MAX_TEXT),
                    options: options
                        .iter()
                        .map(|option| option_label(options, option))
                        .collect(),
                    multi_select,
                    approval: None,
                    mcp: None,
                    input: Some(UserInputDetails {
                        option_descriptions: options
                            .iter()
                            .map(|option| option.description.clone())
                            .collect(),
                        allow_other: custom || options.is_empty(),
                        secret: false,
                    }),
                }
            })
            .collect(),
        PlanShape::Typed => {
            let mut questions = Vec::new();
            for field in &visible {
                if crate::codex::elicitation::sensitive_field(&field.key, &field.raw) {
                    return Err(format!(
                        "field `{}` asks for a secret; forms are for non-sensitive information",
                        field.key
                    ));
                }
                let schema = field.mcp_schema()?;
                let prompt_message = if questions.is_empty() {
                    format!("{message}\n")
                } else {
                    String::new()
                };
                let mut notes = String::new();
                if let FieldKind::String {
                    options,
                    custom: true,
                    ..
                } = &field.kind
                    && !options.is_empty()
                {
                    let labels: Vec<&str> =
                        options.iter().map(|option| option.label.as_str()).collect();
                    notes.push_str(&format!("\nSuggestions: {}", labels.join(", ")));
                }
                if let Some(note) = field.condition_note(&fields) {
                    notes.push_str(&format!("\n{note}"));
                }
                let text = format!(
                    "{prompt_message}{server_name} requests {}. {}\n{} ({}){notes}",
                    field.name(),
                    field.description.as_deref().unwrap_or(""),
                    if schema.required { "Required" } else { "Optional" },
                    schema.value_type
                );
                let options = if schema.value_type == "boolean" {
                    vec!["false".into(), "true".into()]
                } else {
                    schema.choices.iter().map(|c| c.label.clone()).collect()
                };
                let multi = schema.value_type == "array";
                questions.push(mcp_question(
                    format!("form:{form_id}:{}", field.key),
                    format!("{}{header_suffix}", field.name()),
                    clip(&text, luna_proto::MCP_MAX_TEXT * 2),
                    options,
                    McpInputKind::Field {
                        name: field.key.clone(),
                        schema: Box::new(schema),
                    },
                    multi,
                ));
            }
            questions.push(mcp_question(
                format!("form:{form_id}:@action"),
                action_header,
                format!(
                    "{message}\nSubmit these values to {server_name}? Forms are for non-sensitive \
                     information; use a website for passwords or credentials."
                ),
                action_options(),
                McpInputKind::FormAction,
                false,
            ));
            questions
        }
    };
    Ok(FormPlan {
        form_id,
        questions,
        shape,
        fields,
        dismissed: Some(if kind == "question" {
            DISMISSED_FEEDBACK.to_owned()
        } else {
            UNANSWERED_FEEDBACK.to_owned()
        }),
    })
}

impl FormPlan {
    /// The wire reply for the engine's answers (`[]` = dismissed, expired
    /// or closed without an answer).
    pub(super) fn reply(&self, answers: &[UserInputAnswer]) -> FormReply {
        let mut answer = serde_json::Map::new();
        match &self.shape {
            PlanShape::Silent => {}
            PlanShape::Url { key } => {
                return match luna_proto::mcp_response(&self.questions, answers) {
                    Ok(response) if response["action"] == "accept" => {
                        answer.insert(key.clone(), Value::Bool(true));
                        FormReply::Answer(answer)
                    }
                    Ok(response) if response["action"] == "decline" => {
                        FormReply::Cancel(Some(DECLINED_FEEDBACK.into()))
                    }
                    Ok(_) => FormReply::Cancel(None),
                    Err(_) => FormReply::Cancel(self.dismissed.clone()),
                };
            }
            PlanShape::Panel => {
                if answers.iter().all(|answer| answer.labels.is_empty()) {
                    return FormReply::Cancel(self.dismissed.clone());
                }
                for question in &self.questions {
                    let Some(field) = self
                        .fields
                        .iter()
                        .find(|field| question.id == format!("form:{}:{}", self.form_id, field.key))
                    else {
                        continue;
                    };
                    let labels = answers
                        .iter()
                        .find(|answer| answer.question_id == question.id)
                        .map(|answer| answer.labels.as_slice())
                        .unwrap_or(&[]);
                    if let Some(value) = panel_value(field, labels) {
                        answer.insert(field.key.clone(), value);
                    }
                }
            }
            PlanShape::Typed => match luna_proto::mcp_response(&self.questions, answers) {
                Ok(response) => match response["action"].as_str() {
                    Some("accept") => {
                        if let Some(content) = response.get("content").and_then(Value::as_object)
                        {
                            answer = content.clone();
                        }
                    }
                    Some("decline") => return FormReply::Cancel(Some(DECLINED_FEEDBACK.into())),
                    _ => return FormReply::Cancel(None),
                },
                Err(_) => return FormReply::Cancel(self.dismissed.clone()),
            },
        }
        // Hidden fields fall back to their defaults; unanswered defaults too.
        for field in &self.fields {
            if !answer.contains_key(&field.key)
                && let Some(default) = &field.default
                && !matches!(field.kind, FieldKind::External { .. })
            {
                answer.insert(field.key.clone(), default.clone());
            }
        }
        // Inactive conditional fields must not be answered (one forward
        // pass: conditions reference earlier fields, so removal cascades).
        for field in &self.fields {
            if !field.when.is_empty() && !field_active(field, &answer) {
                answer.remove(&field.key);
            }
        }
        for field in &self.fields {
            let missing = match answer.get(&field.key) {
                None => true,
                Some(Value::String(text)) => text.is_empty(),
                Some(Value::Array(items)) => items.is_empty(),
                Some(_) => false,
            };
            if field.required && missing && field_active(field, &answer) {
                return FormReply::Cancel(Some(format!(
                    "The user did not answer the required field \"{}\" in LUNA.",
                    field.name()
                )));
            }
        }
        FormReply::Answer(answer)
    }
}

/// OpenCode's condition semantics: every condition holds; against a
/// multiselect answer `eq` means "includes"; an unanswered field fails both
/// `eq` and `neq`.
fn field_active(field: &FormField, answer: &serde_json::Map<String, Value>) -> bool {
    field.when.iter().all(|when| {
        let Some(value) = answer.get(&when.key) else {
            return false;
        };
        let hit = match value {
            Value::Array(items) => items.iter().any(|item| *item == when.value),
            other => *other == when.value,
        };
        hit == when.eq
    })
}

/// A panel answer's wire value: a chosen option's value, or the typed text
/// when the field takes custom values.
fn panel_value(field: &FormField, labels: &[String]) -> Option<Value> {
    let (options, custom) = match &field.kind {
        FieldKind::String {
            options, custom, ..
        }
        | FieldKind::Multiselect {
            options, custom, ..
        } => (options.as_slice(), *custom),
        _ => return None,
    };
    let resolve = |label: &str| -> Option<String> {
        options
            .iter()
            .find(|option| option_label(options, option) == label)
            .map(|option| option.value.clone())
            .or_else(|| (custom || options.is_empty()).then(|| label.to_owned()))
    };
    match field.kind {
        FieldKind::Multiselect { .. } => {
            let values: Vec<Value> = labels
                .iter()
                .filter(|label| !label.is_empty())
                .filter_map(|label| resolve(label))
                .map(Value::String)
                .collect();
            (!values.is_empty()).then_some(Value::Array(values))
        }
        _ => labels
            .iter()
            .find(|label| !label.is_empty())
            .and_then(|label| resolve(label))
            .map(Value::String),
    }
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(value_text).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Percent-encode a query value (RFC 3986 unreserved characters pass).
fn encode_query(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Where and how one form is answered.
struct FormResponder {
    server: Server,
    /// The reply path's session segment: the owner, or `"global"`.
    owner: String,
    form_id: String,
    title: String,
    dir: Option<String>,
    failures: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    pending: Arc<Mutex<HashMap<String, CancellationToken>>>,
    settled: Arc<Mutex<HashSet<String>>>,
}

impl FormResponder {
    fn forget(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&self.form_id);
        }
    }

    async fn chip(&self, message: String) {
        let _ = self.failures.send(Ok(AgentEvent::Error { message })).await;
    }

    async fn cancel(&self, message: Option<&str>) -> Option<String> {
        let mut path = format!("/api/session/{}/form/{}", self.owner, self.form_id);
        if let Some(message) = message {
            path.push_str(&format!("?message={}", encode_query(message)));
        }
        match self
            .server
            .exchange(
                reqwest::Method::DELETE,
                &path,
                self.dir.as_deref(),
                None,
                Some(CALL_TIMEOUT),
            )
            .await
        {
            Ok((status, _))
                if status.is_success()
                    || status == reqwest::StatusCode::NOT_FOUND
                    || status == reqwest::StatusCode::CONFLICT =>
            {
                None
            }
            Ok((status, text)) => Some(http_error_message("DELETE", &path, status, &text)),
            Err(error) => Some(error.to_string()),
        }
    }

    async fn send(self, token: &CancellationToken, reply: FormReply) {
        self.forget();
        mark_settled(&self.settled, &self.form_id);
        if token.is_cancelled() {
            return;
        }
        match reply {
            FormReply::Answer(answer) => {
                let path = format!("/api/session/{}/form/{}/reply", self.owner, self.form_id);
                let body = json!({ "answer": answer });
                match self
                    .server
                    .post_json_raw(&path, self.dir.as_deref(), &body)
                    .await
                {
                    Ok((status, _)) if status.is_success() => {}
                    // Answered or cancelled elsewhere first (OCF-57).
                    Ok((status, _))
                        if status == reqwest::StatusCode::CONFLICT
                            || status == reqwest::StatusCode::NOT_FOUND =>
                    {
                        tracing::debug!(target: "luna_harness::opencode", form = %self.form_id, "form already settled");
                    }
                    // The answer failed OpenCode's validation: tell the asker
                    // so it continues instead of waiting forever.
                    Ok((status, text)) if status == reqwest::StatusCode::BAD_REQUEST => {
                        let detail = describe_error_envelope(&text)
                            .unwrap_or_else(|| truncate_body(&text));
                        let failed = self
                            .cancel(Some(&format!(
                                "OpenCode rejected the answer LUNA collected: {detail}"
                            )))
                            .await;
                        self.chip(format!(
                            "OpenCode rejected the answer to \"{}\" ({detail}); the request was \
                             cancelled{}",
                            self.title,
                            failed.map(|f| format!(" (cancel failed: {f})")).unwrap_or_default()
                        ))
                        .await;
                    }
                    Ok((status, text)) => {
                        self.chip(format!(
                            "OpenCode did not accept the answer to \"{}\": {}",
                            self.title,
                            post_error_message(&path, status, &text)
                        ))
                        .await;
                    }
                    Err(error) => {
                        self.chip(format!(
                            "OpenCode did not accept the answer to \"{}\": {error}",
                            self.title
                        ))
                        .await;
                    }
                }
            }
            FormReply::Cancel(message) => {
                if let Some(failure) = self.cancel(message.as_deref()).await {
                    self.chip(format!(
                        "OpenCode did not accept LUNA's cancellation of \"{}\": {failure}",
                        self.title
                    ))
                    .await;
                }
            }
        }
    }
}

/// A native `form.created`, with what answering it needs.
pub(super) struct FormAsk<'a> {
    pub(super) props: &'a Value,
    pub(super) server: &'a Server,
    pub(super) dir: Option<&'a str>,
    pub(super) event_tx: &'a mpsc::Sender<Result<AgentEvent, HarnessError>>,
    pub(super) request_input: &'a Arc<RequestInput>,
    pub(super) requests: &'a UserRequests,
    /// The run's turn is active (the `"global"` owner's attribution rule).
    pub(super) turn_active: bool,
}

/// Route a form owned by this run to LUNA's shared surfaces; anything that
/// can't be shown is cancelled with a message (never left to hang).
pub(super) fn handle_form_created(ask: FormAsk<'_>) -> BusOutcome {
    let FormAsk {
        props,
        server,
        dir,
        event_tx,
        request_input,
        requests,
        turn_active,
    } = ask;
    let Some(form) = props.get("form") else {
        return BusOutcome::Continue;
    };
    let (Some(owner), Some(form_id)) = (
        form.get("sessionID").and_then(Value::as_str),
        form.get("id").and_then(Value::as_str),
    ) else {
        return BusOutcome::Continue;
    };
    let global = owner == GLOBAL_FORM_OWNER;
    if !global && !requests.owns(owner) {
        return BusOutcome::Continue;
    }
    if requests.known(form_id) {
        return BusOutcome::Continue;
    }
    let title = form
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("OpenCode form")
        .to_owned();
    let token = requests.track(form_id);
    let responder = FormResponder {
        server: server.detached(),
        owner: owner.to_owned(),
        form_id: form_id.to_owned(),
        title: title.clone(),
        dir: dir.map(str::to_owned),
        failures: event_tx.clone(),
        pending: Arc::clone(&requests.pending),
        settled: Arc::clone(&requests.settled),
    };
    if global && !turn_active {
        tokio::spawn(async move {
            responder
                .send(
                    &token,
                    FormReply::Cancel(Some(
                        "No LUNA turn is active to show this request.".into(),
                    )),
                )
                .await;
        });
        return BusOutcome::Continue;
    }
    let thread = if global { requests.root.as_str() } else { owner };
    let subagent = if global {
        None
    } else {
        requests.subagent_label(owner)
    };
    let plan = match plan_form(form, thread, subagent.as_deref()) {
        Ok(plan) => plan,
        Err(reason) => {
            tokio::spawn(async move {
                responder
                    .chip(format!(
                        "LUNA can't show the OpenCode request \"{title}\": {reason}. It was \
                         cancelled so OpenCode continues."
                    ))
                    .await;
                responder
                    .send(
                        &token,
                        FormReply::Cancel(Some(format!("LUNA can't show this form: {reason}."))),
                    )
                    .await;
            });
            return BusOutcome::Continue;
        }
    };
    if plan.questions.is_empty() {
        let reply = plan.reply(&[]);
        tokio::spawn(async move { responder.send(&token, reply).await });
        return BusOutcome::Continue;
    }
    let receiver = (request_input)(plan.questions.clone());
    tokio::spawn(async move {
        let answers = tokio::select! {
            biased;
            // Stop, the end of the turn, or the form settled elsewhere.
            _ = token.cancelled() => {
                responder.forget();
                return;
            }
            answers = receiver => answers.unwrap_or_default(),
        };
        let reply = plan.reply(&answers);
        responder.send(&token, reply).await;
    });
    BusOutcome::Continue
}

/// `form.replied` / `form.cancelled {id, sessionID}`: the form was settled
/// (by LUNA or elsewhere); close its prompt if one is still open.
pub(super) fn handle_form_settled(props: &Value, requests: &UserRequests) -> BusOutcome {
    if let (Some(id), Some(owner)) = (
        props.get("id").and_then(Value::as_str),
        props.get("sessionID").and_then(Value::as_str),
    ) && (owner == GLOBAL_FORM_OWNER || requests.owns(owner))
    {
        requests.close(id);
    }
    BusOutcome::Continue
}

/// What a resync needs (see [`resync_pending_requests`]).
pub(super) struct Resync<'a> {
    pub(super) server: &'a Server,
    pub(super) dir: Option<&'a str>,
    pub(super) event_tx: &'a mpsc::Sender<Result<AgentEvent, HarnessError>>,
    pub(super) request_input: &'a Arc<RequestInput>,
    pub(super) auto_approve: bool,
    pub(super) permission_policy: &'a luna_proto::PermissionPolicy,
    pub(super) requests: &'a UserRequests,
    pub(super) children: &'a HashMap<String, ChildRun>,
    pub(super) unbound_children: &'a HashMap<String, String>,
    pub(super) turn_active: bool,
}

/// Reconnect hook (OC-04 step 5, OC-08): the bus has no replay, so asks and
/// forms raised during a gap would never reach LUNA. List the location's
/// pending permission requests (`GET /api/permission/request`) and forms
/// (`GET /api/form`); show owned ones LUNA hasn't seen (shown or answered
/// ids are skipped, so nothing is duplicated — OCF-58) and close prompts
/// whose request is no longer pending upstream.
pub(super) async fn resync_pending_requests(ctx: Resync<'_>) {
    let Resync {
        server,
        dir,
        event_tx,
        request_input,
        auto_approve,
        permission_policy,
        requests,
        children,
        unbound_children,
        turn_active,
    } = ctx;
    // Only prompts that existed before the listing can be judged stale.
    let before: Vec<String> = requests
        .pending
        .lock()
        .map(|pending| pending.keys().cloned().collect())
        .unwrap_or_default();
    let list = |value: Value| -> Option<Vec<Value>> {
        unwrap_data(value).as_array().cloned()
    };
    if let Some(asks) = server
        .get_json("/api/permission/request", dir)
        .await
        .ok()
        .and_then(list)
    {
        let upstream: HashSet<&str> = asks
            .iter()
            .filter_map(|ask| ask.get("id").and_then(Value::as_str))
            .collect();
        for id in before.iter().filter(|id| id.starts_with("per")) {
            if !upstream.contains(id.as_str()) {
                requests.close(id);
            }
        }
        for ask in &asks {
            handle_permission_asked(PermissionAsk {
                props: ask,
                event_session: ask.get("sessionID").and_then(Value::as_str),
                children,
                unbound_children,
                server,
                dir,
                event_tx,
                request_input,
                auto_approve,
                permission_policy,
                requests,
            });
        }
    }
    if let Some(forms) = server.get_json("/api/form", dir).await.ok().and_then(list) {
        let upstream: HashSet<&str> = forms
            .iter()
            .filter_map(|form| form.get("id").and_then(Value::as_str))
            .collect();
        for id in before.iter().filter(|id| id.starts_with("frm_")) {
            if !upstream.contains(id.as_str()) {
                requests.close(id);
            }
        }
        for form in forms {
            let props = json!({ "form": form });
            handle_form_created(FormAsk {
                props: &props,
                server,
                dir,
                event_tx,
                request_input,
                requests,
                turn_active,
            });
        }
    }
}
