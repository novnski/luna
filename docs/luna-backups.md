# LUNA offline backups

LUNA follows BB's private gzip/tar archive pattern. A clean engine shutdown makes
at most one automatic backup per 24 hours when the included data changed. There
is no online backup: a failed shutdown or an engine that keeps running does not
produce a new archive. You can quit LUNA and make one explicitly:

```text
luna backup status
luna backup now
luna backup preview /absolute/path/to/luna-....tar.gz
```

Archives are unencrypted. LUNA writes them as mode `0600` files in a mode `0700`
`<data-dir>.backups` sibling directory. The manifest is written first and every
included file is SHA-256-checked before the archive is published. Keep copies
private and protect them as you would your transcripts and attachments. LUNA
does not automatically delete old archives; retention belongs to L18.

To restore, quit every LUNA instance using that data directory, then run:

```text
luna backup restore /absolute/path/to/luna-....tar.gz
```

Restore requires the original data-root path and device identity. It stages and
verifies the archive before replacing only the included settings, profile
databases, journals and attachments. The previous versions of those files are
kept in a private `.luna-l17-managed-rollback-*` sibling directory. A failed
readback rolls the changed files back. If a process dies during replacement,
LUNA refuses to start until `luna backup recover` restores the previous files.
Do not delete the rollback directory until you have checked the restored app.
Preview is read-only and requires LUNA to be stopped. It reports counts,
estimated staging and rollback space, existing owned-file conflicts, and exact
unsupported reasons. Current schema 12 archives retain LUNA cleanup checkpoints
and local executable-authority rows; unreviewed newer schemas are refused.

The archive deliberately excludes LUNA credentials, sign-in sessions, device
identity, provider-native stores, remote grants, logs, caches, pending uploads,
worktrees and arbitrary project files. Restore preserves those existing local
files; the archive cannot recover them if they are lost, and it cannot relocate
absolute attachment references to a different data-root path. Live restore
from the Backups page is disabled. The page shows the latest verified archive
and the unencrypted-data warning, while restore is an offline CLI operation.
