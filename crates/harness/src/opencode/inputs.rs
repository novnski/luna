//! OC-09: what an OpenCode turn carries besides its text, on every route
//! (first prompt, boundary steer, warm follow-up, slash command, retry and
//! restart re-dispatch): staged images as native `files[]` entries and typed
//! skill selections as `skills[]`, plus the admission rules shared by engine
//! admission and adapter dispatch.
//!
//! Contract (OpenCode 2.0.22, `core/src/session/prompt.ts` and
//! `core/src/session/runner/to-llm-message.ts`):
//! - `files[].uri` is a `file:` URI the SERVER reads when it admits the
//!   prompt, so the path must exist on the host that runs the server (the
//!   chat's host). Over 20 MiB is an `AttachmentError`. The server sniffs the
//!   type from the bytes; PNG/JPEG/GIF/WebP reach the model as media, next to
//!   an "Attached file: <path>" text part the server adds itself, so LUNA's
//!   own path trailer would only repeat it.
//! - A model whose `capabilities.input` lacks `image` has the media part
//!   stripped without notice (`unsupportedParts`), so LUNA rejects instead.
//! - `skills[{id}]` names skills from `GET /api/skill`; an unknown id is a
//!   `SkillNotFoundError`.
//!
//! Every admission rejection is [`HarnessError::RejectedBeforeAcceptance`],
//! raised before a server boots or a prompt is posted, so callers keep the
//! exact draft (text, images and selections) for recovery.

use std::collections::HashSet;
use std::io::Read as _;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use luna_proto::ComposerInput;
use sha2::{Digest, Sha256};

use super::*;

/// OpenCode's own per-attachment ceiling (`MAX_ATTACHMENT_BYTES`).
pub(super) const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
/// Bound decoded output separately from the compressed file size.
const MAX_DECODED_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
const KEPT: &str = "your message, images and selections remain available";

fn reject(detail: String) -> HarnessError {
    HarnessError::RejectedBeforeAcceptance(detail)
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// Validate staged images and typed selections for an OpenCode turn without
/// starting anything. Engine admission calls this for every route that
/// admits a user turn; the adapter repeats the image checks on the bytes it
/// is about to name. `model` (`provider/model`) adds the image-capability
/// check when this process has seen that model in a catalog.
pub fn validate_inputs(
    model: Option<&str>,
    attachments: &[String],
    selections: &[ComposerInput],
) -> Result<(), HarnessError> {
    skill_selections(selections)?;
    if attachments.is_empty() {
        return Ok(());
    }
    if let Some(model) = model
        && model_accepts_images(model) == Some(false)
    {
        return Err(text_only_model(model));
    }
    for path in unique(attachments) {
        check_image(path)?;
    }
    Ok(())
}

pub(super) fn text_only_model(model: &str) -> HarnessError {
    reject(format!(
        "The OpenCode model {model} does not accept images. Choose a model with image input or remove the images; {KEPT}."
    ))
}

// ---------------------------------------------------------------------------
// Image capability, per model, from every catalog this process read
// ---------------------------------------------------------------------------

fn image_capabilities() -> &'static Mutex<HashMap<String, bool>> {
    static CAPABILITIES: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    CAPABILITIES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record `provider/model` → accepts images, from a catalog read (picker
/// discovery or a run's own location-scoped catalog). A later read wins.
pub(super) fn remember_image_capabilities(entries: impl IntoIterator<Item = (String, bool)>) {
    if let Ok(mut known) = image_capabilities().lock() {
        known.extend(entries);
    }
}

/// `None` = never seen in a catalog here (custom or not yet discovered):
/// the run re-checks against its own catalog before posting.
pub fn model_accepts_images(model: &str) -> Option<bool> {
    image_capabilities().lock().ok()?.get(model).copied()
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

/// What the adapter verified about one image right before naming it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ImageFacts {
    pub(super) media_type: &'static str,
    pub(super) bytes: u64,
    pub(super) sha256: String,
}

/// First occurrence of each path, in order: one `files[]` entry per image.
pub(super) fn unique(attachments: &[String]) -> impl Iterator<Item = &String> {
    let mut seen = HashSet::new();
    attachments
        .iter()
        .filter(move |path| seen.insert(path.as_str()))
}

/// Host ownership, size, type (from the bytes, never the extension) and a
/// bounded decode. The server reads the same file when it admits the
/// prompt, so these are the bytes the model will see.
pub(super) fn check_image(path: &str) -> Result<ImageFacts, HarnessError> {
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
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| {
            file.take(MAX_IMAGE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map(|_| ())
        })
        .map_err(|error| reject(format!("Image is unreadable ({path}): {error}; {KEPT}.")))?;
    let size = bytes.len() as u64;
    if size > MAX_IMAGE_BYTES {
        return Err(size_error(path, size));
    }
    if size == 0 {
        return Err(reject(format!("Image is empty: {path}; {KEPT}.")));
    }
    let media_type = sniff(path, &bytes)?;
    validate_decodable(path, &bytes, media_type)?;
    Ok(ImageFacts {
        media_type,
        bytes: size,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
    })
}

fn size_error(path: &str, size: u64) -> HarnessError {
    reject(format!(
        "Image is too large for OpenCode ({path}, {}; the limit is {}). Resize it or remove it; {KEPT}.",
        mib(size),
        mib(MAX_IMAGE_BYTES)
    ))
}

/// The formats OpenCode shows a model as an image. SVG is named on purpose:
/// it is text to OpenCode and would arrive as markup, not a picture.
fn sniff(path: &str, bytes: &[u8]) -> Result<&'static str, HarnessError> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("image/png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Ok("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Ok("image/gif");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return Ok("image/webp");
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]);
    let head = head.trim_start_matches('\u{feff}').trim_start();
    let markup = head.starts_with("<?xml")
        || head.starts_with("<!--")
        || head.starts_with("<!DOCTYPE")
        || head.starts_with("<svg");
    if markup && head.contains("<svg") {
        return Err(reject(format!(
            "SVG is vector artwork, which OpenCode cannot show a model as an image ({path}). Convert it to PNG, JPEG, GIF or WebP; {KEPT}."
        )));
    }
    Err(reject(format!(
        "OpenCode accepts PNG, JPEG, GIF or WebP images; this file is not one ({path}). Convert it or remove it; {KEPT}."
    )))
}

/// A header alone admits truncated payloads that the server would fail to
/// decode after accepting the prompt. Decode under output limits instead.
fn validate_decodable(path: &str, bytes: &[u8], media_type: &str) -> Result<(), HarnessError> {
    use image::{ImageDecoder as _, ImageFormat, ImageReader, Limits};
    let format = match media_type {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        _ => ImageFormat::WebP,
    };
    let too_large = || {
        reject(format!(
            "Image is too large to decode safely ({path}). Export a smaller image; {KEPT}."
        ))
    };
    let decode_error = |error: image::ImageError| match error {
        image::ImageError::Limits(_) => too_large(),
        _ => reject(format!(
            "Image is damaged or incomplete ({path}). Export it again; {KEPT}."
        )),
    };
    let mut limits = Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    let mut reader = ImageReader::with_format(std::io::Cursor::new(bytes), format);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(decode_error)?;
    if decoder.total_bytes() > MAX_DECODED_IMAGE_BYTES {
        return Err(too_large());
    }
    image::DynamicImage::from_decoder(decoder).map_err(decode_error)?;
    Ok(())
}

/// The prompt text OpenCode receives: LUNA's "Attached images (local files
/// …)" trailer drops the lines for images sent natively as `files[]`
/// (OpenCode names each one itself); the transcript keeps the trailer.
pub(super) fn native_prompt_text(prompt: &str, attachments: &[String]) -> String {
    if attachments.is_empty() {
        return prompt.to_owned();
    }
    let native: HashSet<&str> = attachments.iter().map(String::as_str).collect();
    crate::composer_input::without_native_image_trailer(prompt, &native)
}

// ---------------------------------------------------------------------------
// Typed skills (like Claude: skills only; plugin/app mentions stay Codex's)
// ---------------------------------------------------------------------------

/// The skill selections of one turn as `(name, path)`. Plugin and app
/// mentions are Codex identities and are refused.
pub(super) fn skill_selections(
    selections: &[ComposerInput],
) -> Result<Vec<(&str, &str)>, HarnessError> {
    let mut skills = Vec::new();
    for selection in selections {
        match selection {
            ComposerInput::Mention { .. } => {
                return Err(reject(
                    "OpenCode cannot receive plugin or app selections. Remove them or choose Codex; your draft is kept.".into(),
                ));
            }
            ComposerInput::Skill { name, path, .. } => {
                let name = name.trim().trim_start_matches(['/', '$']);
                if name.is_empty() || name.chars().any(char::is_control) {
                    return Err(reject(
                        "A skill selection needs a name; your draft is kept.".into(),
                    ));
                }
                skills.push((name, path.as_str()));
            }
        }
    }
    Ok(skills)
}

/// One `GET /api/skill` entry; `content` is not retained.
#[derive(Debug, serde::Deserialize)]
pub(super) struct V2Skill {
    pub(super) id: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) path: String,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct V2SkillList {
    #[serde(default)]
    pub(super) data: Vec<V2Skill>,
}

/// Resolve typed selections against the location's skill catalog: by path
/// first (the composer's identity), then by name. Order kept, ids deduped.
pub(super) fn resolve_skill_refs(
    selections: &[(&str, &str)],
    catalog: &[V2Skill],
) -> Result<Vec<Value>, HarnessError> {
    let mut ids: Vec<&str> = Vec::new();
    for (name, path) in selections {
        let by_path = (!path.is_empty())
            .then(|| {
                catalog.iter().find(|skill| {
                    !skill.path.is_empty()
                        && (skill.path == *path
                            || Path::new(&skill.path).parent() == Some(Path::new(path)))
                })
            })
            .flatten();
        let skill = by_path.or_else(|| catalog.iter().find(|skill| skill.name == *name));
        let Some(skill) = skill else {
            return Err(reject(format!(
                "OpenCode has no skill named \"{name}\" for this project. Refresh the skill list or remove the selection; {KEPT}."
            )));
        };
        if !ids.contains(&skill.id.as_str()) {
            ids.push(skill.id.as_str());
        }
    }
    Ok(ids.into_iter().map(|id| json!({ "id": id })).collect())
}

impl Server {
    /// `skills[]` for a turn: empty without selections (no request).
    pub(super) async fn skill_refs(
        &self,
        directory: Option<&str>,
        selections: &[ComposerInput],
    ) -> Result<Vec<Value>, HarnessError> {
        let skills = skill_selections(selections)?;
        if skills.is_empty() {
            return Ok(Vec::new());
        }
        let list: V2SkillList = self.get("/api/skill", directory).await?;
        resolve_skill_refs(&skills, &list.data)
    }
}
