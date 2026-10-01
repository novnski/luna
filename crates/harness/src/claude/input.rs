//! One typed user-input builder for every Claude Code turn: the first line of
//! a fresh or resumed process and every steer written to a live one.
//!
//! Contract (installed CLI 2.1.286, stream-json input):
//! - Images ride as base64 `image` blocks. The CLI re-detects each image's
//!   format from its bytes, then resizes and compresses it to the API limits
//!   (2000x2000, about 5 MiB of base64) before the request. A block it cannot
//!   decode is silently replaced by a model-only text block, so LUNA rejects
//!   what it can recognise as undecodable before anything is written.
//! - Claude expands a skill or slash command only from the LAST text block,
//!   and only when that block starts with `/name`. Prose before the command
//!   stays in an earlier text block; images sit between the two.
//! - The API rejects empty text blocks, so an image-only turn sends images
//!   only.
//!
//! Every rejection is [`HarnessError::RejectedBeforeAcceptance`] and happens
//! before a process is spawned or a stdin line is written, so callers keep
//! the exact draft envelope for recovery.
use std::io::Read as _;
use std::path::{Component, Path};

use luna_proto::{ComposerInput, ReasoningLevel};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::catalog::{ULTRATHINK_PREFIX, apply_ultrathink};
use super::wire;
use crate::HarnessError;

/// Largest staged image LUNA sends inline. The CLI downsizes anything above
/// the API limit itself; this bound only keeps one stdin line reasonable.
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
/// Aggregate raw image bytes in one user message (one stdin line).
const MAX_MESSAGE_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
/// Engine admission inspects this prefix; the adapter re-checks full bytes.
const ADMISSION_PREFIX_BYTES: u64 = 256 * 1024;
const KEPT: &str = "your message, images and selections remain available";

fn reject(detail: String) -> HarnessError {
    HarnessError::RejectedBeforeAcceptance(detail)
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// Validate staged images and typed selections for Claude Code without
/// building a message. Used by engine admission before a provider starts or a
/// steer is routed; the adapter repeats the full check on the bytes it sends.
pub fn validate_inputs(
    attachments: &[String],
    selections: &[ComposerInput],
) -> Result<(), HarnessError> {
    skill_selections(selections)?;
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u64;
    for path in attachments {
        if !seen.insert(path.as_str()) {
            continue;
        }
        let size = image_size(path)?;
        total += size;
        if total > MAX_MESSAGE_IMAGE_BYTES {
            return Err(aggregate_error(total));
        }
        let mut prefix = Vec::new();
        std::fs::File::open(path)
            .and_then(|file| {
                file.take(ADMISSION_PREFIX_BYTES)
                    .read_to_end(&mut prefix)
                    .map(|_| ())
            })
            .map_err(|error| reject(format!("Image is unreadable ({path}): {error}; {KEPT}.")))?;
        inspect(path, &prefix, size <= ADMISSION_PREFIX_BYTES)?;
    }
    Ok(())
}

/// What one Claude user line carries, for logs and evidence.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InputManifest {
    pub blocks: Vec<&'static str>,
    pub images: Vec<ImageManifest>,
    pub skill: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ImageManifest {
    pub path: String,
    pub media_type: &'static str,
    pub bytes: usize,
    pub sha256: String,
}

impl InputManifest {
    /// Compact log form: block order, each image's type/size/hash, skill.
    pub(crate) fn summary(&self) -> String {
        let images = self
            .images
            .iter()
            .map(|image| {
                format!(
                    "{} {} {}B sha256:{}",
                    image.path, image.media_type, image.bytes, image.sha256
                )
            })
            .collect::<Vec<_>>();
        format!(
            "blocks=[{}] images=[{}] skill={}",
            self.blocks.join(","),
            images.join("; "),
            self.skill.as_deref().unwrap_or("-")
        )
    }
}

pub(crate) struct UserLine {
    pub line: String,
    pub manifest: InputManifest,
}

/// Build the stdin user line for one turn or steer. Text-only input keeps the
/// historical plain-string content, byte for byte.
pub(crate) async fn user_line(
    prompt: &str,
    reasoning: Option<ReasoningLevel>,
    attachments: &[String],
    selections: &[ComposerInput],
) -> Result<UserLine, HarnessError> {
    let skills = skill_selections(selections)?;
    let mut images = Vec::new();
    let mut manifests = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u64;
    for path in attachments {
        if !seen.insert(path.as_str()) {
            continue;
        }
        image_size(path)?;
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|error| reject(format!("Image is unreadable ({path}): {error}; {KEPT}.")))?;
        // The file may have changed since admission: judge the bytes we send.
        let size = bytes.len() as u64;
        if size > MAX_IMAGE_BYTES {
            return Err(size_error(path, size));
        }
        total += size;
        if total > MAX_MESSAGE_IMAGE_BYTES {
            return Err(aggregate_error(total));
        }
        let media_type = inspect(path, &bytes, true)?;
        manifests.push(ImageManifest {
            path: path.clone(),
            media_type,
            bytes: bytes.len(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        });
        use base64::Engine as _;
        images.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": media_type,
                "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
            },
        }));
    }

    let (mut leading, mut text, skill) = match skills.last() {
        Some(_) => {
            let (leading, command, name) = plan_skill_dispatch(prompt, &skills);
            (leading, command, Some(name))
        }
        None => (None, prompt.to_owned(), None),
    };
    if reasoning == Some(ReasoningLevel::Ultrathink) {
        if text.starts_with('/') {
            // A prefix would turn the command into prose; keep it ahead.
            let before = leading.take().unwrap_or_default();
            leading = Some(format!("{ULTRATHINK_PREFIX}{before}").trim_end().to_owned());
        } else {
            text = apply_ultrathink(reasoning, &text);
        }
    }

    let mut manifest = InputManifest {
        blocks: Vec::new(),
        images: manifests,
        skill,
    };
    if images.is_empty() && leading.is_none() {
        manifest.blocks.push("text");
        return Ok(UserLine {
            line: wire::user_message_line(&text),
            manifest,
        });
    }
    let mut blocks = Vec::new();
    if let Some(leading) = leading.filter(|text| !text.trim().is_empty()) {
        manifest.blocks.push("text");
        blocks.push(json!({"type": "text", "text": leading}));
    }
    for image in images {
        manifest.blocks.push("image");
        blocks.push(image);
    }
    if !text.trim().is_empty() {
        manifest.blocks.push("text");
        blocks.push(json!({"type": "text", "text": text}));
    }
    let line = json!({
        "type": "user",
        "message": { "role": "user", "content": Value::Array(blocks) },
        "parent_tool_use_id": null,
    })
    .to_string();
    Ok(UserLine { line, manifest })
}

fn size_error(path: &str, size: u64) -> HarnessError {
    reject(format!(
        "Image is too large for Claude Code ({path}, {}; the limit is {}). Resize it or remove it; {KEPT}.",
        mib(size),
        mib(MAX_IMAGE_BYTES)
    ))
}

fn aggregate_error(total: u64) -> HarnessError {
    reject(format!(
        "These images are too large to send together ({} total; the limit is {} per message). Send fewer or smaller images; {KEPT}.",
        mib(total),
        mib(MAX_MESSAGE_IMAGE_BYTES)
    ))
}

/// Host-ownership and size checks shared by admission and dispatch: the path
/// must name a regular file on THIS host (refs resolve before dispatch).
fn image_size(path: &str) -> Result<u64, HarnessError> {
    if path.starts_with("pending://") {
        return Err(reject(format!(
            "Image has not arrived on the computer running this task ({path}). Wait for the upload or attach it again; {KEPT}."
        )));
    }
    if !Path::new(path).is_absolute() {
        return Err(reject(format!(
            "Image needs an absolute path on the computer running this task: {path}; {KEPT}."
        )));
    }
    let metadata = std::fs::metadata(path).map_err(|error| {
        reject(format!(
            "Image is not available on the computer running this task ({path}): {error}; {KEPT}."
        ))
    })?;
    if !metadata.is_file() {
        return Err(reject(format!(
            "Image is not a regular file: {path}; {KEPT}."
        )));
    }
    if metadata.len() == 0 {
        return Err(reject(format!("Image is empty: {path}; {KEPT}.")));
    }
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(size_error(path, metadata.len()));
    }
    Ok(metadata.len())
}

/// Media type from the bytes (never the extension), plus header sanity and
/// the CMYK check the CLI cannot decode. `complete` means `bytes` is the whole
/// file, so a truncated structure is corruption rather than a short prefix.
fn inspect(path: &str, bytes: &[u8], complete: bool) -> Result<&'static str, HarnessError> {
    let corrupt = || {
        reject(format!(
            "Image is damaged or incomplete ({path}). Export it again; {KEPT}."
        ))
    };
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let (Some(width), Some(height)) = (bytes.get(16..20), bytes.get(20..24)) else {
            return Err(corrupt());
        };
        if bytes.get(12..16) != Some(&b"IHDR"[..]) || be32(width) == 0 || be32(height) == 0 {
            return Err(corrupt());
        }
        return Ok("image/png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return match jpeg_components(bytes) {
            JpegScan::Components(4) => Err(reject(format!(
                "Claude Code cannot decode CMYK JPEG images ({path}). Re-save it as an RGB PNG or JPEG; {KEPT}."
            ))),
            JpegScan::Components(_) => Ok("image/jpeg"),
            JpegScan::Truncated if !complete => Ok("image/jpeg"),
            JpegScan::Truncated | JpegScan::Malformed => Err(corrupt()),
        };
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return if bytes.len() >= 13 {
            Ok("image/gif")
        } else {
            Err(corrupt())
        };
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return if matches!(
            bytes.get(12..16),
            Some(b"VP8 ") | Some(b"VP8L") | Some(b"VP8X")
        ) {
            Ok("image/webp")
        } else {
            Err(corrupt())
        };
    }
    let kind = describe_unsupported(bytes);
    Err(reject(format!(
        "Claude Code accepts PNG, JPEG, GIF or WebP images; {path} is {kind}. Convert it or remove it; {KEPT}."
    )))
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn describe_unsupported(bytes: &[u8]) -> &'static str {
    let ftyp_brand = (bytes.get(4..8) == Some(b"ftyp"))
        .then(|| bytes.get(8..12))
        .flatten();
    if bytes.starts_with(b"II\x2a\x00") || bytes.starts_with(b"MM\x00\x2a") {
        "a TIFF image"
    } else if bytes.starts_with(b"BM") {
        "a BMP image"
    } else if bytes.starts_with(b"%PDF") {
        "a PDF document"
    } else if matches!(ftyp_brand, Some(b"avif") | Some(b"avis")) {
        "an AVIF image"
    } else if ftyp_brand.is_some_and(|brand| brand.starts_with(b"hei") || brand == b"mif1") {
        "a HEIC/HEIF image"
    } else {
        let text = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
        if text.contains("<svg") {
            "SVG vector artwork"
        } else {
            "not a recognised image"
        }
    }
}

enum JpegScan {
    Components(u8),
    Truncated,
    Malformed,
}

/// Walk JPEG marker segments to the first frame header (SOFn) and return its
/// component count: 1 grey, 3 YCbCr/RGB, 4 CMYK/YCCK.
fn jpeg_components(bytes: &[u8]) -> JpegScan {
    let mut at = 2;
    loop {
        match bytes.get(at) {
            None => return JpegScan::Truncated,
            Some(0xFF) => {}
            Some(_) => return JpegScan::Malformed,
        }
        while bytes.get(at) == Some(&0xFF) {
            at += 1;
        }
        let Some(&marker) = bytes.get(at) else {
            return JpegScan::Truncated;
        };
        at += 1;
        match marker {
            0x01 | 0xD0..=0xD8 => continue,
            0xD9 | 0xDA => return JpegScan::Malformed,
            _ => {}
        }
        let Some(length) = bytes
            .get(at..at + 2)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
        else {
            return JpegScan::Truncated;
        };
        if length < 2 {
            return JpegScan::Malformed;
        }
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            // length(2) precision(1) height(2) width(2) components(1)
            let Some(header) = bytes.get(at..at + 8) else {
                return JpegScan::Truncated;
            };
            let width = u16::from_be_bytes([header[5], header[6]]);
            return if width == 0 || header[7] == 0 {
                JpegScan::Malformed
            } else {
                JpegScan::Components(header[7])
            };
        }
        at += length;
    }
}

/// Validated Claude skill invocations, in selection order. Plugin and app
/// mentions are Codex integrations and are rejected here.
fn skill_selections(selections: &[ComposerInput]) -> Result<Vec<(String, String)>, HarnessError> {
    let mut skills: Vec<(String, String)> = Vec::new();
    for selection in selections {
        match selection {
            ComposerInput::Mention { path, .. } => {
                return Err(reject(format!(
                    "Plugin and app mentions ({path}) are Codex integrations; Claude Code cannot run them. Remove the mention or choose Codex; {KEPT}."
                )));
            }
            ComposerInput::Skill { name, path } => {
                let name = claude_skill(name, path)?;
                if !skills.iter().any(|(_, known)| known == path) {
                    skills.push((name, path.clone()));
                }
            }
        }
    }
    Ok(skills)
}

fn claude_skill(name: &str, path: &str) -> Result<String, HarnessError> {
    let valid_name = name.len() <= 128
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '.'));
    if !valid_name {
        return Err(reject(format!(
            "\"{name}\" is not a Claude Code skill command name; {KEPT}."
        )));
    }
    let file = Path::new(path);
    let in_component = |wanted: &str| {
        file.components()
            .any(|part| matches!(part, Component::Normal(part) if part == wanted))
    };
    if !file.is_absolute() {
        return Err(reject(format!(
            "Skill needs an absolute path on the computer running this task: {path}; {KEPT}."
        )));
    }
    if in_component(".codex") || in_component(".agents") {
        return Err(reject(format!(
            "Skill \"{name}\" was selected from Codex's catalog ({path}); Claude Code cannot run it. Remove it or choose Codex; {KEPT}."
        )));
    }
    let dir = file.parent();
    let shaped = file.file_name().is_some_and(|f| f == "SKILL.md")
        && dir
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|f| f == "skills");
    // A custom slash command (`commands/**/<name>.md`) is invoked the same
    // way; CC-12's effective catalog offers both.
    let command = file.extension().is_some_and(|ext| ext == "md") && in_component("commands");
    let config_root = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|root| !root.is_empty())
        .is_some_and(|root| file.starts_with(root));
    if !(shaped || command) || !(in_component(".claude") || config_root) {
        return Err(reject(format!(
            "Skill \"{name}\" is not in a Claude Code skills or commands folder ({path}); {KEPT}."
        )));
    }
    let metadata = std::fs::metadata(file).map_err(|error| {
        reject(format!(
            "Skill \"{name}\" is not available on the computer running this task ({path}): {error}; {KEPT}."
        ))
    })?;
    if !metadata.is_file() {
        return Err(reject(format!(
            "Skill \"{name}\" is not a regular file ({path}); {KEPT}."
        )));
    }
    let body = std::fs::read_to_string(file).map_err(|error| {
        reject(format!(
            "Skill \"{name}\" is unreadable ({path}): {error}; {KEPT}."
        ))
    })?;
    let dir_name = dir
        .and_then(Path::file_name)
        .and_then(|f| f.to_str())
        .unwrap_or_default();
    let leaf = name.rsplit(':').next().unwrap_or(name);
    if leaf != dir_name && frontmatter_name(&body) != Some(name) {
        return Err(reject(format!(
            "Skill \"{name}\" does not match its folder ({path}); reselect it; {KEPT}."
        )));
    }
    Ok(name.to_owned())
}

fn frontmatter_name(body: &str) -> Option<&str> {
    let rest = body.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    rest[..end].lines().find_map(|line| {
        line.trim()
            .strip_prefix("name:")
            .map(|value| value.trim().trim_matches(['"', '\'']))
    })
}

/// Split the prompt around the last skill chip projection
/// (`[@label](skill://<percent-encoded path>)`) of a selected skill. The
/// result is `(prose before, "/name prose after", name)`; earlier chips become
/// inline `/name` text because Claude expands one command per message. With no
/// projection in the text, the whole prompt becomes the command's arguments.
fn plan_skill_dispatch(
    prompt: &str,
    skills: &[(String, String)],
) -> (Option<String>, String, String) {
    let links: Vec<(std::ops::Range<usize>, &str)> = skill_links(prompt)
        .into_iter()
        .filter_map(|(range, path)| {
            skills
                .iter()
                .find(|(_, known)| *known == path)
                .map(|(name, _)| (range, name.as_str()))
        })
        .collect();
    let Some((last, name)) = links.last().cloned() else {
        let (name, _) = skills.last().expect("caller checked a skill exists");
        let command = if prompt.trim().is_empty() {
            format!("/{name}")
        } else {
            format!("/{name} {}", prompt.trim_start())
        };
        return (None, command.trim_end().to_owned(), name.clone());
    };
    let mut leading = String::new();
    let mut at = 0;
    for (range, name) in &links[..links.len() - 1] {
        leading.push_str(&prompt[at..range.start]);
        leading.push('/');
        leading.push_str(name);
        at = range.end;
    }
    leading.push_str(&prompt[at..last.start]);
    let leading = leading.trim_end().to_owned();
    let trailing = &prompt[last.end..];
    let separator = if trailing.is_empty() || trailing.starts_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    let command = format!("/{name}{separator}{trailing}")
        .trim_end()
        .to_owned();
    (
        (!leading.is_empty()).then_some(leading),
        command,
        name.to_owned(),
    )
}

/// `[@label](skill://...)` projections with their decoded host paths.
fn skill_links(prompt: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut links = Vec::new();
    let mut search = 0;
    while let Some(start) = prompt[search..].find("[@").map(|offset| search + offset) {
        let Some(close) = label_close(prompt, start + 1) else {
            search = start + 1;
            continue;
        };
        let target_start = close + 2;
        let Some(end) = prompt[target_start..]
            .find(')')
            .map(|offset| target_start + offset + 1)
        else {
            break;
        };
        match prompt[target_start..end - 1]
            .strip_prefix("skill://")
            .and_then(percent_decode)
        {
            Some(path) => {
                links.push((start..end, path));
                search = end;
            }
            None => search = start + 1,
        }
    }
    links
}

fn label_close(text: &str, start: usize) -> Option<usize> {
    let mut escaped = false;
    for (at, ch) in text[start..].char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == ']' && text[start + at + 1..].starts_with('(') {
            return Some(start + at);
        }
    }
    None
}

fn percent_decode(encoded: &str) -> Option<String> {
    let raw = encoded.as_bytes();
    let mut bytes = Vec::with_capacity(raw.len());
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'%' {
            let hex = std::str::from_utf8(raw.get(at + 1..at + 3)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            bytes.push(raw[at]);
            at += 1;
        }
    }
    String::from_utf8(bytes).ok()
}
