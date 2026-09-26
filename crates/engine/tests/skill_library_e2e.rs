//! Real filesystem lifecycle across synthetic Codex, Claude and OpenCode
//! roots. No provider home, user skill, network service or desktop app is used.

use std::path::Path;
use std::sync::{Arc, Barrier};

use luna_engine::integration_settings::{
    SkillRoots, install_skill, remove_skill, save_skill_document, skill_document, skill_inventory,
};
use luna_proto::{HarnessId, IntegrationKind, IntegrationSkillScope};

fn roots(base: &Path) -> SkillRoots {
    SkillRoots {
        codex_home: Some(base.join("codex")),
        claude_home: base.join("claude"),
        opencode_home: base.join("opencode"),
        project_root: Some(base.join("project")),
        machine_id: "synthetic-device".into(),
    }
}

fn content(name: &str) -> String {
    format!("---\nname: {name}\ndescription: Repeat a synthetic workflow.\n---\n\n# {name}\n")
}

#[test]
fn file_backed_provider_skill_lifecycle() {
    let temp = tempfile::tempdir().unwrap();
    let roots = roots(temp.path());

    let source = save_skill_document(
        HarnessId::Codex,
        &roots,
        IntegrationSkillScope::Global,
        "shared-workflow",
        &content("shared-workflow"),
        None,
    )
    .unwrap();
    let source_dir = roots
        .codex_home
        .as_ref()
        .unwrap()
        .join("skills/shared-workflow");
    std::fs::create_dir(source_dir.join("references")).unwrap();
    std::fs::write(
        source_dir.join("references/checklist.md"),
        "synthetic checklist",
    )
    .unwrap();
    let source = skill_document(HarnessId::Codex, &roots, &source.identity).unwrap();
    assert_eq!(
        source.supporting_files,
        ["references", "references/checklist.md"]
    );

    let installed = install_skill(
        HarnessId::Codex,
        &source.identity,
        &source.revision,
        HarnessId::ClaudeCode,
        IntegrationSkillScope::Project,
        &roots,
    )
    .unwrap();
    assert_eq!(installed.scope, IntegrationSkillScope::Project);
    assert_eq!(installed.provider, HarnessId::ClaudeCode);
    assert_eq!(installed.supporting_files, source.supporting_files);
    assert_eq!(
        std::fs::read_to_string(
            roots
                .project_root
                .as_ref()
                .unwrap()
                .join(".claude/skills/shared-workflow/references/checklist.md")
        )
        .unwrap(),
        "synthetic checklist"
    );
    assert!(
        install_skill(
            HarnessId::Codex,
            &source.identity,
            &source.revision,
            HarnessId::ClaudeCode,
            IntegrationSkillScope::Project,
            &roots,
        )
        .is_err()
    );

    let same_name_project = save_skill_document(
        HarnessId::Codex,
        &roots,
        IntegrationSkillScope::Project,
        "shared-workflow",
        &content("shared-workflow"),
        None,
    )
    .unwrap();
    assert_ne!(same_name_project.identity, source.identity);
    let inventory = skill_inventory(HarnessId::Codex, &roots);
    assert_eq!(
        inventory
            .items
            .iter()
            .filter(|item| item.kind == IntegrationKind::Skill && item.name == "shared-workflow")
            .count(),
        2
    );

    std::fs::write(
        source_dir.join("references/checklist.md"),
        "changed checklist",
    )
    .unwrap();
    assert!(remove_skill(HarnessId::Codex, &roots, &source.identity, &source.revision).is_err());
    assert!(source_dir.join("SKILL.md").exists());
    let current = skill_document(HarnessId::Codex, &roots, &source.identity).unwrap();
    assert_ne!(current.revision, source.revision);
    remove_skill(
        HarnessId::Codex,
        &roots,
        &source.identity,
        &current.revision,
    )
    .unwrap();
    assert!(!source_dir.exists());
    assert!(skill_document(HarnessId::Codex, &roots, &same_name_project.identity).is_ok());

    assert!(
        save_skill_document(
            HarnessId::Opencode,
            &roots,
            IntegrationSkillScope::Global,
            "bad-name",
            "---\ndescription: Missing name\n---\n\nBody\n",
            None,
        )
        .is_err()
    );
    assert!(
        save_skill_document(
            HarnessId::Opencode,
            &roots,
            IntegrationSkillScope::Global,
            "../escape",
            &content("../escape"),
            None,
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn outside_symlink_is_never_read_or_mutated() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let roots = roots(temp.path());
    let secret = temp.path().join("outside-owner-secret.txt");
    std::fs::write(&secret, "SYNTHETIC_SECRET_DO_NOT_RETURN").unwrap();
    let skill_dir = roots
        .project_root
        .as_ref()
        .unwrap()
        .join(".agents/skills/leak");
    std::fs::create_dir_all(&skill_dir).unwrap();
    symlink(&secret, skill_dir.join("SKILL.md")).unwrap();
    let inventory = skill_inventory(HarnessId::Codex, &roots);
    let identity = inventory
        .items
        .iter()
        .find(|item| item.name == "leak")
        .unwrap()
        .identity
        .as_ref()
        .unwrap();
    assert!(skill_document(HarnessId::Codex, &roots, identity).is_err());
    assert!(remove_skill(HarnessId::Codex, &roots, identity, "anything").is_err());
    assert_eq!(
        std::fs::read_to_string(&secret).unwrap(),
        "SYNTHETIC_SECRET_DO_NOT_RETURN"
    );
}

#[test]
fn competing_installs_never_replace_a_winner() {
    let temp = tempfile::tempdir().unwrap();
    let roots = Arc::new(roots(temp.path()));
    let source = save_skill_document(
        HarnessId::ClaudeCode,
        &roots,
        IntegrationSkillScope::Global,
        "race-skill",
        &content("race-skill"),
        None,
    )
    .unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let attempts: Vec<_> = (0..2)
        .map(|_| {
            let roots = Arc::clone(&roots);
            let barrier = Arc::clone(&barrier);
            let identity = source.identity.clone();
            let revision = source.revision.clone();
            std::thread::spawn(move || {
                barrier.wait();
                install_skill(
                    HarnessId::ClaudeCode,
                    &identity,
                    &revision,
                    HarnessId::Opencode,
                    IntegrationSkillScope::Global,
                    &roots,
                )
                .is_ok()
            })
        })
        .collect();
    barrier.wait();
    assert_eq!(
        attempts
            .into_iter()
            .filter(|attempt| attempt.join().unwrap())
            .count(),
        1
    );
    let destination = roots.opencode_home.join("skills/race-skill/SKILL.md");
    assert_eq!(
        std::fs::read_to_string(destination).unwrap(),
        content("race-skill")
    );
}
