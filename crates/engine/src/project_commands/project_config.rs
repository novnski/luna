//! AW-11 repository layer: `.luna/project.toml`, read from a checkout as
//! untrusted input and layered UNDER this device's personal commands.
//!
//! Reading the file never runs anything. Any problem with it refuses the
//! whole file with a visible reason; personal commands never depend on it.
//! A repository command is runnable only while this device's accepted digest
//! equals the digest of its current canonical content, and a personal command
//! with the same id always takes precedence.
use super::action_digest;
use luna_proto::{
    PROJECT_CONFIG_FILE, PROJECT_CONFIG_VERSION, ProjectAction, ProjectCommandTrust,
    ProjectConfigState, ProjectRepositoryCommand, ProjectRepositoryConfig,
};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    io::Read as _,
    path::{Path, PathBuf},
};

const MAX_FILE_BYTES: u64 = 64 * 1024;
/// Matches the personal limit in `ProjectCommands::configure`.
const MAX_COMMANDS: usize = 24;

/// Schema version 1. Unknown keys anywhere refuse the file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileV1 {
    /// Checked before this strict parse so a newer version reads as such.
    #[serde(rename = "version")]
    _version: i64,
    #[serde(default)]
    commands: Vec<FileCommand>,
}

/// No setup or wait keys: a repository command never runs on worktree
/// creation, and always waits for a running personal setup.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct FileCommand {
    id: String,
    name: String,
    command: String,
    #[serde(default)]
    preview_url: Option<String>,
}

/// The validated declarations, or why the whole file is not used.
pub(super) enum Parsed {
    Absent,
    Loaded(Vec<ProjectAction>),
    Refused(String),
}

/// Resolve a project folder the way a task checkout is resolved.
pub(super) fn checkout_root(path: &str) -> Result<PathBuf, String> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err("The project folder is not an absolute path.".into());
    }
    let root = path
        .canonicalize()
        .map_err(|error| format!("The project folder is unavailable: {error}."))?;
    if !root.is_dir() {
        return Err("The project folder is not a directory.".into());
    }
    Ok(root)
}

/// Read and validate `<checkout>/.luna/project.toml`. `checkout` must be
/// canonical. Never fails: every problem is a state the UI shows.
pub(super) fn read(checkout: &Path) -> Parsed {
    match load(checkout) {
        Ok(None) => Parsed::Absent,
        Ok(Some(text)) => match parse(&text) {
            Ok(actions) => Parsed::Loaded(actions),
            Err(reason) => Parsed::Refused(reason),
        },
        Err(reason) => Parsed::Refused(reason),
    }
}

fn load(checkout: &Path) -> Result<Option<String>, String> {
    let path = checkout.join(PROJECT_CONFIG_FILE);
    let resolved = match path.canonicalize() {
        Ok(resolved) => resolved,
        // No file, no `.luna`, or a `.luna` that is not a directory: the
        // repository simply has no configuration. A link that cannot be
        // resolved is present but unusable.
        Err(error) => {
            return match std::fs::symlink_metadata(&path) {
                Err(_) => Ok(None),
                Ok(_) => Err(format!("The file cannot be resolved: {error}.")),
            };
        }
    };
    if !resolved.starts_with(checkout) {
        return Err(
            "The file resolves outside the checkout. LUNA reads project configuration only from inside it."
                .into(),
        );
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // Never block on a FIFO, and never follow a link swapped in after
        // the containment check above.
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options
        .open(&resolved)
        .map_err(|error| format!("The file cannot be opened: {error}."))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("The file cannot be read: {error}."))?;
    let too_large = || format!("The file is larger than {} KiB.", MAX_FILE_BYTES / 1024);
    if !metadata.is_file() {
        return Err("The path is not a regular file.".into());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("The file cannot be read: {error}."))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(too_large());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| String::from("The file is not UTF-8 text."))
}

fn line(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

/// Bidirectional formatting characters can make the text a reviewer sees
/// differ from the bytes that run (CVE-2021-42574).
fn hidden_direction(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= 64
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn parse(text: &str) -> Result<Vec<ProjectAction>, String> {
    let at = |span: Option<std::ops::Range<usize>>| {
        span.map(|span| format!(" (line {})", line(text, span.start)))
            .unwrap_or_default()
    };
    let table = text
        .parse::<toml::Table>()
        .map_err(|error| format!("Invalid TOML{}: {}", at(error.span()), error.message()))?;
    match table.get("version") {
        Some(toml::Value::Integer(PROJECT_CONFIG_VERSION)) => {}
        Some(toml::Value::Integer(other)) => {
            return Err(format!(
                "Unsupported schema version {other}. This LUNA reads version {PROJECT_CONFIG_VERSION} only."
            ));
        }
        Some(_) => {
            return Err(format!(
                "`version` must be the integer {PROJECT_CONFIG_VERSION}."
            ));
        }
        None => return Err(format!("Missing `version = {PROJECT_CONFIG_VERSION}`.")),
    }
    let file: FileV1 = toml::from_str(text).map_err(|error| {
        format!(
            "Invalid configuration{}: {}",
            at(error.span()),
            error.message()
        )
    })?;
    if file.commands.len() > MAX_COMMANDS {
        return Err(format!(
            "The file declares {} commands; at most {MAX_COMMANDS} are supported.",
            file.commands.len()
        ));
    }
    let mut ids = HashSet::new();
    let mut actions = Vec::with_capacity(file.commands.len());
    for command in file.commands {
        let label = format!("Command {:?}", command.id);
        if !valid_id(&command.id) {
            return Err(format!(
                "{label}: an ID has 1–64 letters, digits, '.', '_' or '-' and starts with a letter or digit."
            ));
        }
        if !ids.insert(command.id.clone()) {
            return Err(format!("{label} is declared twice."));
        }
        let name = command.name.trim();
        if name.is_empty() || name.len() > 120 || name.chars().any(char::is_control) {
            return Err(format!(
                "{label}: the name needs 1–120 bytes without control characters."
            ));
        }
        if command.command.trim().is_empty()
            || command.command.len() > 16384
            || command
                .command
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(format!(
                "{label}: the command needs 1–16384 bytes; line feeds and tabs are its only allowed control characters."
            ));
        }
        if name.chars().any(hidden_direction)
            || command.command.chars().any(hidden_direction)
            || command
                .preview_url
                .as_deref()
                .is_some_and(|url| url.chars().any(hidden_direction))
        {
            return Err(format!(
                "{label} contains invisible text-direction characters, so its displayed text could differ from what runs."
            ));
        }
        if let Some(url) = &command.preview_url {
            let valid = url::Url::parse(url).is_ok_and(|parsed| {
                matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some()
            });
            if !valid {
                return Err(format!(
                    "{label}: `preview-url` needs a full HTTP or HTTPS URL."
                ));
            }
        }
        actions.push(ProjectAction {
            id: command.id,
            name: name.to_string(),
            command: command.command,
            setup_on_worktree_create: false,
            wait_for_setup: true,
            preview_url: command.preview_url,
        });
    }
    Ok(actions)
}

/// The repository layer with this device's decisions. `trust` holds
/// `(command id, accepted digest)` rows from the device-local ledger.
pub(super) fn layer(
    checkout: &Path,
    parsed: Parsed,
    personal: &[ProjectAction],
    trust: &[(String, String)],
) -> ProjectRepositoryConfig {
    let accepted: HashMap<&str, &str> = trust
        .iter()
        .map(|(id, digest)| (id.as_str(), digest.as_str()))
        .collect();
    let (state, detail, actions) = match parsed {
        Parsed::Absent => (ProjectConfigState::Absent, None, Vec::new()),
        Parsed::Loaded(actions) => (ProjectConfigState::Loaded, None, actions),
        Parsed::Refused(reason) => (ProjectConfigState::Refused, Some(reason), Vec::new()),
    };
    let commands = actions
        .into_iter()
        .map(|action| {
            let digest = action_digest(&action);
            let trust = match accepted.get(action.id.as_str()) {
                Some(accepted) if *accepted == digest => ProjectCommandTrust::Accepted,
                Some(_) => ProjectCommandTrust::Changed,
                None => ProjectCommandTrust::Unaccepted,
            };
            let overridden = personal.iter().any(|own| own.id == action.id);
            ProjectRepositoryCommand {
                action,
                digest,
                trust,
                overridden,
            }
        })
        .collect();
    ProjectRepositoryConfig {
        checkout: checkout.to_string_lossy().into_owned(),
        file: PROJECT_CONFIG_FILE.into(),
        state,
        detail,
        commands,
    }
}

pub(super) fn unavailable(folder: &str, detail: String) -> ProjectRepositoryConfig {
    ProjectRepositoryConfig {
        checkout: folder.into(),
        file: PROJECT_CONFIG_FILE.into(),
        state: ProjectConfigState::Unavailable,
        detail: Some(detail),
        commands: Vec::new(),
    }
}

/// Precedence: personal commands first, then accepted repository commands
/// that no personal command overrides.
pub(super) fn runnable(
    personal: &[ProjectAction],
    repository: &ProjectRepositoryConfig,
) -> Vec<ProjectAction> {
    personal
        .iter()
        .cloned()
        .chain(
            repository
                .commands
                .iter()
                .filter(|command| command.runnable())
                .map(|command| command.action.clone()),
        )
        .collect()
}
