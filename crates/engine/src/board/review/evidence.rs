//! PB-3 evidence: `OutcomeReference` rows for the detail drawer — the
//! reviewed diff (stats and hash), the review result with findings summary
//! and its actual actor/model, check results (recorded when reported), PR
//! links and artifacts the implementer referenced, and the implementer's
//! summary. References, never copies; each tied to a generation and basis.
//! A newer capture marks older rows of the same kind historical.
use super::*;
use sha2::Sha256;

/// Bounded structured detail (≤ 16 KiB serialized, see `OutcomeReference`).
const DETAIL_MAX_BYTES: usize = 16 * 1024;

fn outcome_id(parts: &impl Serialize) -> BoardResult<String> {
    Ok(format!(
        "oc-{}",
        &board_digest(&("board-outcome", parts))?[..32]
    ))
}

fn bounded_detail(value: Value) -> Value {
    if serde_json::to_vec(&value).map_or(0, |bytes| bytes.len()) <= DETAIL_MAX_BYTES {
        value
    } else {
        serde_json::json!({"omitted": "detail exceeded 16 KiB"})
    }
}

fn is_document(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [
        ".md", ".pdf", ".docx", ".txt", ".html", ".csv", ".xlsx", ".pptx",
    ]
    .iter()
    .any(|extension| lower.ends_with(extension))
}

/// The review-result evidence row recorded with an accepted result.
pub(super) fn review_result_outcome(run: &ReviewRun, actor: &BoardActor) -> OutcomeReference {
    let result = run.result.as_ref();
    let findings = result
        .map(|result| {
            result
                .findings
                .iter()
                .take(20)
                .map(|finding| {
                    serde_json::json!({
                        "id": finding.id,
                        "severity": finding.severity,
                        "blocking": finding.blocking,
                        "file": finding.file,
                        "line": finding.line,
                        "explanation": finding.explanation.chars().take(300).collect::<String>(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let verdict = result.map(|result| result.verdict);
    OutcomeReference {
        id: format!(
            "oc-{}",
            &crate::repos::hex(&Sha256::digest(format!(
                "board-review-result\u{1f}{}",
                run.id
            )))[..32]
        ),
        work_item_id: run.work_item_id.clone(),
        kind: OutcomeKind::ReviewResult,
        execution_generation: run.execution_generation,
        task_id: run.reviewer_task_id.clone(),
        label: format!(
            "Review {} · {}",
            match verdict {
                Some(ReviewVerdict::Passed) => "passed",
                Some(ReviewVerdict::ChangesRequested) => "requested changes",
                Some(ReviewVerdict::Blocked) => "blocked",
                Some(ReviewVerdict::Failed) => "failed",
                Some(ReviewVerdict::Cancelled) => "cancelled",
                None => "pending",
            },
            delivery::reviewer_label(run)
        ),
        basis: run.basis.as_ref().map(|basis| basis.diff_hash.clone()),
        url: None,
        path: None,
        state: verdict.map(|verdict| {
            serde_json::to_value(verdict)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default()
        }),
        detail: bounded_detail(serde_json::json!({
            "reviewRunId": run.id,
            "round": run.round,
            "summary": result.map(|result| result.summary.chars().take(2_000).collect::<String>()),
            "blockingFindings": result.map(|result| blocking_count(&result.findings)),
            "findings": findings,
            "packetHash": run.basis.as_ref().map(|basis| basis.packet_hash.clone()),
            "selectedModel": run.profile.resolved.model,
            "selectedHarness": run.profile.resolved.harness,
            "observedModel": run.profile.observed_model,
            "observedHarness": run.profile.observed_harness,
            "implementationFallback": run.profile.implementation_fallback,
        })),
        historical: false,
        observed_by: actor.clone(),
        observed_at_ms: result.map_or(run.updated_at_ms, |result| result.submitted_at_ms),
    }
}

impl EngineRpc {
    /// Evidence rows for one run's generation (diff, review result, PRs,
    /// artifacts/documents, implementer summary), plus older rows of the
    /// same kinds re-marked historical.
    pub(super) fn review_outcome_rows(
        &self,
        item: &WorkItem,
        run: &ReviewRun,
    ) -> BoardResult<Vec<OutcomeReference>> {
        let now = crate::now_ms();
        let generation = run.execution_generation;
        let mut rows = Vec::new();
        if let Some(basis) = &run.basis {
            rows.push(OutcomeReference {
                id: outcome_id(&("diff", &item.id, generation, &basis.diff_hash))?,
                work_item_id: item.id.clone(),
                kind: OutcomeKind::Diff,
                execution_generation: generation,
                task_id: Some(run.primary_task_id.clone()),
                label: format!(
                    "Reviewed changes · {} omission(s) declared",
                    basis.omissions.len()
                ),
                basis: Some(basis.diff_hash.clone()),
                url: None,
                path: Some(basis.cwd.clone()),
                state: Some("reviewed".into()),
                detail: bounded_detail(serde_json::json!({
                    "checkoutId": basis.checkout_id,
                    "baseRevision": basis.base_revision,
                    "headRevision": basis.head_revision,
                    "diffHash": basis.diff_hash,
                    "manifestHash": basis.uncommitted_manifest_hash,
                    "packetVersion": basis.packet_version,
                    "packetHash": basis.packet_hash,
                    "briefRevision": basis.brief_revision,
                    "checkOutcomeIds": basis.check_outcome_ids,
                    "omissions": basis.omissions.iter().take(20).collect::<Vec<_>>(),
                })),
                historical: false,
                observed_by: engine_actor(),
                observed_at_ms: basis.captured_at_ms,
            });
        }
        if run.result.is_some() {
            let actor = BoardActor::Reviewer {
                task_id: run.reviewer_task_id.clone().unwrap_or_default(),
                review_run_id: run.id.clone(),
            };
            rows.push(review_result_outcome(run, &actor));
        }
        let mut evidence = run.evidence.clone();
        if let Some(report) = item
            .last_report
            .as_ref()
            .filter(|report| report.execution_generation == generation)
        {
            for reference in &report.report.evidence {
                if !evidence.contains(reference) {
                    evidence.push(reference.clone());
                }
            }
        }
        for reference in evidence.iter().take(EVIDENCE_MAX) {
            let pull_request = reference
                .url
                .as_deref()
                .is_some_and(|url| url.contains("/pull/"));
            let kind = if pull_request {
                OutcomeKind::PullRequest
            } else if reference.path.as_deref().is_some_and(is_document) {
                OutcomeKind::Document
            } else if reference.path.is_some() || reference.url.is_some() {
                OutcomeKind::Artifact
            } else {
                continue;
            };
            rows.push(OutcomeReference {
                id: outcome_id(&(
                    kind.as_str(),
                    &item.id,
                    generation,
                    &reference.url,
                    &reference.path,
                ))?,
                work_item_id: item.id.clone(),
                kind,
                execution_generation: generation,
                task_id: Some(run.primary_task_id.clone()),
                label: reference.label.chars().take(200).collect(),
                basis: run.basis.as_ref().map(|basis| basis.diff_hash.clone()),
                url: reference.url.clone(),
                path: reference.path.clone(),
                state: Some(if pull_request { "linked" } else { "referenced" }.into()),
                detail: Value::Null,
                historical: false,
                observed_by: BoardActor::Agent {
                    task_id: run.primary_task_id.clone(),
                    role: None,
                },
                observed_at_ms: now,
            });
        }
        if let Some(summary) = &run.implementer_summary {
            rows.push(OutcomeReference {
                id: outcome_id(&("summary", &item.id, generation, &run.id))?,
                work_item_id: item.id.clone(),
                kind: OutcomeKind::Summary,
                execution_generation: generation,
                task_id: Some(run.primary_task_id.clone()),
                label: "Implementer summary".into(),
                basis: run.basis.as_ref().map(|basis| basis.diff_hash.clone()),
                url: None,
                path: None,
                state: None,
                detail: bounded_detail(serde_json::json!({"summary": summary})),
                historical: false,
                observed_by: BoardActor::Agent {
                    task_id: run.primary_task_id.clone(),
                    role: None,
                },
                observed_at_ms: now,
            });
        }
        // Older evidence of the same kinds stays as history.
        let current = rows
            .iter()
            .map(|row| row.id.clone())
            .collect::<HashSet<_>>();
        let kinds = rows.iter().map(|row| row.kind).collect::<HashSet<_>>();
        for mut old in self.store.board_outcomes(&item.id, 200).map_err(storage)? {
            if !old.historical
                && !current.contains(&old.id)
                && kinds.contains(&old.kind)
                && old.kind != OutcomeKind::Check
            {
                old.historical = true;
                rows.push(old);
            }
        }
        Ok(rows)
    }
}
