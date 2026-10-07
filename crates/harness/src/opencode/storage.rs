//! Where an OpenCode server that LUNA spawns keeps its storage, for the
//! readers that run inside LUNA instead of the server: the Go limits reader,
//! usage history, the skills config root and the native extensions view.
//!
//! The server inherits LUNA's environment and then runs through the
//! executable LUNA chose: the `OPENCODE_EXECUTABLE` override, else this
//! process's last resolution. A QA profile wrapper
//! (`scripts/qa-opencode-env.sh`) re-exports the four XDG roots,
//! `OPENCODE_CONFIG_DIR` and `OPENCODE_DB` before it `exec`s the real binary,
//! so reading LUNA's own environment would open the user's real store from a
//! QA app. This module replays the static environment changes of that
//! wrapper chain over LUNA's environment and derives the directories the way
//! OpenCode 2 does (`packages/core/src/global.ts` and `database.ts`): the
//! xdg-basedir roots (`$XDG_*_HOME`, else `$HOME/.local/share` and
//! `$HOME/.config`) joined with `opencode`, `OPENCODE_CONFIG_DIR` over the
//! config root, and `OPENCODE_DB` absolute or relative to the data root.
//!
//! Failure cases covered:
//! - OCS-01 A QA app reads the user's real store: readers used LUNA's
//!   environment while the server ran through a profile wrapper that moved
//!   every root. The wrapper's exports now overlay that environment.
//! - OCS-02 A wrapper changes a variable in a way that cannot be evaluated
//!   without running it: command substitution, `source`, `eval`, `read`, a
//!   change inside a conditional or loop, `env -i`, a here-document. That
//!   variable (or every variable when the change cannot be attributed) is
//!   indeterminate, and a directory that depends on it is reported as
//!   unavailable instead of guessed from the default home.
//! - OCS-03 Chains: symlinks and static `exec` targets (absolute, beside the
//!   script through `$(dirname "$0")`, or a bare name on the effective
//!   `PATH`) are followed for up to eight hops, each hop's changes applied in
//!   order. A deeper chain, a loop, or a dynamic `exec` target is
//!   indeterminate.
//! - OCS-04 Not every launcher is a shell script: a native binary ends the
//!   chain; a launcher for another interpreter (OpenCode's npm `node`
//!   launcher) passes its environment through and ends it too; an unreadable
//!   or oversized shell script is indeterminate.
//! - OCS-05 Shell semantics: an assignment reaches the server only when its
//!   name is exported (inherited names are, `set -a` exports later ones),
//!   `unset` removes the value and the export, assignments in front of
//!   `exec` reach the server only, values are single-quoted literals or
//!   double-quoted and bare words with `$NAME`, `${NAME}`, `$0` and a leading
//!   `~` expanded.
//! - OCS-06 A relative XDG root or `OPENCODE_CONFIG_DIR` depends on the
//!   server's working directory, which a reader does not share: unavailable.
//! - OCS-07 `OPENCODE_DB=:memory:` has no store to read; empty values count
//!   as unset, as they do for OpenCode.
//! - OCS-08 Values (a wrapper may export provider keys) stay in memory: the
//!   `Debug` form and every log line carry paths only.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Wrapper hops followed before the chain counts as indeterminate.
const MAX_HOPS: usize = 8;
/// Larger shell launchers are not wrappers LUNA can evaluate.
const MAX_SCRIPT_BYTES: u64 = 64 * 1024;
const SHELLS: [&str; 5] = ["sh", "bash", "dash", "zsh", "ksh"];
const BLOCK_OPEN: [&str; 7] = ["if", "case", "for", "while", "until", "select", "{"];
const BLOCK_CLOSE: [&str; 4] = ["fi", "esac", "done", "}"];
const BLOCK_MIDDLE: [&str; 6] = ["then", "else", "elif", "do", "in", "function"];
/// Builtins whose effect on variables LUNA does not evaluate.
const OPAQUE: [&str; 10] = [
    ".", "source", "eval", "read", "getopts", "readonly", "declare", "typeset", "local", "alias",
];

/// A storage variable that the OpenCode executable's wrapper changes in a
/// way LUNA cannot evaluate without running it (OCS-02, OCS-03, OCS-06).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageIndeterminate;

impl std::fmt::Display for StorageIndeterminate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "the OpenCode executable changes its storage environment in a way LUNA cannot evaluate",
        )
    }
}

impl std::error::Error for StorageIndeterminate {}

#[derive(Clone)]
enum Value {
    Set(OsString),
    Unset,
    Unknown,
}

/// The storage environment of the OpenCode server LUNA would spawn now.
#[derive(Clone)]
pub struct OpencodeStorage {
    base: HashMap<String, OsString>,
    changed: HashMap<String, Value>,
    export: HashMap<String, bool>,
    allexport: bool,
    all_unknown: bool,
    executable: Option<PathBuf>,
    wrappers: Vec<PathBuf>,
}

impl std::fmt::Debug for OpencodeStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Paths only (OCS-08).
        f.debug_struct("OpencodeStorage")
            .field("executable", &self.executable)
            .field("wrappers", &self.wrappers)
            .field("indeterminate", &self.all_unknown)
            .finish_non_exhaustive()
    }
}

impl OpencodeStorage {
    /// The environment the next OpenCode server would see: LUNA's own,
    /// replayed through `OPENCODE_EXECUTABLE`, else the executable this
    /// process last resolved. No process is started and no version probe
    /// runs; only the executable and its wrapper scripts are read.
    pub fn current() -> Self {
        let executable = std::env::var_os("OPENCODE_EXECUTABLE")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .or_else(|| super::last_opencode_resolution().map(|resolution| resolution.path));
        let storage = Self::for_executable(executable.as_deref());
        tracing::debug!(
            target: "luna_harness::opencode",
            executable = ?storage.executable,
            wrappers = ?storage.wrappers,
            indeterminate = storage.all_unknown,
            "derived the OpenCode storage environment"
        );
        storage
    }

    /// [`OpencodeStorage::current`] for an explicit executable (or none).
    pub fn for_executable(executable: Option<&Path>) -> Self {
        let base = std::env::vars_os()
            .filter_map(|(name, value)| Some((name.into_string().ok()?, value)))
            .collect();
        let mut storage = Self {
            base,
            changed: HashMap::new(),
            export: HashMap::new(),
            allexport: false,
            all_unknown: false,
            executable: executable.map(Path::to_path_buf),
            wrappers: Vec::new(),
        };
        if let Some(executable) = executable {
            storage.follow(executable);
        }
        storage
    }

    /// The executable whose environment this describes.
    pub fn executable(&self) -> Option<&Path> {
        self.executable.as_deref()
    }

    /// Shell wrappers whose changes were applied, outermost first.
    pub fn wrappers(&self) -> &[PathBuf] {
        &self.wrappers
    }

    /// One variable as the server sees it. Empty counts as unset (OCS-07).
    pub fn var(&self, name: &str) -> Result<Option<OsString>, StorageIndeterminate> {
        if self.all_unknown {
            return Err(StorageIndeterminate);
        }
        match self.get(name) {
            Value::Unknown => Err(StorageIndeterminate),
            Value::Set(value) if self.exported(name) && !value.is_empty() => Ok(Some(value)),
            _ => Ok(None),
        }
    }

    /// `os.homedir()` for the server: `HOME` (Windows: `USERPROFILE`).
    pub fn home(&self) -> Result<Option<PathBuf>, StorageIndeterminate> {
        if let Some(home) = self.var("HOME")? {
            return Ok(Some(PathBuf::from(home)));
        }
        if cfg!(windows) {
            return Ok(self.var("USERPROFILE")?.map(PathBuf::from));
        }
        Ok(None)
    }

    /// OpenCode's data root (`Global.Path.data`): database, `auth.json`,
    /// logs and legacy `storage/`.
    pub fn data_dir(&self) -> Result<Option<PathBuf>, StorageIndeterminate> {
        Ok(self
            .xdg("XDG_DATA_HOME", ".local/share")?
            .map(|root| root.join("opencode")))
    }

    /// OpenCode's global config root (`Global.Path.config`), which holds
    /// `opencode.json`; `OPENCODE_CONFIG_DIR` is loaded beside it.
    pub fn global_config_dir(&self) -> Result<Option<PathBuf>, StorageIndeterminate> {
        Ok(self
            .xdg("XDG_CONFIG_HOME", ".config")?
            .map(|root| root.join("opencode")))
    }

    /// The config root OpenCode's services use (`Global.make().config`):
    /// `OPENCODE_CONFIG_DIR`, else the global config root. Its `skills/`
    /// folder is OpenCode's global skill library.
    pub fn config_dir(&self) -> Result<Option<PathBuf>, StorageIndeterminate> {
        match self.var("OPENCODE_CONFIG_DIR")? {
            Some(value) => absolute(value).map(Some),
            None => self.global_config_dir(),
        }
    }

    /// The server's database: `OPENCODE_DB` (absolute, or relative to the
    /// data root; `:memory:` has none), else `<data>/opencode.db`.
    pub fn database(&self) -> Result<Option<PathBuf>, StorageIndeterminate> {
        match self.var("OPENCODE_DB")? {
            Some(value) if value == ":memory:" => Ok(None),
            Some(value) if Path::new(&value).is_absolute() => Ok(Some(PathBuf::from(value))),
            Some(value) => Ok(self.data_dir()?.map(|data| data.join(value))),
            None => Ok(self.data_dir()?.map(|data| data.join("opencode.db"))),
        }
    }

    fn xdg(&self, variable: &str, fallback: &str) -> Result<Option<PathBuf>, StorageIndeterminate> {
        match self.var(variable)? {
            Some(value) => absolute(value).map(Some),
            None => Ok(self.home()?.map(|home| home.join(fallback))),
        }
    }

    // ---------------------------------------------------------------- shell

    fn get(&self, name: &str) -> Value {
        match self.changed.get(name) {
            Some(value) => value.clone(),
            None => self
                .base
                .get(name)
                .map_or(Value::Unset, |value| Value::Set(value.clone())),
        }
    }

    fn exported(&self, name: &str) -> bool {
        self.export
            .get(name)
            .copied()
            .unwrap_or_else(|| self.base.contains_key(name))
    }

    fn assign(&mut self, name: &str, value: Option<String>) {
        self.changed.insert(
            name.to_owned(),
            value.map_or(Value::Unknown, |value| Value::Set(value.into())),
        );
        if self.allexport {
            self.export.insert(name.to_owned(), true);
        }
    }

    fn unset(&mut self, name: &str) {
        self.changed.insert(name.to_owned(), Value::Unset);
        self.export.insert(name.to_owned(), false);
    }

    fn mark_unknown(&mut self, name: &str) {
        self.changed.insert(name.to_owned(), Value::Unknown);
    }

    /// Follow symlinks and wrapper scripts down to the native binary.
    fn follow(&mut self, start: &Path) {
        // `$0` is the path a hop was invoked by, before symlinks resolve.
        let mut invoked = start.to_path_buf();
        let mut current = start.to_path_buf();
        let mut seen = HashSet::new();
        for _ in 0..MAX_HOPS * 4 {
            if !seen.insert(current.clone()) {
                self.all_unknown = true;
                return;
            }
            let Ok(metadata) = std::fs::symlink_metadata(&current) else {
                // Missing: the server cannot start from it either.
                return;
            };
            if metadata.file_type().is_symlink() {
                let Ok(target) = std::fs::read_link(&current) else {
                    self.all_unknown = true;
                    return;
                };
                current = match current.parent() {
                    Some(parent) if target.is_relative() => parent.join(target),
                    _ => target,
                };
                continue;
            }
            if !metadata.is_file() {
                return;
            }
            match launcher(&current, metadata.len()) {
                Launcher::Passthrough => return,
                Launcher::Opaque => {
                    self.all_unknown = true;
                    return;
                }
                Launcher::Shell(text) => {
                    if self.wrappers.len() >= MAX_HOPS {
                        self.all_unknown = true;
                        return;
                    }
                    self.wrappers.push(current.clone());
                    match self.run_script(&invoked, &text) {
                        Some(next) => {
                            invoked = next.clone();
                            current = next;
                        }
                        None => return,
                    }
                }
            }
        }
        self.all_unknown = true;
    }

    /// Apply one wrapper's statements; the static `exec` target, if any.
    fn run_script(&mut self, invoked: &Path, text: &str) -> Option<PathBuf> {
        let mut depth: isize = 0;
        for line in logical_lines(text) {
            if line.contains("<<") {
                // A here-document's body would read as statements.
                self.all_unknown = true;
                return None;
            }
            let Ok(tokens) = tokenize(&line) else {
                self.all_unknown = true;
                return None;
            };
            let mut delta = 0;
            let mut keyword = false;
            for token in &tokens {
                if let Token::Word(word) = token
                    && let Some(literal) = word.literal()
                {
                    if BLOCK_OPEN.contains(&literal) {
                        delta += 1;
                        keyword = true;
                    } else if BLOCK_CLOSE.contains(&literal) {
                        delta -= 1;
                        keyword = true;
                    } else if BLOCK_MIDDLE.contains(&literal) {
                        keyword = true;
                    }
                }
            }
            let operator = tokens.iter().any(|token| matches!(token, Token::Operator));
            if depth > 0 || keyword || operator {
                self.scan_conditional(&tokens);
                if self.all_unknown {
                    return None;
                }
                depth = (depth + delta).max(0);
                continue;
            }
            for statement in tokens.split(|token| matches!(token, Token::Separator)) {
                let words: Vec<&Word> = statement
                    .iter()
                    .filter_map(|token| match token {
                        Token::Word(word) => Some(word),
                        _ => None,
                    })
                    .collect();
                match self.statement(&words, invoked) {
                    Step::Continue => {}
                    Step::Exec(target) => return target,
                }
                if self.all_unknown {
                    return None;
                }
            }
        }
        None
    }

    /// A line whose statements may not all run: every name it could assign,
    /// export or unset becomes indeterminate (OCS-02).
    fn scan_conditional(&mut self, tokens: &[Token]) {
        let mut declaring = false;
        let mut setting = false;
        for token in tokens {
            let Token::Word(word) = token else {
                declaring = false;
                setting = false;
                continue;
            };
            for name in &word.defaults {
                self.mark_unknown(name);
            }
            if let Some(name) = &word.assign {
                self.mark_unknown(name);
                continue;
            }
            let Some(literal) = word.literal() else {
                continue;
            };
            if setting
                && (literal == "allexport"
                    || (literal.starts_with(['-', '+'])
                        && !literal.starts_with("--")
                        && literal.contains('a')))
            {
                self.all_unknown = true;
                return;
            }
            match literal {
                "export" | "unset" => declaring = true,
                "set" => setting = true,
                "exec" => {
                    // A conditional exec: the next hop is not known.
                    self.all_unknown = true;
                    return;
                }
                literal if OPAQUE.contains(&literal) => {
                    self.all_unknown = true;
                    return;
                }
                name if declaring && is_name(name) => self.mark_unknown(name),
                _ => {}
            }
        }
    }

    fn statement(&mut self, words: &[&Word], invoked: &Path) -> Step {
        for word in words {
            for name in &word.defaults {
                // `${NAME:=value}` assigns when NAME is unset or empty.
                if !matches!(self.get(name), Value::Set(value) if !value.is_empty()) {
                    self.mark_unknown(name);
                }
            }
        }
        let prefix = words
            .iter()
            .take_while(|word| word.assign.is_some())
            .count();
        let Some(command) = words.get(prefix) else {
            for word in words {
                let value = self.eval(word, invoked);
                self.assign(word.assign.as_deref().unwrap_or_default(), value);
            }
            return Step::Continue;
        };
        let rest = &words[prefix + 1..];
        match self.eval(command, invoked).as_deref() {
            Some("export") => {
                for word in rest {
                    if let Some(name) = &word.assign {
                        let value = self.eval(word, invoked);
                        self.assign(name, value);
                        self.export.insert(name.clone(), true);
                        continue;
                    }
                    match self.eval(word, invoked).as_deref() {
                        Some("-p") | Some("--") => {}
                        Some(name) if is_name(name) => {
                            self.export.insert(name.to_owned(), true);
                        }
                        _ => self.all_unknown = true,
                    }
                }
            }
            Some("unset") => {
                for word in rest {
                    match self.eval(word, invoked).as_deref() {
                        Some("-v") | Some("--") => {}
                        // Functions only; no variable changes.
                        Some("-f") => break,
                        Some(name) if is_name(name) => self.unset(name),
                        _ => self.all_unknown = true,
                    }
                }
            }
            Some("set") => {
                let mut option = false;
                for word in rest {
                    let Some(flag) = self.eval(word, invoked) else {
                        continue;
                    };
                    if option {
                        if flag == "allexport" {
                            self.allexport = true;
                        }
                        option = false;
                    } else if flag == "-o" || flag == "+o" {
                        option = flag == "-o";
                    } else if !flag.starts_with("--") && flag.contains('a') {
                        if flag.starts_with('-') {
                            self.allexport = true;
                        } else if flag.starts_with('+') {
                            self.allexport = false;
                        }
                    }
                }
            }
            Some("exec") => return self.exec(&words[..prefix], rest, invoked),
            Some(name) if OPAQUE.contains(&name) => self.all_unknown = true,
            // Any other command runs in its own process; assignments in
            // front of it are its own.
            _ => {}
        }
        Step::Continue
    }

    fn exec(&mut self, prefix: &[&Word], words: &[&Word], invoked: &Path) -> Step {
        let mut index = 0;
        while let Some(word) = words.get(index)
            && word.assign.is_none()
            && let Some(option) = self.eval(word, invoked).filter(|w| w.starts_with('-'))
        {
            index += if option == "-a" { 2 } else { 1 };
        }
        let mut child: Vec<(String, Option<String>)> = prefix
            .iter()
            .chain(
                words[index.min(words.len())..]
                    .iter()
                    .take_while(|word| word.assign.is_some()),
            )
            .map(|word| {
                (
                    word.assign.clone().unwrap_or_default(),
                    self.eval(word, invoked),
                )
            })
            .collect();
        index += words[index.min(words.len())..]
            .iter()
            .take_while(|word| word.assign.is_some())
            .count();
        if words
            .get(index)
            .and_then(|word| self.eval(word, invoked))
            .is_some_and(|word| word == "env" || word.ends_with("/env"))
        {
            index += 1;
            while let Some(word) = words.get(index) {
                if let Some(name) = &word.assign {
                    child.push((name.clone(), self.eval(word, invoked)));
                    index += 1;
                } else if self
                    .eval(word, invoked)
                    .is_none_or(|word| word.starts_with('-'))
                {
                    // `env -i`, `env -u NAME`, or an unknown option.
                    self.all_unknown = true;
                    return Step::Exec(None);
                } else {
                    break;
                }
            }
        }
        let Some(target) = words.get(index) else {
            // `exec` without a command changes nothing here.
            return Step::Continue;
        };
        for (name, value) in child {
            self.assign(&name, value);
            self.export.insert(name, true);
        }
        let target = self.exec_target(target, invoked);
        if target.is_none() {
            self.all_unknown = true;
        }
        Step::Exec(target)
    }

    fn exec_target(&self, word: &Word, invoked: &Path) -> Option<PathBuf> {
        let parent = invoked.parent()?;
        for prefix in [
            "\"$(dirname \"$0\")\"/",
            "\"$(dirname \"$0\")/",
            "$(dirname \"$0\")/",
            "\"$(dirname $0)\"/",
            "\"$(dirname $0)/",
            "$(dirname $0)/",
        ] {
            if let Some(rest) = word.raw.strip_prefix(prefix) {
                let rest = rest.trim_matches(['"', '\'']);
                if rest.is_empty() || rest.contains(['$', '`', '"', '\'', '\\']) {
                    return None;
                }
                return Some(parent.join(rest));
            }
        }
        let value = self.eval(word, invoked)?;
        if value.is_empty() || value.contains(['*', '?', '[']) {
            return None;
        }
        let path = PathBuf::from(&value);
        if path.is_absolute() {
            return Some(path);
        }
        if value.contains('/') {
            // Relative to the server's working directory.
            return None;
        }
        let Value::Set(search) = self.get("PATH") else {
            return None;
        };
        std::env::split_paths(&search)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(&value))
            .find(|candidate| candidate.is_file())
    }

    /// A word's value, or `None` when it needs evaluation LUNA does not do.
    fn eval(&self, word: &Word, invoked: &Path) -> Option<String> {
        let mut out = String::new();
        for part in &word.parts {
            match part {
                Part::Literal(text) => out.push_str(text),
                Part::Variable(name) => match self.get(name) {
                    Value::Set(value) => out.push_str(value.to_str()?),
                    Value::Unset => {}
                    Value::Unknown => return None,
                },
                Part::Tilde => match self.get("HOME") {
                    Value::Set(value) if !value.is_empty() => out.push_str(value.to_str()?),
                    _ => return None,
                },
                Part::Zero => out.push_str(invoked.to_str()?),
                Part::Dynamic => return None,
            }
        }
        Some(out)
    }
}

fn absolute(value: OsString) -> Result<PathBuf, StorageIndeterminate> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        // Relative to the server's working directory (OCS-06).
        Err(StorageIndeterminate)
    }
}

enum Step {
    Continue,
    Exec(Option<PathBuf>),
}

enum Launcher {
    /// Native, another format, or another interpreter: no shell changes.
    Passthrough,
    /// A shell script.
    Shell(String),
    /// A launcher whose effect cannot be evaluated.
    Opaque,
}

fn launcher(path: &Path, len: u64) -> Launcher {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Launcher::Opaque;
    };
    let mut magic = [0u8; 2];
    if file.read_exact(&mut magic).is_err() || &magic != b"#!" {
        // A native binary (or a file too short to be a script).
        return Launcher::Passthrough;
    }
    let mut bytes = magic.to_vec();
    if file.take(MAX_SCRIPT_BYTES).read_to_end(&mut bytes).is_err() {
        return Launcher::Opaque;
    }
    let Some(shebang) = bytes.strip_prefix(b"#!") else {
        return Launcher::Passthrough;
    };
    let line = shebang
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    let line = String::from_utf8_lossy(line);
    let mut words = line.split_whitespace();
    let mut interpreter = words.next().unwrap_or_default();
    let base = |path: &str| path.rsplit('/').next().unwrap_or(path).to_owned();
    if base(interpreter) == "env" {
        let mut next = words.next().unwrap_or_default();
        while next.starts_with('-') || next.contains('=') {
            if next.contains('=') || next.starts_with("-i") || next.starts_with("-u") {
                return Launcher::Opaque;
            }
            next = words.next().unwrap_or_default();
        }
        interpreter = next;
    }
    if !SHELLS.contains(&base(interpreter).as_str()) {
        return Launcher::Passthrough;
    }
    if len > MAX_SCRIPT_BYTES || bytes.len() as u64 > MAX_SCRIPT_BYTES {
        return Launcher::Opaque;
    }
    Launcher::Shell(String::from_utf8_lossy(&bytes).into_owned())
}

fn is_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

/// Lines with backslash continuations joined.
fn logical_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for raw in text.lines() {
        let trailing = raw.chars().rev().take_while(|ch| *ch == '\\').count();
        if trailing % 2 == 1 {
            current.push_str(&raw[..raw.len() - 1]);
            continue;
        }
        current.push_str(raw);
        lines.push(std::mem::take(&mut current));
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

enum Part {
    Literal(String),
    Variable(String),
    Tilde,
    Zero,
    Dynamic,
}

struct Word {
    raw: String,
    quoted: bool,
    assign: Option<String>,
    /// Names a `${NAME:=value}` / `${NAME=value}` in the word may assign.
    defaults: Vec<String>,
    parts: Vec<Part>,
}

impl Word {
    /// The text of an unquoted, expansion-free word.
    fn literal(&self) -> Option<&str> {
        match self.parts.as_slice() {
            [Part::Literal(text)] if !self.quoted && self.assign.is_none() => Some(text),
            _ => None,
        }
    }
}

enum Token {
    Word(Word),
    /// `;`
    Separator,
    /// `&`, `|` and parentheses.
    Operator,
}

fn boundary(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, ';' | '&' | '|' | '(' | ')' | '<' | '>')
}

/// Words and operators of one line. Redirections (`2>&1`, `>>log`, `<in`)
/// change no variable, so they and their targets are dropped; a command with
/// them is still a simple command.
fn tokenize(line: &str) -> Result<Vec<Token>, ()> {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        if ch == '#' {
            break;
        }
        if ch == ';' {
            tokens.push(Token::Separator);
            i += 1;
            continue;
        }
        if ch == '<' || ch == '>' {
            i += 1;
            if chars
                .get(i)
                .is_some_and(|next| matches!(next, '>' | '&' | '|'))
            {
                i += 1;
            }
            while chars.get(i).is_some_and(|next| next.is_whitespace()) {
                i += 1;
            }
            if chars.get(i).is_some_and(|next| !boundary(*next)) {
                i = parse_word(&chars, i)?.1;
            }
            continue;
        }
        if boundary(ch) {
            tokens.push(Token::Operator);
            i += 1;
            continue;
        }
        let (word, end) = parse_word(&chars, i)?;
        i = end;
        let fd_number = word.raw.chars().all(|ch| ch.is_ascii_digit())
            && chars.get(end).is_some_and(|next| matches!(next, '<' | '>'));
        if !fd_number {
            tokens.push(Token::Word(word));
        }
    }
    Ok(tokens)
}

/// One shell word starting at `chars[start]`; the index after it.
fn parse_word(chars: &[char], start: usize) -> Result<(Word, usize), ()> {
    let mut i = start;
    let mut word = Word {
        raw: String::new(),
        quoted: false,
        assign: None,
        defaults: Vec::new(),
        parts: Vec::new(),
    };
    if chars[i] == '_' || chars[i].is_ascii_alphabetic() {
        let mut end = i;
        while end < chars.len() && (chars[end] == '_' || chars[end].is_ascii_alphanumeric()) {
            end += 1;
        }
        if chars.get(end) == Some(&'=') {
            word.assign = Some(chars[i..end].iter().collect());
            i = end + 1;
        }
    }
    if chars.get(i) == Some(&'~')
        && chars
            .get(i + 1)
            .is_none_or(|next| *next == '/' || boundary(*next))
    {
        word.parts.push(Part::Tilde);
        i += 1;
    }
    let mut literal = String::new();
    let flush = |literal: &mut String, parts: &mut Vec<Part>| {
        if !literal.is_empty() {
            parts.push(Part::Literal(std::mem::take(literal)));
        }
    };
    while i < chars.len() && !boundary(chars[i]) {
        match chars[i] {
            '\'' => {
                word.quoted = true;
                let end = (i + 1..chars.len()).find(|j| chars[*j] == '\'').ok_or(())?;
                literal.extend(&chars[i + 1..end]);
                i = end + 1;
            }
            '"' => {
                word.quoted = true;
                i += 1;
                loop {
                    match chars.get(i) {
                        None => return Err(()),
                        Some('"') => {
                            i += 1;
                            break;
                        }
                        Some('\\')
                            if chars
                                .get(i + 1)
                                .is_some_and(|next| matches!(next, '$' | '`' | '"' | '\\')) =>
                        {
                            literal.push(chars[i + 1]);
                            i += 2;
                        }
                        Some('$') => {
                            flush(&mut literal, &mut word.parts);
                            i = dollar(chars, i, &mut word, &mut literal)?;
                        }
                        Some('`') => {
                            flush(&mut literal, &mut word.parts);
                            word.parts.push(Part::Dynamic);
                            i = (i + 1..chars.len()).find(|j| chars[*j] == '`').ok_or(())? + 1;
                        }
                        Some(other) => {
                            literal.push(*other);
                            i += 1;
                        }
                    }
                }
            }
            '\\' => {
                if let Some(next) = chars.get(i + 1) {
                    literal.push(*next);
                }
                i += 2;
            }
            '$' => {
                flush(&mut literal, &mut word.parts);
                i = dollar(chars, i, &mut word, &mut literal)?;
            }
            '`' => {
                flush(&mut literal, &mut word.parts);
                word.parts.push(Part::Dynamic);
                i = (i + 1..chars.len()).find(|j| chars[*j] == '`').ok_or(())? + 1;
            }
            other => {
                literal.push(other);
                i += 1;
            }
        }
    }
    flush(&mut literal, &mut word.parts);
    let end = i.min(chars.len());
    word.raw = chars[start..end].iter().collect();
    Ok((word, end))
}

/// One `$` expansion starting at `chars[i]`; the index after it.
fn dollar(chars: &[char], i: usize, word: &mut Word, literal: &mut String) -> Result<usize, ()> {
    match chars.get(i + 1) {
        Some('{') => {
            let end = (i + 2..chars.len()).find(|j| chars[*j] == '}').ok_or(())?;
            let inner: String = chars[i + 2..end].iter().collect();
            if is_name(&inner) {
                word.parts.push(Part::Variable(inner));
            } else if inner == "0" {
                word.parts.push(Part::Zero);
            } else {
                let name: String = inner
                    .chars()
                    .take_while(|ch| *ch == '_' || ch.is_ascii_alphanumeric())
                    .collect();
                let operator = &inner[name.len()..];
                if is_name(&name) && (operator.starts_with(":=") || operator.starts_with('=')) {
                    word.defaults.push(name);
                }
                word.parts.push(Part::Dynamic);
            }
            Ok(end + 1)
        }
        Some('(') => {
            word.parts.push(Part::Dynamic);
            skip_parens(chars, i + 1)
        }
        Some(next) if *next == '_' || next.is_ascii_alphabetic() => {
            let end = (i + 1..chars.len())
                .find(|j| !(chars[*j] == '_' || chars[*j].is_ascii_alphanumeric()))
                .unwrap_or(chars.len());
            word.parts
                .push(Part::Variable(chars[i + 1..end].iter().collect()));
            Ok(end)
        }
        Some('0') => {
            word.parts.push(Part::Zero);
            Ok(i + 2)
        }
        Some(next)
            if next.is_ascii_digit() || matches!(next, '@' | '*' | '#' | '?' | '$' | '!' | '-') =>
        {
            word.parts.push(Part::Dynamic);
            Ok(i + 2)
        }
        _ => {
            literal.push('$');
            Ok(i + 1)
        }
    }
}

/// The index after the `)` that closes the `(` at `open`.
fn skip_parens(chars: &[char], open: usize) -> Result<usize, ()> {
    let mut depth = 0usize;
    let mut i = open;
    while i < chars.len() {
        match chars[i] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i + 1);
                }
            }
            '\'' => i = (i + 1..chars.len()).find(|j| chars[*j] == '\'').ok_or(())?,
            '"' => {
                i += 1;
                while chars.get(i).ok_or(())? != &'"' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            '\\' => i += 1,
            _ => {}
        }
        i += 1;
    }
    Err(())
}
