//! Execution scope for provider command discovery (E23).

use std::path::PathBuf;

use luna_proto::PermissionMode;

/// The exact workspace, permission mode and account that a run in this scope
/// would launch with. Providers whose loaded configuration depends on any of
/// these (Claude Code's project settings, custom commands, output styles,
/// restricted mode) discover commands under the same scope, so the catalog
/// matches the run. `Default` is the legacy unscoped request: no workspace, Ask.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct CommandScope {
    /// Working directory the run would use; `None` inherits the engine's.
    pub cwd: Option<PathBuf>,
    /// Effective (engine-resolved) permission mode of the run.
    pub permission_mode: PermissionMode,
    /// Engine-only provider authentication instance (never a credential or a
    /// path). A different signed-in account invalidates cached discovery.
    pub auth_instance: Option<String>,
}
