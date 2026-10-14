//! Executable discovery shared by native and ACP harnesses.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Identity of the actual launch source, refreshed without spawning a process.
pub(crate) fn catalog_source(path: &Path) -> String {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let metadata = std::fs::metadata(&path).ok();
    format!(
        "{}:{:?}:{:?}",
        path.display(),
        metadata.as_ref().map(|m| m.len()),
        metadata.and_then(|m| m.modified().ok())
    )
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    home_dir_with(&|key| std::env::var_os(key))
}

/// Resolve a user base directory, falling back to the current directory.
pub(crate) fn home_or_current_dir() -> PathBuf {
    home_or_current_dir_with(&|key| std::env::var_os(key), &std::env::current_dir)
}

fn home_or_current_dir_with(
    env: &impl Fn(&str) -> Option<OsString>,
    current_dir: &impl Fn() -> std::io::Result<PathBuf>,
) -> PathBuf {
    home_dir_with(env)
        .or_else(|| current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Accept an executable override. The path must exist, so `installed()` and
/// the launch path agree on what an override means.
pub(crate) fn validate_native_override(path: &Path) -> Result<PathBuf, crate::HarnessError> {
    if !path.is_file() {
        return Err(crate::HarnessError::NotInstalled(format!(
            "{} does not exist",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

pub(crate) fn find_on_paths(exe: &str, extra: Vec<PathBuf>) -> Option<PathBuf> {
    find_on_paths_with(
        exe,
        extra,
        &|key| std::env::var_os(key),
        crate::shell_env::login_shell_path().map(OsString::from),
    )
}

/// Like [`find_on_paths`], but a runnable candidate must also satisfy
/// `predicate` (OpenCode skips binaries of an unsupported generation and
/// keeps searching in the same order).
pub(crate) fn find_on_paths_matching(
    exe: &str,
    extra: Vec<PathBuf>,
    predicate: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    find_on_paths_matching_with(
        exe,
        extra,
        &|key| std::env::var_os(key),
        crate::shell_env::login_shell_path().map(OsString::from),
        predicate,
    )
}

fn home_dir_with(env: &impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    env("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn node_version_manager_bins_with(env: &impl Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let home = home_dir_with(env);
    let mut dirs = Vec::new();

    let mut fnm_roots: Vec<PathBuf> = env("FNM_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .collect();
    if let Some(home) = &home {
        fnm_roots.push(home.join(".local").join("share").join("fnm"));
        fnm_roots.push(home.join("Library").join("Application Support").join("fnm"));
        fnm_roots.push(home.join(".fnm"));
    }
    for root in fnm_roots {
        dirs.push(root.join("aliases").join("default").join("bin"));
    }

    if let Some(home) = &home {
        dirs.push(home.join(".volta").join("bin"));
        dirs.push(home.join(".bun").join("bin"));
        dirs.push(home.join("Library").join("pnpm"));
        dirs.push(home.join(".local").join("share").join("pnpm"));

        let nvm = home.join(".nvm").join("versions").join("node");
        if let Ok(entries) = std::fs::read_dir(&nvm) {
            let mut versions: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path().join("bin"))
                .collect();
            versions.sort();
            versions.reverse();
            dirs.append(&mut versions);
        }
    }
    dirs
}

pub(crate) fn find_on_paths_with(
    exe: &str,
    extra: Vec<PathBuf>,
    env: &impl Fn(&str) -> Option<OsString>,
    login_shell_path: Option<OsString>,
) -> Option<PathBuf> {
    find_on_paths_matching_with(exe, extra, env, login_shell_path, |_| true)
}

pub(crate) fn find_on_paths_matching_with(
    exe: &str,
    extra: Vec<PathBuf>,
    env: &impl Fn(&str) -> Option<OsString>,
    login_shell_path: Option<OsString>,
    mut predicate: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    let from_path = |path: OsString| {
        std::env::split_paths(&path)
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(|dir| dir.join(exe))
            .collect::<Vec<_>>()
    };
    let mut candidates = env("PATH").map(from_path).unwrap_or_default();
    if let Some(shell_path) = login_shell_path {
        candidates.extend(from_path(shell_path));
    }
    candidates.extend(extra);
    candidates.extend(
        node_version_manager_bins_with(env)
            .into_iter()
            .map(|dir| dir.join(exe)),
    );

    candidates
        .into_iter()
        .find(|path| path.is_file() && predicate(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(values: &[(&str, OsString)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let values: HashMap<String, OsString> = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect();
        move |key| values.get(key).cloned()
    }

    fn joined(paths: &[&Path]) -> OsString {
        std::env::join_paths(paths).unwrap()
    }

    #[test]
    fn discovery_preserves_exact_name_and_source_order() {
        let temp = tempfile::tempdir().unwrap();
        let path_dir = temp.path().join("path");
        let shell_dir = temp.path().join("shell");
        let extra_dir = temp.path().join("extra");
        for dir in [&path_dir, &shell_dir, &extra_dir] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("agent"), b"shim").unwrap();
        }
        let found = find_on_paths_with(
            "agent",
            vec![extra_dir.join("agent")],
            &env(&[("PATH", joined(&[&path_dir]))]),
            Some(joined(&[&shell_dir])),
        );
        assert_eq!(found, Some(path_dir.join("agent")));
    }

    #[test]
    fn native_overrides_must_exist() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("agent.sh");
        std::fs::write(&script, b"#!/bin/sh\n").unwrap();
        assert_eq!(validate_native_override(&script).unwrap(), script);
        let error = validate_native_override(&temp.path().join("agent")).unwrap_err();
        assert!(error.to_string().contains("does not exist"), "{error}");
    }

    #[test]
    fn base_dir_uses_current_dir_when_home_is_unset() {
        let current = PathBuf::from("/work/comet");
        assert_eq!(
            home_or_current_dir_with(&env(&[]), &|| Ok(current.clone())),
            current
        );
    }
}
