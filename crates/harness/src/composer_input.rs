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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImageKind {
    Png,
    Jpeg,
    Gif,
    WebP,
    Tiff,
    Bmp,
    Svg,
    Unknown,
}

/// Magic bytes as `image` 0.25's format guesser reads them (classic TIFF in
/// both byte orders; BigTIFF is not guessed). SVG is recognised only to give
/// an explicit vector-image rejection.
fn sniff_image(bytes: &[u8]) -> ImageKind {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        ImageKind::Png
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        ImageKind::Jpeg
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        ImageKind::Gif
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        ImageKind::WebP
    } else if bytes.starts_with(b"II\x2a\x00") || bytes.starts_with(b"MM\x00\x2a") {
        ImageKind::Tiff
    } else if bytes.starts_with(b"BM") {
        ImageKind::Bmp
    } else {
        let text = String::from_utf8_lossy(bytes);
        let text = text.trim_start_matches('\u{feff}').trim_start();
        let markup = text.starts_with("<?xml")
            || text.starts_with("<!--")
            || text.starts_with("<!DOCTYPE")
            || text.starts_with("<svg");
        if markup && text.contains("<svg") {
            ImageKind::Svg
        } else {
            ImageKind::Unknown
        }
    }
}

/// Raster inputs the standard Codex CLI release decodes on THIS execution host.
/// Codex 0.159 (`rust-v0.159.0`) links one unified `image` 0.25: its image
/// utility enables PNG/JPEG/GIF/WebP and the TUI's `arboard` adds TIFF on macOS
/// and BMP on Windows. Codex decodes and re-encodes the original bytes; this
/// header check is admission only, never a decode or a transcode.
fn host_decodes(kind: ImageKind) -> bool {
    match kind {
        ImageKind::Png | ImageKind::Jpeg | ImageKind::Gif | ImageKind::WebP => true,
        ImageKind::Tiff => cfg!(target_os = "macos"),
        ImageKind::Bmp => cfg!(windows),
        ImageKind::Svg | ImageKind::Unknown => false,
    }
}

fn host_image_formats() -> &'static str {
    if cfg!(target_os = "macos") {
        "PNG, JPEG, GIF, WebP or TIFF"
    } else if cfg!(windows) {
        "PNG, JPEG, GIF, WebP or BMP"
    } else {
        "PNG, JPEG, GIF or WebP"
    }
}

/// Validate before mailbox admission as well as adapter dispatch. A disappeared
/// file rejects the input instead of silently reducing an image prompt to text.
/// Unsupported formats reject the whole input as `RejectedBeforeAcceptance` so
/// the caller keeps the text, images and typed selections for recovery.
pub fn validate_composer_inputs(
    attachments: &[String],
    selections: &[ComposerInput],
) -> Result<(), HarnessError> {
    for path in attachments {
        let file = readable_file(path)?;
        let mut header = Vec::with_capacity(512);
        file.take(512).read_to_end(&mut header).map_err(|error| {
            HarnessError::RejectedBeforeAcceptance(format!(
                "Image input is unreadable ({path}): {error}"
            ))
        })?;
        let kind = sniff_image(&header);
        if host_decodes(kind) {
            continue;
        }
        let formats = host_image_formats();
        let detail = match kind {
            ImageKind::Svg => format!(
                "SVG is vector artwork, which Codex cannot read as an image ({path}). Convert it to {formats}; your message and selections remain available."
            ),
            ImageKind::Tiff => format!(
                "Codex on this computer cannot read TIFF images ({path}). Convert it to {formats}; your message and selections remain available."
            ),
            ImageKind::Bmp => format!(
                "Codex on this computer cannot read BMP images ({path}). Convert it to {formats}; your message and selections remain available."
            ),
            _ => format!(
                "Unsupported native image input: {path}. Attach a {formats} image; your message and selections remain available."
            ),
        };
        return Err(HarnessError::RejectedBeforeAcceptance(detail));
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

/// LUNA's attachment transport (engine `doc_host.rs`, UI `attachments.rs`):
/// the prompt ends with this header and one `- <path>` line per image. The
/// trailer stays in the transcript for display/restore and for providers
/// that open files with tools.
const ATTACHMENT_PROMPT_HEADER: &str = "Attached images (local files — open them to view):";

/// Codex receives these images natively as `localImage`; telling it to also
/// "open" them only causes a redundant `viewImage` call. Drop the lines for
/// natively sent paths and keep any path that is not sent natively.
fn without_native_image_trailer(prompt: &str, native: &HashSet<&str>) -> String {
    let marker = format!("\n\n{ATTACHMENT_PROMPT_HEADER}\n");
    let Some(at) = prompt.rfind(&marker) else {
        return prompt.to_owned();
    };
    let mut kept = Vec::new();
    for line in prompt[at + marker.len()..].lines() {
        let Some(path) = line.strip_prefix("- ") else {
            // Not LUNA's transport shape: leave the user's text alone.
            return prompt.to_owned();
        };
        if !native.contains(path) {
            kept.push(line);
        }
    }
    let body = &prompt[..at];
    if kept.is_empty() {
        body.to_owned()
    } else {
        format!("{body}{marker}{}", kept.join("\n"))
    }
}

/// Build native input once for initial, steered and rejected-steer fallback turns.
pub(crate) fn codex_input(
    prompt: &str,
    attachments: &[String],
    selections: &[ComposerInput],
) -> Result<Vec<Value>, HarnessError> {
    validate_composer_inputs(attachments, selections)?;
    let mut input = Vec::new();
    let native: HashSet<&str> = attachments.iter().map(String::as_str).collect();
    let prompt = if native.is_empty() {
        prompt.to_owned()
    } else {
        without_native_image_trailer(prompt, &native)
    };
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
