//! Real document/store lifecycle acceptance, specified before implementation.
//! Failure cases: rapidly browsing chats retains every document forever after
//! the user goes idle; a queue-only watcher is orphaned by eviction; active
//! transcript readers/writers are evicted; reopening loses saved content.
//! Persistence failure case: eviction must retain the only unsaved in-memory
//! copy until SQLite accepts a snapshot again.

use std::{sync::Arc, time::Duration};

use luna_engine::{DocHost, DocHostConfig};
use luna_proto::HarnessId;
use luna_sync::DocsStore;

#[tokio::test]
async fn quiet_host_reclaims_unwatched_documents_and_preserves_live_views() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(DocsStore::open(root.path()).unwrap());
    let host = DocHost::new(
        store,
        DocHostConfig {
            device_id: "residency-audit".into(),
            default_harness: HarnessId::Mock,
            edge: None,
        },
    );
    let mut handles = Vec::new();
    let mut queue = None;
    let mut transcript = None;
    let mut writer = None;
    for index in 0..20 {
        let handle = host.open(&format!("chat-{index}")).unwrap();
        handle
            .doc()
            .doc()
            .get_text("audit")
            .insert(0, &format!("saved-{index}"))
            .unwrap();
        handle.doc().doc().commit();
        match index {
            0 => queue = Some(handle.watch_queue()),
            1 => transcript = Some(handle.watch_messages()),
            2 => writer = Some(handle.doc_arc()),
            _ => {}
        }
        handles.push(Arc::downgrade(&handle));
    }
    // All reads finish inside the 30-second attach grace. There is no new
    // open or mutation afterward to accidentally trigger a cleanup pass.
    tokio::time::timeout(Duration::from_secs(40), async {
        while handles
            .iter()
            .filter(|handle| handle.strong_count() > 0)
            .count()
            > 12
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("an idle host never reclaimed its burst of unwatched documents");

    for (index, handle) in handles.iter().enumerate().take(3) {
        assert!(
            handle.upgrade().is_some(),
            "live view/writer {index} was evicted"
        );
    }
    assert!(queue.as_ref().unwrap().has_changed().is_ok());
    assert!(transcript.as_ref().unwrap().has_changed().is_ok());
    let evicted = handles
        .iter()
        .position(|handle| handle.upgrade().is_none())
        .unwrap();
    let reopened = host.open(&format!("chat-{evicted}")).unwrap();
    assert_eq!(
        reopened.doc().doc().get_text("audit").to_string(),
        format!("saved-{evicted}")
    );
    drop((queue, transcript, writer, reopened));
    host.shutdown_workers().await;
    assert!(handles.iter().all(|handle| handle.upgrade().is_none()));
}

#[tokio::test]
async fn failed_snapshot_does_not_evict_the_only_unsaved_copy() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(DocsStore::open(root.path()).unwrap());
    let host = DocHost::new(
        store.clone(),
        DocHostConfig {
            device_id: "residency-audit".into(),
            default_harness: HarnessId::Mock,
            edge: None,
        },
    );
    let handle = host.open("000-unsaved").unwrap();
    handle
        .doc()
        .doc()
        .get_text("audit")
        .insert(0, "before")
        .unwrap();
    handle.doc().doc().commit();
    host.flush_all();
    let fault = rusqlite::Connection::open(root.path().join("docs.sqlite3")).unwrap();
    fault
        .execute_batch(
            "CREATE TRIGGER fail_audit_snapshot BEFORE INSERT ON snapshots
        WHEN NEW.doc_id = '000-unsaved'
        BEGIN SELECT RAISE(FAIL, 'synthetic snapshot failure'); END;",
        )
        .unwrap();
    handle
        .doc()
        .doc()
        .get_text("audit")
        .insert(6, "-after")
        .unwrap();
    handle.doc().doc().commit();
    let unsaved = Arc::downgrade(&handle);
    drop(handle);
    let mut others = Vec::new();
    for index in 0..20 {
        others.push(Arc::downgrade(
            &host.open(&format!("other-{index}")).unwrap(),
        ));
    }
    tokio::time::timeout(Duration::from_secs(40), async {
        while others.iter().all(|handle| handle.strong_count() > 0) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("eviction pass ran");
    assert!(
        unsaved.upgrade().is_some(),
        "failed final flush discarded the only unsaved document"
    );
    fault
        .execute_batch("DROP TRIGGER fail_audit_snapshot;")
        .unwrap();
    host.flush_all();
    host.shutdown_workers().await;
    let reloaded_host = DocHost::new(
        store,
        DocHostConfig {
            device_id: "residency-audit".into(),
            default_harness: HarnessId::Mock,
            edge: None,
        },
    );
    let reopened = reloaded_host.open("000-unsaved").unwrap();
    assert_eq!(
        reopened.doc().doc().get_text("audit").to_string(),
        "before-after"
    );
    drop(reopened);
    reloaded_host.shutdown_workers().await;
}
