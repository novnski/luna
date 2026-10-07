//! Executable resolution: `OPENCODE_EXECUTABLE`, PATH, the login shell's
//! PATH and the known install locations.

use super::*;

pub(super) fn opencode_install_paths() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    // `crate::executable::home_dir` checks USERPROFILE too, so the Unix-style
    // install locations resolve on Windows (`~/.local/bin/opencode.exe`, the
    // bun layout) instead of silently vanishing with an unset HOME.
    if let Some(home) = crate::executable::home_dir() {
        dirs.push(home.join(".opencode").join("bin").join("opencode"));
        dirs.push(home.join(".local").join("bin").join("opencode"));
        dirs.push(home.join(".bun").join("bin").join("opencode"));
        dirs.push(home.join(".npm-global").join("bin").join("opencode"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin/opencode"));
    dirs.push(PathBuf::from("/usr/local/bin/opencode"));
    dirs
}

pub(super) const INSTALL_HINT: &str = "opencode (searched PATH, the login shell's PATH, ~/.opencode/bin, \
     ~/.local/bin, ~/.bun/bin, ~/.npm-global/bin, /opt/homebrew/bin, /usr/local/bin, and \
     fnm/nvm/volta/pnpm/bun install dirs; Windows also checks USERPROFILE, \
     %APPDATA%\\npm, and explicit NVM_SYMLINK/VOLTA_HOME/PNPM_HOME; install with \
     `curl -fsSL https://opencode.ai/install | bash` or \
     `npm install -g opencode-ai`, then `opencode auth login`; set \
     OPENCODE_EXECUTABLE to override)";

/// The user's opencode: `OPENCODE_EXECUTABLE` when it exists, else PATH (plus
/// install locations). The override is validated at launch through
/// [`crate::executable::validate_native_override`] — including the `.cmd`
/// shims npm installs on Windows.
pub fn resolve_opencode_executable() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("OPENCODE_EXECUTABLE")
        && !path.is_empty()
    {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    crate::executable::find_on_paths("opencode", opencode_install_paths())
}
