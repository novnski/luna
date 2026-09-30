//! Host-native composer selections and staged images. No label/Markdown parsing.
use crate::HarnessError;
use luna_proto::ComposerInput;
use serde_json::{Value, json};
use std::{collections::HashSet, io::Read, path::Path};

fn readable_file(path: &str) -> Result<std::fs::File, HarnessError> {
    if !Path::new(path).is_absolute() {
        return Err(HarnessError::RejectedBeforeAcceptance(format!(
            "Native input needs an absolute host path: {path}"
        )));
    }
    let metadata = std::fs::metadata(path).map_err(|error| {
        HarnessError::RejectedBeforeAcceptance(format!(
            "Native input is unavailable ({path}): {error}"
        ))
    })?;
    if !metadata.is_file() {
        return Err(HarnessError::RejectedBeforeAcceptance(format!(
            "Native input is not a regular file: {path}"
        )));
    }
    let file = std::fs::File::open(path).map_err(|error| {
        HarnessError::RejectedBeforeAcceptance(format!(
            "Native input is unavailable ({path}): {error}"
        ))
    })?;
    if !file
        .metadata()
        .map_err(|error| HarnessError::RejectedBeforeAcceptance(error.to_string()))?
        .is_file()
    {
        return Err(HarnessError::RejectedBeforeAcceptance(format!(
            "Native input is not a regular file: {path}"
        )));
    }
    Ok(file)
}

/// Validate before mailbox admission as well as adapter dispatch. A disappeared
/// file rejects the input instead of silently reducing an image prompt to text.
pub fn validate_composer_inputs(
    attachments: &[String],
    selections: &[ComposerInput],
) -> Result<(), HarnessError> {
    for path in attachments {
        let mut file = readable_file(path)?;
        let mut header = [0u8; 12];
        let n = file.read(&mut header).map_err(|error| {
            HarnessError::RejectedBeforeAcceptance(format!(
                "Image input is unreadable ({path}): {error}"
            ))
        })?;
        let bytes = &header[..n];
        let supported = bytes.starts_with(b"\x89PNG\r\n\x1a\n")
            || bytes.starts_with(b"\xff\xd8\xff")
            || bytes.starts_with(b"GIF87a")
            || bytes.starts_with(b"GIF89a")
            || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"));
        if !supported {
            return Err(HarnessError::RejectedBeforeAcceptance(format!(
                "Unsupported native image input: {path}. Attach a PNG, JPEG, GIF or WebP image."
            )));
        }
    }
    for selection in selections {
        let (name, path) = match selection {
            ComposerInput::Mention { name, path } | ComposerInput::Skill { name, path } => {
                (name, path)
            }
        };
        if name.trim().is_empty() || name.contains('\0') {
            return Err(HarnessError::RejectedBeforeAcceptance(
                "Native selection needs a display name".into(),
            ));
        }
        match selection {
            ComposerInput::Mention { .. } => {
                let id = path
                    .strip_prefix("plugin://")
                    .or_else(|| path.strip_prefix("app://"));
                if id.is_none_or(|id| {
                    id.is_empty() || id.chars().any(|c| c.is_whitespace() || c.is_control())
                }) {
                    return Err(HarnessError::RejectedBeforeAcceptance(format!(
                        "Invalid native mention identity: {path}"
                    )));
                }
            }
            ComposerInput::Skill { .. } => {
                readable_file(path)?;
            }
        }
    }
    Ok(())
}

/// Build native input once for initial, steered and rejected-steer fallback turns.
pub(crate) fn codex_input(
    prompt: &str,
    attachments: &[String],
    selections: &[ComposerInput],
) -> Result<Vec<Value>, HarnessError> {
    validate_composer_inputs(attachments, selections)?;
    let mut input = Vec::new();
    if !prompt.trim().is_empty() {
        input.push(json!({"type":"text", "text":prompt}));
    }
    let mut seen = HashSet::new();
    for path in attachments {
        if seen.insert(("localImage", path.as_str())) {
            input.push(json!({"type":"localImage", "path":path}));
        }
    }
    for selection in selections {
        let (kind, name, path) = match selection {
            ComposerInput::Mention { name, path } => ("mention", name, path),
            ComposerInput::Skill { name, path } => ("skill", name, path),
        };
        if seen.insert((kind, path.as_str())) {
            input.push(json!({"type":kind, "name":name, "path":path}));
        }
    }
    Ok(input)
}
