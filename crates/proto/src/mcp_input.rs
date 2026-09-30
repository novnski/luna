//! Bounded MCP elicitation metadata and validation shared by adapter, engine and UI.
//! Authorization URLs are deliberately transient: never serialize them to documents.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const MCP_MAX_FIELDS: usize = 16;
pub const MCP_MAX_OPTIONS: usize = 64;
pub const MCP_MAX_TEXT: usize = 4096;
pub const MCP_MAX_CONTENT: usize = 16 * 1024;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpInput {
    pub server_name: String,
    pub provider_request_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub expires_at_ms: i64,
    pub kind: McpInputKind,
    /// Only the live engine pending-input map owns this value. It must never
    /// appear in a journal, synced question, transcript, command or screenshot.
    #[serde(skip)]
    pub launch_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum McpInputKind {
    Field {
        name: String,
        schema: McpFieldSchema,
    },
    FormAction,
    Url {
        destination: String,
        elicitation_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpChoice {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpFieldSchema {
    pub value_type: String,
    pub required: bool,
    pub choices: Vec<McpChoice>,
    pub default: Option<Value>,
    pub min_length: Option<u64>,
    pub max_length: Option<u64>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub min_items: Option<u64>,
    pub max_items: Option<u64>,
    pub format: Option<String>,
}

impl McpFieldSchema {
    pub fn parse(raw: &Value, required: bool) -> Result<Self, &'static str> {
        let object = raw.as_object().ok_or("Field schema must be an object")?;
        let value_type = raw
            .get("type")
            .and_then(Value::as_str)
            .ok_or("Field type is missing")?;
        let allowed: &[&str] = match value_type {
            "string" if raw.get("enum").is_some() || raw.get("oneOf").is_some() => &[
                "type",
                "title",
                "description",
                "enum",
                "enumNames",
                "oneOf",
                "default",
            ],
            "string" => &[
                "type",
                "title",
                "description",
                "minLength",
                "maxLength",
                "format",
                "default",
            ],
            "number" | "integer" => &[
                "type",
                "title",
                "description",
                "minimum",
                "maximum",
                "default",
            ],
            "boolean" => &["type", "title", "description", "default"],
            "array" => &[
                "type",
                "title",
                "description",
                "items",
                "minItems",
                "maxItems",
                "default",
            ],
            _ => return Err("Unsupported MCP field type"),
        };
        if object.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err("Unsupported MCP field constraint");
        }
        for key in ["title", "description"] {
            if let Some(value) = raw.get(key) {
                let text = value.as_str().ok_or("Invalid field description")?;
                if text.len() > MCP_MAX_TEXT {
                    return Err("Field description is too long");
                }
            }
        }
        let count = |key| -> Result<Option<u64>, &'static str> {
            raw.get(key)
                .map(|v| v.as_u64().ok_or("Invalid field limit"))
                .transpose()
        };
        let bound = |key| -> Result<Option<f64>, &'static str> {
            raw.get(key)
                .map(|v| {
                    v.as_f64()
                        .filter(|v| v.is_finite())
                        .ok_or("Invalid numeric limit")
                })
                .transpose()
        };
        let choices = if value_type == "array" {
            let items = raw
                .get("items")
                .and_then(Value::as_object)
                .ok_or("Array choices are missing")?;
            if items
                .keys()
                .any(|key| !["type", "enum", "anyOf"].contains(&key.as_str()))
                || items
                    .get("type")
                    .is_some_and(|v| v.as_str() != Some("string"))
            {
                return Err("Unsupported array item schema");
            }
            parse_choices(&Value::Object(items.clone()), "anyOf")?
        } else if raw.get("enum").is_some() || raw.get("oneOf").is_some() {
            parse_choices(raw, "oneOf")?
        } else {
            Vec::new()
        };
        let format = raw
            .get("format")
            .map(|v| v.as_str().map(str::to_owned).ok_or("Invalid string format"))
            .transpose()?;
        if format
            .as_deref()
            .is_some_and(|f| !["email", "uri", "date", "date-time"].contains(&f))
        {
            return Err("Unsupported string format");
        }
        let mut schema = Self {
            value_type: value_type.into(),
            required,
            choices,
            default: raw.get("default").cloned(),
            min_length: count("minLength")?,
            max_length: count("maxLength")?,
            minimum: bound("minimum")?,
            maximum: bound("maximum")?,
            min_items: count("minItems")?,
            max_items: count("maxItems")?,
            format,
        };
        if schema
            .min_length
            .zip(schema.max_length)
            .is_some_and(|(a, b)| a > b)
            || schema
                .minimum
                .zip(schema.maximum)
                .is_some_and(|(a, b)| a > b)
            || schema
                .min_items
                .zip(schema.max_items)
                .is_some_and(|(a, b)| a > b)
            || schema.min_length.is_some_and(|n| n > MCP_MAX_TEXT as u64)
            || schema
                .min_items
                .is_some_and(|n| n > schema.choices.len() as u64)
        {
            return Err("MCP field limits cannot be satisfied");
        }
        if schema.value_type == "integer"
            && let Some(default) = schema.default.take()
        {
            schema.default = Some(canonical_integer(default)?);
        }
        if let Some(default) = &schema.default {
            schema.validate_value(default)?;
        }
        Ok(schema)
    }

    pub fn value_for_labels(&self, labels: &[String]) -> Result<Option<Value>, &'static str> {
        if labels.is_empty() {
            if let Some(default) = &self.default {
                return Ok(Some(default.clone()));
            }
            if !self.required {
                return Ok(None);
            }
            return Err("This field is required");
        }
        if labels.len() > MCP_MAX_OPTIONS || labels.iter().any(|s| s.len() > MCP_MAX_TEXT) {
            return Err("Field answer is too long");
        }
        let value = if !self.choices.is_empty() {
            let values = labels
                .iter()
                .map(|label| {
                    self.choices
                        .iter()
                        .find(|choice| &choice.label == label)
                        .map(|choice| Value::String(choice.value.clone()))
                        .ok_or("Choose a listed option")
                })
                .collect::<Result<Vec<_>, _>>()?;
            if self.value_type == "array" {
                Value::Array(values)
            } else if values.len() == 1 {
                values[0].clone()
            } else {
                return Err("Choose one option");
            }
        } else {
            if labels.len() != 1 {
                return Err("Enter one value");
            }
            match self.value_type.as_str() {
                "string" => Value::String(labels[0].clone()),
                "integer" => canonical_integer(
                    serde_json::from_str::<Value>(&labels[0]).map_err(|_| "Enter an integer")?,
                )?,
                "number" | "boolean" => serde_json::from_str::<Value>(&labels[0])
                    .map_err(|_| "Enter a valid typed value")?,
                _ => return Err("Unsupported value"),
            }
        };
        self.validate_value(&value)?;
        Ok(Some(value))
    }

    pub fn validate_value(&self, value: &Value) -> Result<(), &'static str> {
        match self.value_type.as_str() {
            "string" => {
                let text = value.as_str().ok_or("Enter text")?;
                let length = text.chars().count() as u64;
                if text.len() > MCP_MAX_TEXT
                    || self.min_length.is_some_and(|n| length < n)
                    || self.max_length.is_some_and(|n| length > n)
                {
                    return Err("Text length is outside the allowed range");
                }
                if !self.choices.is_empty() && !self.choices.iter().any(|c| c.value == text) {
                    return Err("Choose a listed option");
                }
                let valid = match self.format.as_deref() {
                    Some("email") => text.rsplit_once('@').is_some_and(|(local, domain)| {
                        !local.is_empty()
                            && !domain.is_empty()
                            && !text.chars().any(char::is_whitespace)
                            && !local.contains('@')
                            && domain.split('.').all(|p| {
                                !p.is_empty()
                                    && !p.starts_with('-')
                                    && !p.ends_with('-')
                                    && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                            })
                    }),
                    Some("uri") => url::Url::parse(text).is_ok(),
                    Some("date") => {
                        text.len() == 10
                            && chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok()
                    }
                    Some("date-time") => chrono::DateTime::parse_from_rfc3339(text).is_ok(),
                    _ => true,
                };
                if !valid {
                    return Err("Value does not match the requested format");
                }
            }
            "number" | "integer" => {
                let number = value
                    .as_f64()
                    .filter(|v| v.is_finite())
                    .ok_or("Enter a number")?;
                if self.value_type == "integer" && (!value.is_i64() && !value.is_u64()) {
                    return Err("Enter an integer");
                }
                if self.minimum.is_some_and(|n| number < n)
                    || self.maximum.is_some_and(|n| number > n)
                {
                    return Err("Number is outside the allowed range");
                }
            }
            "boolean" => {
                if !value.is_boolean() {
                    return Err("Choose true or false");
                }
            }
            "array" => {
                let values = value.as_array().ok_or("Choose listed options")?;
                if values.len() > MCP_MAX_OPTIONS
                    || self.min_items.is_some_and(|n| values.len() < n as usize)
                    || self.max_items.is_some_and(|n| values.len() > n as usize)
                {
                    return Err("Selection count is outside the allowed range");
                }
                let mut seen = std::collections::HashSet::new();
                for item in values {
                    let item = item.as_str().ok_or("Choose listed options")?;
                    if !seen.insert(item) || !self.choices.iter().any(|c| c.value == item) {
                        return Err("Choose distinct listed options");
                    }
                }
            }
            _ => return Err("Unsupported value"),
        }
        Ok(())
    }
}

fn canonical_integer(value: Value) -> Result<Value, &'static str> {
    if value.is_i64() || value.is_u64() {
        return Ok(value);
    }
    let number = value
        .as_f64()
        .filter(|n| n.is_finite() && n.fract() == 0.0)
        .ok_or("Enter an integer")?;
    if number >= i64::MIN as f64 && number < -(i64::MIN as f64) {
        return Ok(Value::from(number as i64));
    }
    if number >= 0.0 && number < u64::MAX as f64 {
        return Ok(Value::from(number as u64));
    }
    Err("Integer is outside the supported range")
}

fn parse_choices(raw: &Value, titled_key: &str) -> Result<Vec<McpChoice>, &'static str> {
    if raw.get("enumNames").is_some() && raw.get("enum").is_none() {
        return Err("Enum names require enum values");
    }
    if raw.get("enum").is_some() && raw.get(titled_key).is_some() {
        return Err("Ambiguous choices");
    }
    let mut choices = Vec::new();
    if let Some(values) = raw.get("enum").and_then(Value::as_array) {
        let names = raw
            .get("enumNames")
            .map(|v| v.as_array().ok_or("Invalid enum names"))
            .transpose()?;
        if names.is_some_and(|names| names.len() != values.len()) {
            return Err("Enum names must match choices");
        }
        for (index, value) in values.iter().enumerate() {
            let value = value.as_str().ok_or("Invalid enum value")?;
            let name = names
                .and_then(|n| n.get(index))
                .map(|v| v.as_str().ok_or("Invalid enum name"))
                .transpose()?;
            choices.push(McpChoice {
                value: value.into(),
                label: name.map_or_else(|| value.into(), |name| format!("{name} ({value})")),
            });
        }
    } else if let Some(values) = raw.get(titled_key).and_then(Value::as_array) {
        for item in values {
            let object = item.as_object().ok_or("Invalid titled choice")?;
            if object
                .keys()
                .any(|key| !["const", "title"].contains(&key.as_str()))
            {
                return Err("Unsupported titled choice");
            }
            let value = item
                .get("const")
                .and_then(Value::as_str)
                .ok_or("Invalid titled value")?;
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .ok_or("Missing choice title")?;
            choices.push(McpChoice {
                value: value.into(),
                label: format!("{title} ({value})"),
            });
        }
    }
    let mut values = std::collections::HashSet::new();
    let mut labels = std::collections::HashSet::new();
    if choices.is_empty()
        || choices.len() > MCP_MAX_OPTIONS
        || choices.iter().any(|c| {
            c.value.len() > 256
                || c.label.len() > 512
                || !values.insert(c.value.clone())
                || !labels.insert(c.label.clone())
        })
    {
        return Err("Invalid or excessive enum choices");
    }
    Ok(choices)
}

/// Validate using the engine-owned pending questions, never client metadata.
/// Returns exactly the provider response, without echoing invalid user values.
pub fn mcp_response(
    questions: &[crate::UserInputQuestion],
    answers: &[crate::UserInputAnswer],
) -> Result<Value, &'static str> {
    let Some(binding) = questions.first().and_then(|q| q.mcp.as_ref()) else {
        return Err("Missing MCP request");
    };
    let mut seen = std::collections::HashSet::new();
    if answers.iter().any(|answer| {
        answer.labels.len() > MCP_MAX_OPTIONS
            || answer.labels.iter().any(|label| label.len() > MCP_MAX_TEXT)
    }) || answers
        .iter()
        .flat_map(|answer| &answer.labels)
        .map(String::len)
        .sum::<usize>()
        > MCP_MAX_CONTENT
    {
        return Err("MCP answers are too large");
    }
    if answers.len() > MCP_MAX_FIELDS + 1
        || answers.iter().any(|a| {
            !seen.insert(a.question_id.as_str()) || !questions.iter().any(|q| q.id == a.question_id)
        })
    {
        return Err("Answers do not match the pending MCP request");
    }
    let action_question = questions
        .iter()
        .find(|q| {
            q.mcp.as_ref().is_some_and(|m| {
                matches!(m.kind, McpInputKind::FormAction | McpInputKind::Url { .. })
            })
        })
        .ok_or("MCP action is missing")?;
    let action = answers
        .iter()
        .find(|a| a.question_id == action_question.id)
        .ok_or("Choose Accept, Decline or Cancel")?;
    let [action] = action.labels.as_slice() else {
        return Err("Choose one MCP action");
    };
    match action.as_str() {
        "Decline" => return Ok(serde_json::json!({"action":"decline"})),
        "Cancel" => return Ok(serde_json::json!({"action":"cancel"})),
        "Accept" => {}
        _ => return Err("Choose Accept, Decline or Cancel"),
    }
    let mut content = Map::new();
    for question in questions {
        let meta = question.mcp.as_ref().ok_or("Mixed MCP question set")?;
        if meta.server_name != binding.server_name
            || meta.provider_request_id != binding.provider_request_id
            || meta.thread_id != binding.thread_id
            || meta.turn_id != binding.turn_id
        {
            return Err("Mixed MCP request bindings");
        }
        if let McpInputKind::Field { name, schema } = &meta.kind {
            let labels = answers
                .iter()
                .find(|a| a.question_id == question.id)
                .map(|a| a.labels.as_slice())
                .unwrap_or(&[]);
            if let Some(value) = schema.value_for_labels(labels)? {
                content.insert(name.clone(), value);
            }
        }
    }
    if content.len() > MCP_MAX_FIELDS
        || Value::Object(content.clone()).to_string().len() > MCP_MAX_CONTENT
    {
        return Err("MCP content is too large");
    }
    Ok(
        if matches!(
            action_question.mcp.as_ref().map(|m| &m.kind),
            Some(McpInputKind::Url { .. })
        ) {
            serde_json::json!({"action":"accept"})
        } else {
            serde_json::json!({"action":"accept", "content":content})
        },
    )
}

/// Display only the origin. The query, path and fragment can contain secrets.
pub fn mcp_url_destination(raw: &str) -> Result<String, &'static str> {
    if raw.len() > MCP_MAX_TEXT {
        return Err("Authorization URL is too long");
    }
    let url = url::Url::parse(raw).map_err(|_| "Invalid authorization URL")?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Authorization URL must be an HTTP(S) website without embedded credentials");
    }
    Ok(url.origin().ascii_serialization())
}

impl std::fmt::Debug for McpInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpInput")
            .field("server_name", &self.server_name)
            .field("provider_request_id", &self.provider_request_id)
            .field("thread_id", &self.thread_id)
            .field("turn_id", &self.turn_id)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
