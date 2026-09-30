//! MCP 2025-11-25 primitive forms and URL prompts; experimental OpenAI
//! verification/form variants remain unsupported until their contracts are implemented.
use luna_proto::{McpFieldSchema, McpInput, McpInputKind, UserInputQuestion};
use serde_json::Value;

pub(super) fn questions(
    params: &Value,
    request_id: &Value,
) -> Result<Vec<UserInputQuestion>, &'static str> {
    if params.to_string().len() > 64 * 1024 {
        return Err("MCP request is too large");
    }
    let bounded = |key: &str, maximum: usize| -> Result<String, &'static str> {
        params
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= maximum)
            .map(str::to_owned)
            .ok_or("Missing or excessive MCP context")
    };
    if !request_id.is_string() && !request_id.is_i64() && !request_id.is_u64() {
        return Err("Invalid MCP request identity");
    }
    let provider_request_id = request_id.to_string();
    if provider_request_id.len() > 256 {
        return Err("MCP request identity is too long");
    }
    let message = bounded("message", luna_proto::MCP_MAX_TEXT)?;
    let binding = McpInput {
        server_name: bounded("serverName", 128)?,
        provider_request_id,
        thread_id: bounded("threadId", 256)?,
        turn_id: bounded("turnId", 256)?,
        expires_at_ms: chrono_time_ms() + 120_000,
        kind: McpInputKind::FormAction,
        launch_url: None,
    };
    let question = |header: String,
                    question: String,
                    options: Vec<String>,
                    kind: McpInputKind,
                    multi_select: bool| {
        let mut meta = binding.clone();
        meta.kind = kind;
        UserInputQuestion {
            id: super::new_message_id(),
            header,
            question,
            options,
            multi_select,
            approval: None,
            mcp: Some(meta),
        }
    };
    match params.get("mode").and_then(Value::as_str) {
        Some("url") => {
            let raw_url = bounded("url", luna_proto::MCP_MAX_TEXT)?;
            let destination = luna_proto::mcp_url_destination(&raw_url)?;
            let elicitation_id = bounded("elicitationId", 256)?;
            let message = message.replace(&raw_url, &destination);
            let mut q = question(
                format!("MCP · {}", binding.server_name),
                format!(
                    "{message}\nOpen {destination} to continue this request, then choose Accept, Decline or Cancel. Opening the website does not submit a response."
                ),
                vec!["Accept".into(), "Decline".into(), "Cancel".into()],
                McpInputKind::Url {
                    destination,
                    elicitation_id,
                },
                false,
            );
            q.mcp.as_mut().unwrap().launch_url = Some(raw_url);
            Ok(vec![q])
        }
        Some("form") => {
            let raw = params
                .get("requestedSchema")
                .and_then(Value::as_object)
                .ok_or("Missing form schema")?;
            if raw.get("type").and_then(Value::as_str) != Some("object")
                || raw.keys().any(|key| {
                    !["$schema", "type", "properties", "required"].contains(&key.as_str())
                })
            {
                return Err("Unsupported MCP form schema");
            }
            if raw.get("$schema").is_some_and(|v| !v.is_string()) {
                return Err("Invalid schema URI");
            }
            let fields = raw
                .get("properties")
                .and_then(Value::as_object)
                .ok_or("Missing form fields")?;
            if fields.len() > luna_proto::MCP_MAX_FIELDS {
                return Err("Too many MCP fields");
            }
            let required = raw
                .get("required")
                .map(|v| v.as_array().ok_or("Invalid required fields"))
                .transpose()?;
            let mut required_names = std::collections::HashSet::new();
            if let Some(required) = required {
                for name in required {
                    let name = name.as_str().ok_or("Invalid required field name")?;
                    if !fields.contains_key(name) || !required_names.insert(name) {
                        return Err("Unknown or repeated required field");
                    }
                }
            }
            let mut result = Vec::new();
            for (name, raw) in fields {
                if name.is_empty() || name.len() > 128 || sensitive_field(name, raw) {
                    return Err("Sensitive or invalid MCP form field; use a URL flow");
                }
                let schema = McpFieldSchema::parse(raw, required_names.contains(name.as_str()))?;
                let title = raw.get("title").and_then(Value::as_str).unwrap_or(name);
                let description = raw.get("description").and_then(Value::as_str).unwrap_or("");
                let options = if schema.value_type == "boolean" {
                    vec!["false".into(), "true".into()]
                } else {
                    schema.choices.iter().map(|c| c.label.clone()).collect()
                };
                let prompt_message = if result.is_empty() {
                    format!("{message}\n")
                } else {
                    String::new()
                };
                let text = format!(
                    "{prompt_message}{} requests {title}. {description}\n{} ({})",
                    binding.server_name,
                    if schema.required {
                        "Required"
                    } else {
                        "Optional"
                    },
                    schema.value_type
                );
                let multi = schema.value_type == "array";
                result.push(question(
                    name.clone(),
                    text,
                    options,
                    McpInputKind::Field {
                        name: name.clone(),
                        schema: Box::new(schema),
                    },
                    multi,
                ));
            }
            result.push(question(
                format!("MCP · {}", binding.server_name),
                format!("{message}\nSubmit these values to {}? Forms are for non-sensitive information; use a website for passwords or credentials.", binding.server_name),
                vec!["Accept".into(), "Decline".into(), "Cancel".into()],
                McpInputKind::FormAction,
                false,
            ));
            Ok(result)
        }
        _ => Err("Unsupported MCP elicitation mode"),
    }
}

fn chrono_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn sensitive_field(name: &str, schema: &Value) -> bool {
    let description = format!(
        "{} {} {}",
        name,
        schema.get("title").and_then(Value::as_str).unwrap_or(""),
        schema
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
    )
    .chars()
    .filter(char::is_ascii_alphanumeric)
    .collect::<String>()
    .to_ascii_lowercase();
    [
        "password",
        "passwd",
        "secret",
        "apikey",
        "accesstoken",
        "creditcard",
        "privatekey",
        "seedphrase",
        "recoveryphrase",
        "credential",
        "passphrase",
        "cardnumber",
        "cvv",
        "socialsecurity",
    ]
    .iter()
    .any(|key| description.contains(key))
}
