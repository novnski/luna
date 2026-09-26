//! File-backed schema acceptance across v13, v14, and the current schema.
//! Failure cases: upgrading v13 loses command config; opening twice reruns DDL;
//! a partial migration marks v14 or leaks tables; a future version mutates bytes.

use luna_sync::DocsStore;
use rusqlite::Connection;

fn v13_profile() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    drop(DocsStore::open(root.path()).unwrap());
    let db = Connection::open(root.path().join("docs.sqlite3")).unwrap();
    // Undo v14 and v15 physically before resetting the migration marker.
    db.execute_batch(
        "DROP TABLE IF EXISTS side_chat_sends;
        DROP TABLE IF EXISTS side_chat_entries;
        DROP TABLE IF EXISTS side_chats;
        DROP TABLE task_execution_lineage;
        ALTER TABLE task_command_state DROP COLUMN workspace_repair_revision;
        ALTER TABLE task_command_receipts DROP COLUMN workspace_repair_revision;
        ALTER TABLE local_executable_authority DROP COLUMN workspace_repair_revision;
        DELETE FROM schema_migrations WHERE version>=14;
        INSERT INTO project_command_configs VALUES ('project','device',X'5B5D');",
    )
    .unwrap();
    assert_eq!(version(&db), 13);
    root
}

fn version(db: &Connection) -> i64 {
    db.query_row("SELECT max(version) FROM schema_migrations", [], |row| {
        row.get(0)
    })
    .unwrap()
}

#[test]
fn v13_profile_migrates_once_and_preserves_existing_command_storage() {
    let root = v13_profile();
    drop(DocsStore::open(root.path()).unwrap());
    let db = Connection::open(root.path().join("docs.sqlite3")).unwrap();
    assert_eq!(version(&db), 15);
    assert_eq!(
        db.query_row("SELECT count(*) FROM schema_migrations", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        15
    );
    assert_eq!(db.query_row("SELECT config FROM project_command_configs WHERE project_id='project' AND device_id='device'", [], |row| row.get::<_, Vec<u8>>(0)).unwrap(), b"[]");
    db.execute("INSERT INTO side_chats (id,parent_id,title,created_at_ms,expires_at_ms,state,config,cwd,context,context_mode,creation_hash) VALUES ('side','parent','Saved',1,7200001,'unloaded','{}','/fixture','','contextSnapshot','hash')", []).unwrap();
    db.execute(
        "INSERT INTO side_chat_entries VALUES ('side',0,'saved transcript')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO side_chat_sends VALUES ('side','message','accepted','payload')",
        [],
    )
    .unwrap();
    drop(db);
    let reopened = DocsStore::open(root.path()).unwrap();
    assert!(reopened.is_side_chat("side").unwrap());
    drop(reopened);
    let db = Connection::open(root.path().join("docs.sqlite3")).unwrap();
    assert_eq!(version(&db), 15);
    assert_eq!(
        db.query_row(
            "SELECT entry FROM side_chat_entries WHERE chat_id='side'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "saved transcript"
    );
    assert_eq!(
        db.query_row(
            "SELECT state FROM side_chat_sends WHERE chat_id='side'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "accepted"
    );
}

#[test]
fn failed_v14_migration_rolls_back_all_new_tables_and_version() {
    let root = v13_profile();
    let db = Connection::open(root.path().join("docs.sqlite3")).unwrap();
    // The second table collides after the first DDL succeeds inside the txn.
    db.execute_batch("CREATE TABLE side_chat_entries(value TEXT); INSERT INTO side_chat_entries VALUES('preserve collision evidence');").unwrap();
    drop(db);
    assert!(DocsStore::open(root.path()).is_err());
    let db = Connection::open(root.path().join("docs.sqlite3")).unwrap();
    assert_eq!(version(&db), 13);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='side_chats'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT value FROM side_chat_entries", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "preserve collision evidence"
    );
    db.execute_batch("DROP TABLE side_chat_entries").unwrap();
    drop(db);
    drop(DocsStore::open(root.path()).unwrap());
    let db = Connection::open(root.path().join("docs.sqlite3")).unwrap();
    assert_eq!(version(&db), 15);
    assert_eq!(
        db.query_row("SELECT count(*) FROM project_command_configs", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
}

#[test]
fn next_schema_is_rejected_before_wal_or_migration_changes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("docs.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,applied_at INTEGER NOT NULL) STRICT;
        INSERT INTO schema_migrations VALUES(16,1);
        CREATE TABLE future_data(value TEXT); INSERT INTO future_data VALUES('keep');").unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    let error = match DocsStore::open(root.path()) {
        Ok(_) => panic!("future schema accepted"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("16") && error.contains("supported 15"),
        "{error}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!root.path().join("docs.sqlite3-wal").exists());
}
