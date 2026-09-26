# Managed skill library acceptance

Exercise these failure cases with synthetic provider homes and a synthetic
project before changing the file operations. Keep every fixture outside real
Codex, Claude, and OpenCode configuration roots.

1. A project skill directory or `SKILL.md` symlink points to a synthetic secret
   outside the selected owner root. Discovery may report the entry, but
   `GetSkillDocument` must not return the secret bytes. Save, install, and
   remove must leave the target untouched. A deliberately linked *global*
   provider skill root may resolve to a shared user directory, while a nested
   symlink inside it remains blocked.
2. An existing skill changes after inspection. Save and remove with the old
   revision must fail without overwriting or deleting it. A supporting script
   changing after inspection must also block removal and installation.
3. Two installs race for one destination. Exactly one may succeed; the other
   must fail without replacing or merging into the winner. A preexisting
   destination must remain byte for byte unchanged.
4. Install a skill with supporting files between provider and scope roots.
   Preserve relative files, validate frontmatter for the destination provider,
   and reject symlinked or oversized source trees.
5. Reject unsafe names, malformed frontmatter, absent project selection, and
   revision conflicts. A global operation must not touch a same-named project
   skill, and vice versa.
6. The inventory must keep provider, global/project, and selected Codex-account
   ownership distinct. Disk presence and a provider catalog entry do not prove
   selected-session attachment; show Unknown without exact owner readback.
7. Keep existing LUNA instruction editing and MCP configuration intact. All
   skill RPC mutations require the local owning device and a known project ID
   for project scope.

LUNA serializes its own skill mutations inside the owning engine and rechecks
the whole skill tree immediately before edit or removal. Provider CLIs and
external editors do not share that lock; an edit racing within the final
filesystem operation cannot be an exact cross-process compare-and-swap. The
revision guard detects changes made before that final check, and install uses
an atomic no-replace rename for destination ownership.

Run the file-backed lifecycle E2E with `scripts/verify-managed-skills.sh` after
the coordinator grants the build slot. It writes a command/status artifact and
full log under `target/qa/managed-skills/` using repeatable synthetic inputs.
Then refresh the reserved QA app, inspect Settings, and read back its exact
process, IPC listener, and signature.
