//! Agent-tools slice (A4): board, review and chat-creation tools in the
//! engine-owned internal catalog (`cross_task_tools.rs`), delivered through
//! Codex `luna_mcp`, Claude `luna_tools` and OpenCode's private endpoint.
//!
//! - **Catalog.** [`EngineRpc::board_tools_for_authority`] derives the
//!   caller's canonical audience (main, side, ticket worker, reviewer,
//!   temporary authoring session) and returns only the tools and modes that
//!   apply, with role-shaped schemas (a side chat's `create_side_chat` has no
//!   `context`; a main chat's destination offers `sideChat`, a side chat's
//!   `sibling`). `CrossTaskTools::catalog` attaches them only on a verified
//!   delivery with the `board.read`/`board.write` scopes.
//! - **Authority.** The caller is bound from the run
//!   ([`BoardCaller::from_authority`] over the engine-built
//!   `CrossTaskAuthority`; a temporary authoring session id binds
//!   `BoardCaller::Authoring`). Tool argument structs carry no caller fields;
//!   a model-supplied caller, source, parent or session id that differs from
//!   the run is rejected, never used. Every call re-derives the audience and
//!   refuses tools it does not have, whatever the attached schema said.
//! - **Service.** Each tool translates to the same board request and service
//!   function the RPC dispatcher calls, binding caller ids from the run.
//! - **Results.** Receipts and reads are bounded (briefs truncated with a
//!   flag, lists capped); errors are the `BoardError` JSON plus a `message`.
//!   A slice that has not landed answers `notImplemented`; nothing claims an
//!   operation that did not happen.
//!
//! Contract: docs/design/project-board/pb0-contract.md §7. Tool names are
//! `luna_proto::BOARD_TOOL_NAMES`.
use super::*;
use luna_proto::{
    AuthoringProposal, BOARD_BATCH_START_MAX, BOARD_TOOL_AUTHORING_CHECKPOINT, BOARD_TOOL_CONTROL,
    BOARD_TOOL_CONVERT_CHAT, BOARD_TOOL_CREATE, BOARD_TOOL_CREATE_MAIN_CHAT,
    BOARD_TOOL_CREATE_SIDE_CHAT, BOARD_TOOL_EXECUTION_OPTIONS, BOARD_TOOL_ISSUE_IMPORT,
    BOARD_TOOL_ISSUE_SEARCH, BOARD_TOOL_LIST, BOARD_TOOL_MOVE, BOARD_TOOL_NAMES, BOARD_TOOL_READ,
    BOARD_TOOL_REVIEW_CANCEL, BOARD_TOOL_REVIEW_START, BOARD_TOOL_REVIEW_STATUS,
    BOARD_TOOL_REVIEW_SUBMIT, BOARD_TOOL_SET_STATUS, BOARD_TOOL_START, BOARD_TOOL_UPDATE,
    BoardArchiveRequest, BoardArchivedFilter, BoardAuthoringCheckpointRequest, BoardChatRole,
    BoardColumn, BoardCompletionPolicy, BoardContextMode, BoardContextRef, BoardControlAction,
    BoardControlRequest, BoardConvertChatRequest, BoardCreateMainChatRequest, BoardCreateRequest,
    BoardCreateSideChatRequest, BoardDestination, BoardEvidenceRef, BoardExecutionOptionsRequest,
    BoardExecutionProfile, BoardFollowUp, BoardImportStart, BoardIssueImportRequest,
    BoardIssueSearchRequest, BoardIssueSourcesRequest, BoardItemChanges, BoardItemDetail,
    BoardItemOrigin, BoardLinkChatRequest, BoardListPage, BoardListRequest, BoardMoveRequest,
    BoardPlacement, BoardPriority, BoardReadRequest, BoardReviewCancelRequest, BoardReviewProfile,
    BoardReviewStartRequest, BoardReviewStatusRequest, BoardReviewSubmitRequest,
    BoardRuntimeContext, BoardScope, BoardSendUpdateRequest, BoardSetDependencyRequest,
    BoardSetStatusRequest, BoardSourceKind, BoardStartBatchRequest, BoardStartRequest,
    BoardStatusReport, BoardStatusReportKind, BoardTicketBinding, BoardToolAudience,
    BoardUpdateRequest, ChatPermittedActions, CrossTaskSendMode, IssueStateFilter,
    ReviewCheckReport, ReviewFinding, ReviewVerdict, RuntimeToolDefinition, TaskLinkRole,
    WorkItemView,
};
use serde::de::DeserializeOwned;
use serde_json::json;

/// Default and largest page `board_list` returns to a model.
const LIST_DEFAULT: u32 = 20;
const LIST_MAX: u32 = 25;
/// Default and largest history bound per `board_read` detail list.
const HISTORY_DEFAULT: u32 = 5;
const HISTORY_MAX: u32 = 10;
const OPTIONS_MAX: usize = 20;
const ISSUES_MAX: u32 = 20;
/// The brief `board_read` returns (characters); receipts carry a shorter
/// excerpt and lists a one-paragraph one.
const READ_DESCRIPTION_CHARS: usize = 12_000;
const RECEIPT_DESCRIPTION_CHARS: usize = 2_000;
const LIST_EXCERPT_CHARS: usize = 240;
/// Any other string in a result (launch instructions, findings, reports).
const STRING_CHARS: usize = 1_500;
const STATUS_SUMMARY_CHARS: usize = 2_000;
const STATUS_EVIDENCE_MAX: usize = 10;
const PROMPT_CHARS: usize = 8_000;
const REVIEW_SUMMARY_CHARS: usize = 4_000;
const REVIEW_FINDINGS_MAX: usize = 50;
/// Checks and evidence references one review_start reports (A5 bound).
const REVIEW_REPORTS_MAX: usize = 16;

type BoardToolFuture<'a> = futures::future::BoxFuture<'a, BoardResult<Value>>;
const ERROR_DETAIL_CHARS: usize = 300;
/// Serialized ceiling for one board tool result (the runtime refuses more).
pub(crate) const BOARD_TOOL_OUTPUT_BYTES: usize = 64 * 1024;
const OUTPUT_TARGET_BYTES: usize = 60 * 1024;

const READ_ONLY: &str = "read-only";
const DURABLE: &str =
    "durable requestId receipt: repeating the exact call with the same key returns it";

fn storage(error: impl std::fmt::Display) -> BoardError {
    BoardError::Storage {
        message: error.to_string(),
    }
}

fn bounded_text(value: &str, chars: usize) -> String {
    if value.chars().count() <= chars {
        value.to_owned()
    } else {
        let mut text: String = value.chars().take(chars).collect();
        text.push('…');
        text
    }
}

// ---------------------------------------------------------------------------
// Audiences and role-shaped catalogs
// ---------------------------------------------------------------------------

fn read_only(name: &str) -> bool {
    matches!(
        name,
        BOARD_TOOL_LIST
            | BOARD_TOOL_READ
            | BOARD_TOOL_EXECUTION_OPTIONS
            | BOARD_TOOL_REVIEW_STATUS
            | BOARD_TOOL_ISSUE_SEARCH
    )
}

/// The tools a caller's audience may use, in catalog order. Each tool is
/// offered only where it applies: review tools to ticket workers (and
/// `review_status`/`review_submit` to reviewers), status reports to workers,
/// authoring tools to authoring sessions. Links, brief updates and
/// dependencies are `board_control` actions, not tools of their own.
fn audience_tools(context: &BoardRuntimeContext) -> Vec<&'static str> {
    let permitted = &context.permitted;
    let worker = context.audience == BoardToolAudience::TicketWorker;
    let allowed = |name: &str| -> bool {
        match context.audience {
            BoardToolAudience::Reviewer => matches!(
                name,
                BOARD_TOOL_READ | BOARD_TOOL_REVIEW_STATUS | BOARD_TOOL_REVIEW_SUBMIT
            ),
            BoardToolAudience::Authoring => matches!(
                name,
                BOARD_TOOL_LIST
                    | BOARD_TOOL_READ
                    | BOARD_TOOL_EXECUTION_OPTIONS
                    | BOARD_TOOL_CREATE
                    | BOARD_TOOL_START
                    | BOARD_TOOL_AUTHORING_CHECKPOINT
            ),
            BoardToolAudience::MainChat
            | BoardToolAudience::SideChat
            | BoardToolAudience::TicketWorker => match name {
                BOARD_TOOL_LIST
                | BOARD_TOOL_READ
                | BOARD_TOOL_EXECUTION_OPTIONS
                | BOARD_TOOL_CREATE
                | BOARD_TOOL_UPDATE
                | BOARD_TOOL_MOVE
                | BOARD_TOOL_CONTROL
                | BOARD_TOOL_ISSUE_SEARCH
                | BOARD_TOOL_ISSUE_IMPORT => true,
                BOARD_TOOL_START => permitted.start_ticket,
                BOARD_TOOL_CONVERT_CHAT => permitted.convert_to_ticket && !worker,
                BOARD_TOOL_CREATE_SIDE_CHAT => {
                    permitted.create_side_fresh
                        || permitted.create_side_with_context
                        || permitted.create_sibling_fresh
                }
                BOARD_TOOL_CREATE_MAIN_CHAT => {
                    permitted.create_main || permitted.create_main_with_handoff
                }
                BOARD_TOOL_SET_STATUS
                | BOARD_TOOL_REVIEW_START
                | BOARD_TOOL_REVIEW_STATUS
                | BOARD_TOOL_REVIEW_CANCEL => worker,
                _ => false,
            },
        }
    };
    BOARD_TOOL_NAMES
        .iter()
        .copied()
        .filter(|name| allowed(name))
        .collect()
}

/// Board tool names a context's audience gets (before delivery gating).
pub(crate) fn board_tool_names(context: &BoardRuntimeContext) -> Vec<String> {
    audience_tools(context)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn audience_phrase(context: &BoardRuntimeContext) -> &'static str {
    match (context.audience, context.role) {
        (BoardToolAudience::Authoring, _) => "a temporary ticket-authoring session",
        (BoardToolAudience::Reviewer, _) => "a reviewer side chat",
        (BoardToolAudience::TicketWorker, _) => "a ticket's working chat",
        (_, Some(BoardChatRole::Side)) => "a side chat",
        _ => "a main chat",
    }
}

/// A nominal context for an audience alone (the contract's role-agnostic
/// view). Ticket workers and reviewers get the narrower side-chat modes.
fn nominal_context(audience: BoardToolAudience) -> BoardRuntimeContext {
    let (role, permitted) = match audience {
        BoardToolAudience::MainChat => (
            Some(BoardChatRole::Main),
            ChatPermittedActions {
                create_side_fresh: true,
                create_side_with_context: true,
                create_main: true,
                create_main_with_handoff: true,
                convert_to_ticket: true,
                start_ticket: true,
                ..ChatPermittedActions::default()
            },
        ),
        BoardToolAudience::SideChat | BoardToolAudience::TicketWorker => (
            Some(BoardChatRole::Side),
            ChatPermittedActions {
                create_sibling_fresh: true,
                create_main: true,
                create_main_with_handoff: true,
                convert_to_ticket: audience == BoardToolAudience::SideChat,
                start_ticket: true,
                ..ChatPermittedActions::default()
            },
        ),
        BoardToolAudience::Reviewer => (Some(BoardChatRole::Side), ChatPermittedActions::default()),
        BoardToolAudience::Authoring => (
            None,
            ChatPermittedActions {
                start_ticket: true,
                ..ChatPermittedActions::default()
            },
        ),
    };
    BoardRuntimeContext {
        audience,
        task_id: None,
        role,
        main_parent_id: None,
        authoring_session_id: None,
        project_id: String::new(),
        ticket: None,
        permitted,
        parent_locked: audience == BoardToolAudience::TicketWorker,
        tools: Vec::new(),
        display_container_id: None,
        creator: None,
        source: None,
        context_origin: None,
        original_container: None,
        parent_unavailable: false,
        owner_host_id: None,
    }
}

fn definition(name: &'static str, context: &BoardRuntimeContext) -> RuntimeToolDefinition {
    let read = read_only(name);
    RuntimeToolDefinition {
        name: name.into(),
        version: 2,
        description: description(name, context).into(),
        input_schema: input_schema(name, context),
        output_schema: json!({"type":"object"}),
        required_scopes: vec![
            if read {
                BOARD_SCOPE_READ
            } else {
                BOARD_SCOPE_WRITE
            }
            .into(),
        ],
        deadline_ms: match name {
            BOARD_TOOL_LIST | BOARD_TOOL_READ | BOARD_TOOL_REVIEW_STATUS => 10_000,
            BOARD_TOOL_EXECUTION_OPTIONS | BOARD_TOOL_ISSUE_SEARCH => 30_000,
            BOARD_TOOL_MOVE
            | BOARD_TOOL_START
            | BOARD_TOOL_CONVERT_CHAT
            | BOARD_TOOL_CREATE_SIDE_CHAT
            | BOARD_TOOL_CREATE_MAIN_CHAT
            | BOARD_TOOL_REVIEW_START
            | BOARD_TOOL_ISSUE_IMPORT
            | BOARD_TOOL_CONTROL => 60_000,
            _ => 20_000,
        },
        idempotency: if read {
            READ_ONLY
        } else if name == BOARD_TOOL_AUTHORING_CHECKPOINT {
            "expectedVersion compare-and-set"
        } else {
            DURABLE
        }
        .into(),
        presentation: "board".into(),
        activity_classification: "luna-board".into(),
    }
}

/// The definitions for a concrete run context (role-shaped schemas).
pub(crate) fn board_tool_set(context: &BoardRuntimeContext) -> Vec<RuntimeToolDefinition> {
    audience_tools(context)
        .into_iter()
        .map(|name| definition(name, context))
        .collect()
}

/// One sentence per tool: the shared rules (request keys, untrusted text,
/// settings shapes, control actions) live once in the instructions' board
/// context and the `get_workspace` `board` guide, never in every schema.
fn description(name: &str, context: &BoardRuntimeContext) -> &'static str {
    let side = context.role == Some(BoardChatRole::Side);
    let authoring = context.audience == BoardToolAudience::Authoring;
    match name {
        BOARD_TOOL_LIST => "List this project's tickets (bounded page).",
        BOARD_TOOL_READ => "Read one ticket: brief, revisions, links, launches, reviews, events.",
        BOARD_TOOL_EXECUTION_OPTIONS => {
            "Real harness/model ids, reasoning, access modes and project defaults for ticket settings."
        }
        BOARD_TOOL_CREATE if authoring => {
            "Save the agreed ticket in Backlog when the user asks. Starts nothing."
        }
        BOARD_TOOL_CREATE => {
            "Save a ticket in Backlog; starts nothing. This chat becomes its source. Attachments accept only images supplied to this chat or discussion."
        }
        BOARD_TOOL_UPDATE => {
            "Edit ticket fields against expectedRevision; a conflict returns staleRevision with the ticket."
        }
        BOARD_TOOL_MOVE => {
            "Move to ready (start or continue its work) or backlog (stop or hold), like dragging it."
        }
        BOARD_TOOL_START if authoring => {
            "Start a saved ticket in a new main chat, only when the user asks."
        }
        BOARD_TOOL_START => {
            "Start or continue workItemId (or up to 10 workItemIds); an existing worker continues in place."
        }
        BOARD_TOOL_CONTROL => {
            "Ticket actions: stop, continue, retry, cancelQueued, archive, unarchive, delete, sendUpdate (send the edited brief), link/unlink (taskId, role), block/unblock (blockedById). Delete only when requested: requires expectedRevision and a settled ticket, preserves chats/files/history, cannot be restored."
        }
        BOARD_TOOL_SET_STATUS => {
            "Report progress, blocked, unblocked, readyForReview or a note on your ticket; Done is engine-owned."
        }
        BOARD_TOOL_CONVERT_CHAT => {
            "Make THIS chat a new ticket's working chat, in place; nothing is resent."
        }
        BOARD_TOOL_REVIEW_START => {
            "Submit your ticket's current result for automatic review with the checks and evidence you ran."
        }
        BOARD_TOOL_REVIEW_STATUS => "Review runs and remaining repair rounds.",
        BOARD_TOOL_REVIEW_CANCEL => "Cancel a review run.",
        BOARD_TOOL_REVIEW_SUBMIT => {
            "Submit your review run's one structured result: verdict, basisPacketHash, findings, summary."
        }
        BOARD_TOOL_CREATE_SIDE_CHAT if side => {
            "New fresh sibling side chat under your main parent; start:true sends prompt."
        }
        BOARD_TOOL_CREATE_SIDE_CHAT => {
            "New side chat under this chat (context fresh or withContext); start:true sends prompt."
        }
        BOARD_TOOL_CREATE_MAIN_CHAT if side => {
            "Hand work to a new main chat with a written handoffBrief; you stay here."
        }
        BOARD_TOOL_CREATE_MAIN_CHAT => {
            "New independent main chat (optional handoffBrief); this chat stays as it is."
        }
        BOARD_TOOL_AUTHORING_CHECKPOINT => {
            "Save the candidate ticket as this discussion's draft (no ticket, no chat)."
        }
        BOARD_TOOL_ISSUE_SEARCH => {
            "Search this project's GitHub issues; omit sourceId to list sources."
        }
        BOARD_TOOL_ISSUE_IMPORT => {
            "Add a GitHub issue as a ticket (an existing link returns its ticket); never writes to GitHub."
        }
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// Input schemas (role-shaped, compact)
// ---------------------------------------------------------------------------

fn object(properties: Value, required: &[&str]) -> Value {
    let mut schema = json!({"type":"object","properties":properties,"additionalProperties":false});
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

fn string() -> Value {
    json!({"type":"string"})
}

fn integer() -> Value {
    json!({"type":"integer"})
}

fn boolean() -> Value {
    json!({"type":"boolean"})
}

fn objects(description: &str) -> Value {
    json!({"type":"array","items":{"type":"object"},"description":description})
}

/// Requested settings; the full shape is in the `board` guide.
fn execution_schema() -> Value {
    json!({"type":"object","description":"harness, model, reasoning, permissionMode, providerAccountId, modelOptions, checkout; omitted fields inherit"})
}

fn review_profile_schema() -> Value {
    json!({"type":"object","description":"harness, model, reasoning; default: the implementation model"})
}

fn ticket_binding_schema() -> Value {
    json!({"type":"object","description":"{workItemId, expectedRevision}: work on this ticket"})
}

fn destination_schema(context: &BoardRuntimeContext) -> Option<Value> {
    if context.audience == BoardToolAudience::Authoring {
        return Some(json!({"enum":["newMain"]}));
    }
    match context.role {
        Some(BoardChatRole::Main) => Some(json!({"enum":["auto","newMain","sideChat"]})),
        Some(BoardChatRole::Side) => Some(json!({"enum":["auto","newMain","sibling"]})),
        None => None,
    }
}

fn with_context_allowed(context: &BoardRuntimeContext) -> bool {
    context.audience != BoardToolAudience::Authoring
        && context.role == Some(BoardChatRole::Main)
        && context.permitted.create_side_with_context
}

/// Main chats choose fresh or withContext; every other caller has only
/// fresh, so the property is absent from its schema.
fn insert_context_property(properties: &mut Value, context: &BoardRuntimeContext) {
    if with_context_allowed(context) {
        properties["context"] = json!({"enum":["fresh","withContext"]});
    }
}

fn input_schema(name: &str, context: &BoardRuntimeContext) -> Value {
    match name {
        BOARD_TOOL_LIST => object(
            json!({
                "columns": {"type":"array","items":{"enum":["backlog","ready","done"]}},
                "text": string(),
                "archived": {"enum":["exclude","include","only"]},
                "cursor": string(),
                "limit": integer(),
            }),
            &[],
        ),
        BOARD_TOOL_READ => object(
            json!({
                "workItemId": {"type":"string","description":"id or key, e.g. LUN-42"},
                "historyLimit": integer(),
            }),
            &["workItemId"],
        ),
        BOARD_TOOL_EXECUTION_OPTIONS => object(
            json!({"harness": string(), "query": string(), "offset": integer(), "limit": integer()}),
            &[],
        ),
        BOARD_TOOL_CREATE => {
            let mut properties = json!({
                "requestId": string(),
                "title": string(),
                "description": {"type":"string","description":"brief with acceptance criteria"},
                "implementation": execution_schema(),
                "review": review_profile_schema(),
                "completion": {"type":"object","description":"{mode: automaticChecksAndReview|manualAcceptance}"},
                "labels": {"type":"array","items":string()},
                "priority": {"enum":["urgent","high","medium","low"]},
                "contextRefs": objects("{kind:file,path}|{kind:url,url}|{kind:chatMessage,taskId,messageId}|{kind:note,text}"),
                "attachments": {"type":"array","items":string(),"maxItems":10,"description":"Upload paths of images supplied to this chat as attachments. Paths merely mentioned in text or belonging to another chat are refused."},
            });
            if context.audience != BoardToolAudience::Authoring {
                properties["sourceMessageId"] = string();
            }
            object(properties, &["requestId", "title"])
        }
        BOARD_TOOL_UPDATE => object(
            json!({
                "requestId": string(),
                "workItemId": string(),
                "expectedRevision": integer(),
                "changes": {"type":"object","description":"any board_create field to change; clearPriority:true"},
            }),
            &["requestId", "workItemId", "expectedRevision", "changes"],
        ),
        BOARD_TOOL_MOVE => object(
            json!({
                "requestId": string(),
                "workItemId": string(),
                "to": {"enum":["backlog","ready"]},
                "afterId": string(),
                "beforeId": string(),
            }),
            &["requestId", "workItemId", "to"],
        ),
        BOARD_TOOL_START => {
            let mut properties = json!({
                "requestId": string(),
                "workItemId": string(),
                "workItemIds": {"type":"array","items":string()},
                "execution": execution_schema(),
            });
            if let Some(destination) = destination_schema(context) {
                properties["destination"] = destination;
            }
            insert_context_property(&mut properties, context);
            if context.audience != BoardToolAudience::Authoring {
                properties["followUp"] = json!({"enum":["none","reportBack","reportAndContinue"]});
            }
            object(properties, &["requestId"])
        }
        BOARD_TOOL_CONTROL => object(
            json!({
                "requestId": string(),
                "workItemId": string(),
                "action": {"enum":["stop","continue","retry","cancelQueued","archive","unarchive","delete","sendUpdate","link","unlink","block","unblock"]},
                "expectedRevision": integer(),
                "taskId": string(),
                "role": {"enum":["source","reference","attempt"]},
                "blockedById": string(),
            }),
            &["requestId", "workItemId", "action"],
        ),
        BOARD_TOOL_SET_STATUS => object(
            json!({
                "requestId": string(),
                "workItemId": string(),
                "kind": {"enum":["progress","blocked","unblocked","readyForReview","note"]},
                "summary": string(),
                "evidence": objects("{label, path?, url?}"),
            }),
            &["requestId", "workItemId", "kind", "summary"],
        ),
        BOARD_TOOL_CONVERT_CHAT => object(
            json!({"requestId": string(), "description": string(), "title": string()}),
            &["requestId", "description"],
        ),
        BOARD_TOOL_REVIEW_START => object(
            json!({
                "requestId": string(),
                "workItemId": string(),
                "checks": objects("{name, passed, command?, exitCode?, detail?}"),
                "summary": string(),
                "evidence": objects("{label, path?, url?}"),
            }),
            &["requestId"],
        ),
        BOARD_TOOL_REVIEW_STATUS => object(
            json!({"workItemId": string(), "reviewRunId": string()}),
            &[],
        ),
        BOARD_TOOL_REVIEW_CANCEL => object(
            json!({"requestId": string(), "reviewRunId": string()}),
            &["requestId", "reviewRunId"],
        ),
        BOARD_TOOL_REVIEW_SUBMIT => object(
            json!({
                "requestId": string(),
                "verdict": {"enum":["passed","changesRequested","blocked","failed","cancelled"]},
                "basisPacketHash": string(),
                "findings": objects("{id, severity: blocker|major|minor|note, blocking, file?, line?, explanation, requiredAction}"),
                "summary": string(),
            }),
            &["requestId", "verdict", "basisPacketHash", "summary"],
        ),
        BOARD_TOOL_CREATE_SIDE_CHAT => {
            let mut properties = json!({
                "requestId": string(),
                "prompt": string(),
                "title": string(),
                "start": boolean(),
                "ticket": ticket_binding_schema(),
                "execution": execution_schema(),
            });
            insert_context_property(&mut properties, context);
            object(properties, &["requestId"])
        }
        BOARD_TOOL_CREATE_MAIN_CHAT => {
            let required: &[&str] = if context.role == Some(BoardChatRole::Side) {
                &["requestId", "handoffBrief"]
            } else {
                &["requestId"]
            };
            object(
                json!({
                    "requestId": string(),
                    "handoffBrief": {"type":"string","description":"goal, done, files, decisions, constraints, next step"},
                    "title": string(),
                    "start": boolean(),
                    "ticket": ticket_binding_schema(),
                    "execution": execution_schema(),
                }),
                required,
            )
        }
        BOARD_TOOL_AUTHORING_CHECKPOINT => object(
            json!({
                "expectedVersion": integer(),
                "proposal": {"type":"object","description":"board_create fields plus unresolved: settings still to ask"},
            }),
            &["expectedVersion", "proposal"],
        ),
        BOARD_TOOL_ISSUE_SEARCH => object(
            json!({
                "sourceId": string(),
                "query": string(),
                "state": {"enum":["open","closed","all"]},
                "cursor": string(),
                "limit": integer(),
            }),
            &[],
        ),
        BOARD_TOOL_ISSUE_IMPORT => object(
            json!({
                "requestId": string(),
                "sourceId": string(),
                "number": integer(),
                "additions": string(),
                "start": {"type":"object","description":"{destination?, execution?}: also start it"},
            }),
            &["requestId", "sourceId", "number"],
        ),
        _ => object(json!({}), &[]),
    }
}

// ---------------------------------------------------------------------------
// Tool arguments (no caller fields; claimed ids are checked, never used)
// ---------------------------------------------------------------------------

/// `null` clears, absent leaves the field unchanged.
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// A destination as a role-relative word or the canonical object form.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DestinationArg {
    Word(String),
    Full(BoardDestination),
}

/// A context mode as a word or the canonical object form.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ContextArg {
    Word(String),
    Full(BoardContextMode),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    columns: Vec<BoardColumn>,
    #[serde(default)]
    archived: BoardArchivedFilter,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    attention_only: bool,
    #[serde(default)]
    source: Option<BoardSourceKind>,
    #[serde(default)]
    provider: Option<luna_proto::HarnessId>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadArgs {
    work_item_id: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    history_limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OptionsArgs {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    harness: Option<luna_proto::HarnessId>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateArgs {
    #[serde(default)]
    attachments: Vec<String>,
    request_id: String,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    implementation: Option<BoardExecutionProfile>,
    #[serde(default)]
    review: Option<BoardReviewProfile>,
    #[serde(default)]
    completion: Option<BoardCompletionPolicy>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    priority: Option<BoardPriority>,
    #[serde(default)]
    context_refs: Vec<BoardContextRef>,
    #[serde(default)]
    placement: BoardPlacement,
    #[serde(default)]
    source_message_id: Option<String>,
    /// Never advertised: a claimed origin is validated against the bound
    /// caller by the store (a foreign chat or session is refused).
    #[serde(default)]
    origin: Option<BoardItemOrigin>,
    #[serde(default)]
    caller_task_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChangesArgs {
    #[serde(default)]
    attachments: Option<Vec<String>>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    implementation: Option<BoardExecutionProfile>,
    #[serde(default)]
    review: Option<BoardReviewProfile>,
    #[serde(default)]
    completion: Option<BoardCompletionPolicy>,
    #[serde(default)]
    labels: Option<Vec<String>>,
    #[serde(default, deserialize_with = "double_option")]
    priority: Option<Option<BoardPriority>>,
    #[serde(default)]
    clear_priority: bool,
    #[serde(default)]
    context_refs: Option<Vec<BoardContextRef>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateArgs {
    request_id: String,
    work_item_id: String,
    expected_revision: u64,
    changes: ChangesArgs,
    #[serde(default)]
    caller_task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MoveArgs {
    request_id: String,
    work_item_id: String,
    to: BoardColumn,
    #[serde(default)]
    expected_revision: Option<u64>,
    #[serde(default)]
    after_id: Option<String>,
    #[serde(default)]
    before_id: Option<String>,
    #[serde(default)]
    destination: Option<DestinationArg>,
    #[serde(default)]
    context: Option<ContextArg>,
    #[serde(default)]
    checkpoint_message_id: Option<String>,
    #[serde(default)]
    execution: Option<BoardExecutionProfile>,
    #[serde(default)]
    follow_up: Option<BoardFollowUp>,
    #[serde(default)]
    caller_task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartArgs {
    request_id: String,
    #[serde(default)]
    work_item_id: Option<String>,
    #[serde(default)]
    work_item_ids: Option<Vec<String>>,
    #[serde(default)]
    expected_revision: Option<u64>,
    #[serde(default)]
    destination: Option<DestinationArg>,
    #[serde(default)]
    context: Option<ContextArg>,
    #[serde(default)]
    checkpoint_message_id: Option<String>,
    #[serde(default)]
    execution: Option<BoardExecutionProfile>,
    #[serde(default)]
    follow_up: Option<BoardFollowUp>,
    #[serde(default)]
    caller_task_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ControlArg {
    Delete,
    Stop,
    Continue,
    Retry,
    CancelQueued,
    Archive,
    Unarchive,
    /// Send the edited brief to the working chat (`board_send_update`).
    SendUpdate,
    /// Link or unlink a source/reference/attempt chat (`board_link_thread`).
    Link,
    Unlink,
    /// Add or remove a blocked-by dependency (`board_set_dependency`).
    Block,
    Unblock,
    /// Accepted only to explain that acceptance is the user's decision.
    Accept,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ControlArgs {
    request_id: String,
    work_item_id: String,
    action: ControlArg,
    #[serde(default)]
    expected_revision: Option<u64>,
    #[serde(default)]
    adopt_latest_brief: bool,
    /// link/unlink: the chat and its relation.
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    role: Option<TaskLinkRole>,
    /// block/unblock: the ticket this one waits for.
    #[serde(default)]
    blocked_by_id: Option<String>,
    /// sendUpdate: queue (default), steer or auto.
    #[serde(default)]
    mode: Option<CrossTaskSendMode>,
    #[serde(default)]
    caller_task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StatusArgs {
    request_id: String,
    work_item_id: String,
    kind: BoardStatusReportKind,
    summary: String,
    #[serde(default)]
    evidence: Vec<BoardEvidenceRef>,
    #[serde(default)]
    expected_revision: Option<u64>,
    #[serde(default)]
    execution_generation: Option<u64>,
    #[serde(default)]
    caller_task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConvertArgs {
    request_id: String,
    description: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    implementation: Option<BoardExecutionProfile>,
    #[serde(default)]
    review: Option<BoardReviewProfile>,
    #[serde(default)]
    completion: Option<BoardCompletionPolicy>,
    /// Never advertised: only this chat converts itself.
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    caller_task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewStartArgs {
    request_id: String,
    /// Default: this worker's own ticket.
    #[serde(default)]
    work_item_id: Option<String>,
    #[serde(default)]
    execution_generation: Option<u64>,
    #[serde(default)]
    profile: Option<BoardReviewProfile>,
    /// Checks the implementer ran on its current result.
    #[serde(default)]
    checks: Vec<ReviewCheckReport>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    evidence: Vec<BoardEvidenceRef>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewStatusArgs {
    #[serde(default)]
    work_item_id: Option<String>,
    #[serde(default)]
    review_run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewCancelArgs {
    request_id: String,
    review_run_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewSubmitArgs {
    request_id: String,
    #[serde(default)]
    review_run_id: Option<String>,
    verdict: ReviewVerdict,
    basis_packet_hash: String,
    #[serde(default)]
    findings: Vec<ReviewFinding>,
    summary: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TicketArg {
    work_item_id: String,
    expected_revision: u64,
}

impl From<TicketArg> for BoardTicketBinding {
    fn from(ticket: TicketArg) -> Self {
        Self {
            work_item_id: ticket.work_item_id,
            expected_revision: ticket.expected_revision,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SideChatArgs {
    request_id: String,
    #[serde(default)]
    context: Option<ContextArg>,
    #[serde(default)]
    checkpoint_message_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    start: bool,
    #[serde(default)]
    ticket: Option<TicketArg>,
    #[serde(default)]
    execution: Option<BoardExecutionProfile>,
    /// Never advertised: the parent is resolved by the engine.
    #[serde(default)]
    caller_task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MainChatArgs {
    request_id: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    handoff_brief: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    start: bool,
    #[serde(default)]
    ticket: Option<TicketArg>,
    #[serde(default)]
    execution: Option<BoardExecutionProfile>,
    /// Never advertised: the handoff source is this chat.
    #[serde(default)]
    from_task_id: Option<String>,
    #[serde(default)]
    caller_task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CheckpointArgs {
    expected_version: u64,
    proposal: AuthoringProposal,
    /// Never advertised: only this session checkpoints itself.
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IssueSearchArgs {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    source_id: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    state: IssueStateFilter,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    refresh: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportStartArgs {
    #[serde(default)]
    destination: Option<DestinationArg>,
    #[serde(default)]
    execution: Option<BoardExecutionProfile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IssueImportArgs {
    request_id: String,
    #[serde(default)]
    project_id: Option<String>,
    source_id: String,
    number: u64,
    #[serde(default)]
    additions: Option<String>,
    #[serde(default)]
    comment_ids: Vec<String>,
    #[serde(default)]
    start: Option<ImportStartArgs>,
}

fn parse_args<T: DeserializeOwned>(arguments: Value) -> BoardResult<T> {
    serde_json::from_value(arguments).map_err(|error| {
        BoardError::invalid(
            "arguments",
            bounded_text(&error.to_string(), ERROR_DETAIL_CHARS),
        )
    })
}

fn bounded_limit(field: &str, value: Option<u32>, default: u32, max: u32) -> BoardResult<u32> {
    match value {
        None => Ok(default),
        Some(value) if (1..=max).contains(&value) => Ok(value),
        Some(_) => Err(BoardError::invalid(field, format!("use 1-{max}"))),
    }
}

/// A model may restate the run's own id; any other id is refused. The caller
/// is always bound from the run, never from this value.
fn claimed_self(
    context: &BoardRuntimeContext,
    field: &str,
    claimed: Option<&str>,
) -> BoardResult<()> {
    match (claimed, context.task_id.as_deref()) {
        (None, _) => Ok(()),
        (Some(claimed), Some(own)) if claimed == own => Ok(()),
        (Some(_), Some(own)) => Err(BoardError::NotAuthorized {
            reason: format!(
                "{field} is bound to this chat ({own}); a tool call can never act as another chat. Omit {field}."
            ),
        }),
        (Some(_), None) => Err(BoardError::NotAuthorized {
            reason: format!(
                "{field}: a ticket discussion is not a chat and cannot act as one. Omit {field}."
            ),
        }),
    }
}

fn destination_refusal(context: &BoardRuntimeContext) -> BoardError {
    let reason = if context.audience == BoardToolAudience::Authoring {
        "From a ticket discussion, work starts only in a new main chat (newMain).".to_owned()
    } else if context.role == Some(BoardChatRole::Side) {
        format!(
            "A side chat has no side chats of its own: use sibling (a fresh side chat under your main parent {}) or newMain. A destination can never name another chat as the parent.",
            context.main_parent_id.as_deref().unwrap_or("unknown")
        )
    } else {
        "From this main chat a new worker goes in a side chat under this chat (sideChat) or a new main chat (newMain); a destination can never name another chat as the parent.".to_owned()
    };
    BoardError::InvalidDestination {
        reason,
        canonical: None,
    }
}

/// Resolve a role-relative destination against the bound caller. Explicit
/// ids must name the caller itself; anything else is refused.
fn bound_destination(
    context: &BoardRuntimeContext,
    argument: Option<DestinationArg>,
) -> BoardResult<BoardDestination> {
    let own = context.task_id.clone();
    let authoring = context.audience == BoardToolAudience::Authoring;
    let role = if authoring { None } else { context.role };
    let destination = match argument {
        None => BoardDestination::Auto,
        Some(DestinationArg::Word(word)) => match (word.as_str(), role, own) {
            ("auto", _, _) => BoardDestination::Auto,
            ("newMain", _, _) => BoardDestination::NewMain,
            ("sideChat", Some(BoardChatRole::Main), Some(own)) => {
                BoardDestination::SideOf { main_task_id: own }
            }
            ("sibling", Some(BoardChatRole::Side), Some(own)) => {
                BoardDestination::SiblingOf { side_task_id: own }
            }
            ("sideChat" | "sibling", _, _) => return Err(destination_refusal(context)),
            (other, _, _) => {
                return Err(BoardError::invalid(
                    "destination",
                    format!("unknown destination {}", bounded_text(other, 40)),
                ));
            }
        },
        Some(DestinationArg::Full(destination)) => destination,
    };
    let own = context.task_id.as_deref();
    match (&destination, role) {
        (BoardDestination::Auto, None) => Ok(BoardDestination::NewMain),
        (BoardDestination::NewMain, _) => Ok(destination),
        (BoardDestination::Auto, Some(_)) => Ok(destination),
        (BoardDestination::SideOf { main_task_id }, Some(BoardChatRole::Main))
            if Some(main_task_id.as_str()) == own =>
        {
            Ok(destination)
        }
        (BoardDestination::SiblingOf { side_task_id }, Some(BoardChatRole::Side))
            if Some(side_task_id.as_str()) == own =>
        {
            Ok(destination)
        }
        _ => Err(destination_refusal(context)),
    }
}

/// Fresh, or (main callers only) with context. A side chat never forks or
/// copies its conversation, and a ticket discussion has none to copy.
fn bound_context(
    context: &BoardRuntimeContext,
    operation: &str,
    argument: Option<ContextArg>,
    checkpoint: Option<String>,
) -> BoardResult<BoardContextMode> {
    let mode = match argument {
        None if checkpoint.is_some() => BoardContextMode::WithContext {
            checkpoint_message_id: checkpoint,
        },
        None => BoardContextMode::Fresh,
        Some(ContextArg::Word(word)) => match word.as_str() {
            "fresh" if checkpoint.is_some() => {
                return Err(BoardError::invalid(
                    "checkpointMessageId",
                    "applies only to withContext",
                ));
            }
            "fresh" => BoardContextMode::Fresh,
            "withContext" => BoardContextMode::WithContext {
                checkpoint_message_id: checkpoint,
            },
            other => {
                return Err(BoardError::invalid(
                    "context",
                    format!("unknown context {}; use fresh", bounded_text(other, 40)),
                ));
            }
        },
        Some(ContextArg::Full(mode)) => mode,
    };
    if matches!(mode, BoardContextMode::WithContext { .. }) && !with_context_allowed(context) {
        let reason = if context.audience == BoardToolAudience::Authoring {
            "A ticket discussion is not a chat and has no conversation to copy; work starts fresh from the saved brief."
        } else if context.role == Some(BoardChatRole::Side) {
            "A side chat cannot fork or copy its conversation into another chat. Use fresh (the brief and prompt still apply), or create_main_chat with a written handoffBrief."
        } else {
            "This chat cannot share its conversation context here; use fresh."
        };
        return Err(BoardError::InvalidRole {
            operation: format!("{operation}.withContext"),
            reason: reason.into(),
            allowed: vec!["fresh".into()],
        });
    }
    Ok(mode)
}

fn bound_follow_up(
    context: &BoardRuntimeContext,
    follow_up: Option<BoardFollowUp>,
) -> BoardResult<BoardFollowUp> {
    match follow_up {
        Some(follow_up)
            if follow_up != BoardFollowUp::None
                && context.audience == BoardToolAudience::Authoring =>
        {
            Err(BoardError::invalid(
                "followUp",
                "a ticket discussion is not a chat that can receive results",
            ))
        }
        other => Ok(other.unwrap_or_default()),
    }
}

// ---------------------------------------------------------------------------
// Bounded results
// ---------------------------------------------------------------------------

/// Bound strings, briefs and lists in a result. Briefs keep a flag and their
/// full length so the model knows to read more instead of guessing.
fn bound_value(value: &mut Value, description_chars: usize, string_chars: usize, items: usize) {
    match value {
        Value::Object(object) => {
            let mut notes = Vec::new();
            for (key, entry) in object.iter_mut() {
                match entry {
                    Value::String(text) if key == "description" => {
                        let chars = text.chars().count();
                        if chars > description_chars {
                            *text = bounded_text(text, description_chars);
                            notes.push(("descriptionTruncated".to_owned(), json!(true)));
                            notes.push(("descriptionChars".to_owned(), json!(chars)));
                        }
                    }
                    Value::Array(list) if list.len() > items => {
                        notes.push((format!("{key}Omitted"), json!(list.len() - items)));
                        list.truncate(items);
                        for entry in list.iter_mut() {
                            bound_value(entry, description_chars, string_chars, items);
                        }
                    }
                    other => bound_value(other, description_chars, string_chars, items),
                }
            }
            for (key, note) in notes {
                object.entry(key).or_insert(note);
            }
        }
        Value::Array(list) => {
            for entry in list.iter_mut() {
                bound_value(entry, description_chars, string_chars, items);
            }
        }
        Value::String(text) if text.chars().count() > string_chars => {
            *text = bounded_text(text, string_chars);
        }
        _ => {}
    }
}

fn prune_nulls(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.retain(|_, entry| !entry.is_null());
            object.values_mut().for_each(prune_nulls);
        }
        Value::Array(list) => list.iter_mut().for_each(prune_nulls),
        _ => {}
    }
}

fn encoded_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

/// Fit a result under the output ceiling, tightening bounds in stages. The
/// last stage keeps only top-level scalars and says so: a committed receipt
/// is never turned into a failure by its size.
fn fit_output(value: &mut Value, description_chars: usize) {
    prune_nulls(value);
    for (description, strings, items) in [
        (description_chars, STRING_CHARS, usize::MAX),
        (description_chars.min(1_000), 400, 10),
        (LIST_EXCERPT_CHARS, 160, 3),
    ] {
        let mut candidate = value.clone();
        bound_value(&mut candidate, description, strings, items);
        if encoded_len(&candidate) <= OUTPUT_TARGET_BYTES {
            *value = candidate;
            return;
        }
    }
    let mut summary = serde_json::Map::new();
    if let Value::Object(object) = value {
        for (key, entry) in object.iter() {
            if !entry.is_object() && !entry.is_array() {
                let mut entry = entry.clone();
                bound_value(&mut entry, 160, 160, 0);
                summary.insert(key.clone(), entry);
            }
        }
    }
    summary.insert("outputTruncated".into(), json!(true));
    *value = Value::Object(summary);
}

fn tool_output<T: Serialize>(value: &T) -> BoardResult<Value> {
    let mut value = serde_json::to_value(value).map_err(storage)?;
    fit_output(&mut value, RECEIPT_DESCRIPTION_CHARS);
    Ok(value)
}

fn untrusted(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.insert("contentUntrusted".into(), json!(true));
    }
    value
}

fn compact_view(view: &WorkItemView) -> Value {
    let item = &view.item;
    json!({
        "workItemId": item.id,
        "key": item.key,
        "projectId": item.project_id,
        "title": item.title,
        "column": item.column,
        "archived": item.archived,
        "phase": view.phase,
        "revision": item.revision,
        "briefRevision": item.brief_revision,
        "priority": item.priority,
        "labels": item.labels,
        "origin": item.origin,
        "implementation": item.implementation,
        "completion": item.completion.mode,
        "attention": view.attention,
        "workingChat": view.working_chat,
        "queueReason": view.queue_reason,
        "issue": view.issue,
        "blockedBy": view.blocked_by,
        "actions": view.actions,
        "lastReport": item.last_report.as_ref().map(|report| json!({
            "kind": report.report.kind,
            "summary": bounded_text(&report.report.summary, LIST_EXCERPT_CHARS),
            "atMs": report.at_ms,
        })),
        "lastActivityMs": view.last_activity_ms,
        "descriptionExcerpt": bounded_text(&item.description, LIST_EXCERPT_CHARS),
    })
}

fn list_output(page: &BoardListPage) -> Value {
    let mut value = json!({
        "items": page.items.iter().map(compact_view).collect::<Vec<_>>(),
        "nextCursor": page.next_cursor,
        "projects": page.projects,
        "asOfSeq": page.as_of_seq,
    });
    fit_output(&mut value, LIST_EXCERPT_CHARS);
    untrusted(value)
}

fn read_output(detail: &BoardItemDetail) -> BoardResult<Value> {
    let mut value = serde_json::to_value(detail).map_err(storage)?;
    fit_output(&mut value, READ_DESCRIPTION_CHARS);
    Ok(untrusted(value))
}

/// A board error as tool output: its JSON (bounded) plus a `message`.
pub(crate) fn board_error_value(error: &BoardError) -> Value {
    let mut value = serde_json::to_value(error).unwrap_or_else(|_| json!({"code":"storage"}));
    fit_output(&mut value, RECEIPT_DESCRIPTION_CHARS);
    if let Some(object) = value.as_object_mut() {
        object.insert("message".into(), Value::String(error.message()));
    }
    value
}

pub(crate) fn board_error_text(error: &BoardError) -> String {
    board_error_value(error).to_string()
}

/// The answer for a board call whose wait ended before its outcome was known
/// (cancelled turn or elapsed deadline). The operation keeps running; the
/// same requestId reconciles it.
pub(crate) fn board_outcome_unknown_text(request_id: Option<&str>, detail: &str) -> String {
    let error = match request_id {
        Some(request_id) => BoardError::OutcomeUnknown {
            request_id: request_id.into(),
        },
        None => BoardError::Unavailable {
            reason: format!("The board tool {detail}."),
        },
    };
    let mut value = board_error_value(&error);
    if let Some(object) = value.as_object_mut() {
        object.insert("detail".into(), Value::String(detail.into()));
        if request_id.is_some() {
            object.insert(
                "next".into(),
                Value::String(
                    "Repeat the same call with the same requestId to read its receipt; it will not act twice.".into(),
                ),
            );
        }
    }
    value.to_string()
}

// ---------------------------------------------------------------------------
// Engine entry points
// ---------------------------------------------------------------------------

impl EngineRpc {
    /// The board tool definitions attached for an audience (only modes that
    /// apply to the caller's canonical role; engine checks still enforce).
    /// Concrete runs use [`Self::board_tools_for_authority`], which shapes
    /// the schemas from the run's own permitted actions.
    #[allow(dead_code)] // contract API (§7.5); runs use board_tools_for_authority
    pub(crate) fn board_tool_definitions(
        &self,
        audience: BoardToolAudience,
    ) -> BoardResult<Vec<RuntimeToolDefinition>> {
        Ok(board_tool_set(&nominal_context(audience)))
    }

    /// The caller a run's authority binds: a temporary authoring session
    /// when the engine-built thread id names one, otherwise the chat run.
    fn board_tool_caller(&self, authority: &CrossTaskAuthority) -> BoardResult<BoardCaller> {
        if self
            .store
            .is_board_authoring_session(&authority.caller_task_id)
            .map_err(board_store_error)?
        {
            let session = self
                .store
                .board_authoring_session(&authority.caller_task_id)
                .map_err(board_store_error)?
                .ok_or_else(|| BoardError::Unavailable {
                    reason: "This ticket discussion no longer exists.".into(),
                })?;
            if !authority.visible_project_ids.contains(&session.project_id) {
                return Err(BoardError::NotAuthorized {
                    reason: "This discussion can only use its own project's board.".into(),
                });
            }
            return Ok(BoardCaller::Authoring {
                session_id: session.id,
                project_id: session.project_id,
            });
        }
        Ok(BoardCaller::from_authority(authority))
    }

    /// The caller's current canonical context, re-derived for every call.
    fn board_caller_context(&self, caller: &BoardCaller) -> BoardResult<BoardRuntimeContext> {
        match caller {
            BoardCaller::Agent {
                task_id,
                project_ids,
                ..
            } => {
                let context = self.board_runtime_context(task_id)?.ok_or_else(|| {
                    BoardError::Unavailable {
                        reason: "This chat has no project board on this host.".into(),
                    }
                })?;
                if context.audience == BoardToolAudience::Authoring
                    || !project_ids.contains(&context.project_id)
                {
                    return Err(BoardError::NotAuthorized {
                        reason: "This run is not bound to that chat's project.".into(),
                    });
                }
                Ok(context)
            }
            BoardCaller::Authoring {
                session_id,
                project_id,
            } => {
                let context = self.board_authoring_runtime_context(session_id)?;
                if &context.project_id != project_id {
                    return Err(BoardError::NotAuthorized {
                        reason: "This discussion can only use its own project's board.".into(),
                    });
                }
                Ok(context)
            }
            BoardCaller::User { .. } | BoardCaller::Engine { .. } => {
                Err(BoardError::NotAuthorized {
                    reason: "Board tools serve provider runs; the Board UI uses Board RPCs.".into(),
                })
            }
        }
    }

    /// Role-shaped board tools for a run's authority (empty without a local
    /// project board). Delivery and scopes are gated by the catalog.
    pub(crate) fn board_tools_for_authority(
        &self,
        authority: &CrossTaskAuthority,
    ) -> Vec<RuntimeToolDefinition> {
        let context = match self.board_tool_caller(authority) {
            Ok(caller) => self.board_caller_context(&caller),
            Err(error) => Err(error),
        };
        match context {
            Ok(context) => board_tool_set(&context),
            Err(error) => {
                if !matches!(error, BoardError::Unavailable { .. }) {
                    tracing::debug!(task_id = %authority.caller_task_id, code = %error.code(), "no board tools: {error}");
                }
                Vec::new()
            }
        }
    }

    /// Backend glue: bind the caller from the run's authority and execute;
    /// a board error travels as its JSON text.
    pub(crate) async fn board_tool_invoke(
        &self,
        authority: &CrossTaskAuthority,
        name: &str,
        arguments: Value,
    ) -> Result<Value, CrossTaskError> {
        let result = match self.board_tool_caller(authority) {
            Ok(caller) => self.board_tool_call(&caller, name, arguments).await,
            Err(error) => Err(error),
        };
        result.map_err(|error| CrossTaskError::Backend(board_error_text(&error)))
    }

    /// Execute one board tool call for a bound caller: parse the tool's own
    /// argument struct, translate to the matching board request (binding
    /// caller ids from the run) and call the same service function as the
    /// RPC dispatcher. Returns the bounded receipt or a `BoardError`.
    pub(crate) async fn board_tool_call(
        &self,
        caller: &BoardCaller,
        tool: &str,
        arguments: Value,
    ) -> BoardResult<Value> {
        let context = self.board_caller_context(caller)?;
        let allowed = audience_tools(&context);
        if !allowed.contains(&tool) {
            return Err(if BOARD_TOOL_NAMES.contains(&tool) {
                BoardError::InvalidRole {
                    operation: tool.into(),
                    reason: format!("{tool} is not available to {}.", audience_phrase(&context)),
                    allowed: allowed.iter().map(|name| (*name).to_owned()).collect(),
                }
            } else {
                BoardError::invalid("tool", "not a board tool")
            });
        }
        let arguments = if arguments.is_null() {
            json!({})
        } else {
            arguments
        };
        let project = context.project_id.clone();
        match tool {
            BOARD_TOOL_LIST => {
                self.board_tool_list_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_READ => {
                self.board_tool_read_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_EXECUTION_OPTIONS => {
                self.board_tool_execution_options_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_CREATE => {
                self.board_tool_create_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_UPDATE => {
                self.board_tool_update_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_MOVE => {
                self.board_tool_move_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_START => {
                self.board_tool_start_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_CONTROL => {
                self.board_tool_control_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_SET_STATUS => {
                self.board_tool_set_status_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_CONVERT_CHAT => {
                self.board_tool_convert_chat_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_REVIEW_START => {
                self.board_tool_review_start_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_REVIEW_STATUS => {
                self.board_tool_review_status_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_REVIEW_CANCEL => {
                self.board_tool_review_cancel_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_REVIEW_SUBMIT => {
                self.board_tool_review_submit_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_CREATE_SIDE_CHAT => {
                self.board_tool_create_side_chat_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_CREATE_MAIN_CHAT => {
                self.board_tool_create_main_chat_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_AUTHORING_CHECKPOINT => {
                self.board_tool_authoring_checkpoint_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_ISSUE_SEARCH => {
                self.board_tool_issue_search_call(caller, context, tool, project, arguments)
                    .await
            }
            BOARD_TOOL_ISSUE_IMPORT => {
                self.board_tool_issue_import_call(caller, context, tool, project, arguments)
                    .await
            }
            _ => Err(BoardError::invalid("tool", "not a board tool")),
        }
    }

    // One boxed future per tool keeps debug-build poll frames small: a
    // tool call runs on a worker stack below the provider/RPC frames, and
    // the inline match held every arm's callee future at once (~900 KiB).
    fn board_tool_list_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        _context: BoardRuntimeContext,
        _tool: &'a str,
        project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: ListArgs = parse_args(arguments)?;
            let limit = bounded_limit("limit", args.limit, LIST_DEFAULT, LIST_MAX)?;
            let page = self
                .board_list(
                    caller,
                    BoardListRequest {
                        scope: BoardScope::Project {
                            project_id: args.project_id.unwrap_or(project),
                        },
                        columns: args.columns,
                        archived: args.archived,
                        text: args.text,
                        attention_only: args.attention_only,
                        provider: args.provider,
                        source: args.source,
                        cursor: args.cursor,
                        limit: Some(limit),
                    },
                )
                .await?;
            Ok(list_output(&page))
        })
    }

    fn board_tool_read_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        _context: BoardRuntimeContext,
        _tool: &'a str,
        _project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: ReadArgs = parse_args(arguments)?;
            let history = bounded_limit(
                "historyLimit",
                args.history_limit,
                HISTORY_DEFAULT,
                HISTORY_MAX,
            )?;
            let detail = self
                .board_read(
                    caller,
                    BoardReadRequest {
                        work_item_id: args.work_item_id,
                        project_id: args.project_id,
                        history_limit: Some(history),
                    },
                )
                .await?;
            read_output(&detail)
        })
    }

    fn board_tool_execution_options_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        _context: BoardRuntimeContext,
        _tool: &'a str,
        project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: OptionsArgs = parse_args(arguments)?;
            if args
                .limit
                .is_some_and(|limit| limit == 0 || limit > OPTIONS_MAX)
            {
                return Err(BoardError::invalid("limit", format!("use 1-{OPTIONS_MAX}")));
            }
            let options = self
                .board_execution_options(
                    caller,
                    BoardExecutionOptionsRequest {
                        project_id: args.project_id.unwrap_or(project),
                        harness: args.harness,
                        query: args.query,
                        offset: args.offset,
                        limit: Some(args.limit.unwrap_or(OPTIONS_MAX)),
                    },
                )
                .await?;
            tool_output(&options)
        })
    }

    fn board_tool_create_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        context: BoardRuntimeContext,
        _tool: &'a str,
        project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: CreateArgs = parse_args(arguments)?;
            claimed_self(&context, "callerTaskId", args.caller_task_id.as_deref())?;
            let origin = match (args.origin, args.source_message_id, context.task_id.clone()) {
                // A claimed origin is checked against the bound caller.
                (Some(origin), _, _) => Some(origin),
                (None, Some(message_id), Some(task_id)) => Some(BoardItemOrigin::Chat {
                    task_id,
                    message_id: Some(message_id),
                }),
                (None, Some(_), None) => {
                    return Err(BoardError::invalid(
                        "sourceMessageId",
                        "a ticket discussion has no chat messages to cite",
                    ));
                }
                (None, None, _) => None,
            };
            let receipt = self
                .board_create(
                    caller,
                    BoardCreateRequest {
                        request_id: args.request_id,
                        project_id: Some(args.project_id.unwrap_or(project)),
                        title: args.title,
                        description: args.description,
                        implementation: args.implementation,
                        review: args.review,
                        completion: args.completion,
                        labels: args.labels,
                        priority: args.priority,
                        attachments: args.attachments,
                        input_items: Vec::new(),
                        context_refs: args.context_refs,
                        origin,
                        placement: args.placement,
                    },
                )
                .await?;
            tool_output(&receipt)
        })
    }

    fn board_tool_update_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        context: BoardRuntimeContext,
        _tool: &'a str,
        _project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: UpdateArgs = parse_args(arguments)?;
            claimed_self(&context, "callerTaskId", args.caller_task_id.as_deref())?;
            let changes = args.changes;
            let priority = match (changes.priority, changes.clear_priority) {
                (Some(Some(_)), true) => {
                    return Err(BoardError::invalid(
                        "clearPriority",
                        "pass either priority or clearPriority",
                    ));
                }
                (_, true) => Some(None),
                (priority, false) => priority,
            };
            let receipt = self
                .board_update(
                    caller,
                    BoardUpdateRequest {
                        request_id: args.request_id,
                        work_item_id: args.work_item_id,
                        expected_revision: args.expected_revision,
                        changes: BoardItemChanges {
                            attachments: changes.attachments,
                            title: changes.title,
                            description: changes.description,
                            implementation: changes.implementation,
                            review: changes.review,
                            completion: changes.completion,
                            labels: changes.labels,
                            priority,
                            context_refs: changes.context_refs,
                        },
                    },
                )
                .await?;
            tool_output(&receipt)
        })
    }

    fn board_tool_move_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        context: BoardRuntimeContext,
        tool: &'a str,
        _project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: MoveArgs = parse_args(arguments)?;
            claimed_self(&context, "callerTaskId", args.caller_task_id.as_deref())?;
            if args.to == BoardColumn::Done {
                return Err(BoardError::invalid(
                    "to",
                    "Done is engine-owned: it is set after current checks and review pass. Move to backlog (hold or stop) or ready (start or continue).",
                ));
            }
            let destination = bound_destination(&context, args.destination)?;
            let mode = bound_context(&context, tool, args.context, args.checkpoint_message_id)?;
            let follow_up = bound_follow_up(&context, args.follow_up)?;
            let receipt = self
                .board_move(
                    caller,
                    BoardMoveRequest {
                        request_id: args.request_id,
                        work_item_id: args.work_item_id,
                        to: args.to,
                        expected_revision: args.expected_revision,
                        after_id: args.after_id,
                        before_id: args.before_id,
                        destination,
                        context: mode,
                        execution: args.execution,
                        follow_up,
                    },
                )
                .await?;
            tool_output(&receipt)
        })
    }

    fn board_tool_start_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        context: BoardRuntimeContext,
        tool: &'a str,
        _project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: StartArgs = parse_args(arguments)?;
            claimed_self(&context, "callerTaskId", args.caller_task_id.as_deref())?;
            let destination = bound_destination(&context, args.destination)?;
            let mode = bound_context(&context, tool, args.context, args.checkpoint_message_id)?;
            let follow_up = bound_follow_up(&context, args.follow_up)?;
            match (args.work_item_id, args.work_item_ids) {
                (Some(work_item_id), None) => {
                    let receipt = self
                        .board_start(
                            caller,
                            BoardStartRequest {
                                request_id: args.request_id,
                                work_item_id,
                                expected_revision: args.expected_revision,
                                destination,
                                context: mode,
                                execution: args.execution,
                                follow_up,
                                caller_task_id: context.task_id.clone(),
                            },
                        )
                        .await?;
                    tool_output(&receipt)
                }
                (None, Some(work_item_ids)) => {
                    if work_item_ids.is_empty() || work_item_ids.len() > BOARD_BATCH_START_MAX {
                        return Err(BoardError::invalid(
                            "workItemIds",
                            format!("name 1-{BOARD_BATCH_START_MAX} tickets"),
                        ));
                    }
                    if args.expected_revision.is_some() {
                        return Err(BoardError::invalid(
                            "expectedRevision",
                            "applies to one ticket; omit it with workItemIds",
                        ));
                    }
                    let receipt = self
                        .board_start_batch(
                            caller,
                            BoardStartBatchRequest {
                                request_id: args.request_id,
                                work_item_ids,
                                destination,
                                context: mode,
                                execution: args.execution,
                                follow_up,
                                caller_task_id: context.task_id.clone(),
                            },
                        )
                        .await?;
                    tool_output(&receipt)
                }
                _ => Err(BoardError::invalid(
                    "workItemId",
                    format!(
                        "pass workItemId for one ticket or workItemIds for up to {BOARD_BATCH_START_MAX}"
                    ),
                )),
            }
        })
    }

    fn board_tool_control_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        context: BoardRuntimeContext,
        _tool: &'a str,
        _project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: ControlArgs = parse_args(arguments)?;
            claimed_self(&context, "callerTaskId", args.caller_task_id.as_deref())?;
            if args.adopt_latest_brief && args.action != ControlArg::Retry {
                return Err(BoardError::invalid(
                    "adoptLatestBrief",
                    "applies only to retry",
                ));
            }
            let revision = |action: &str| {
                args.expected_revision.ok_or_else(|| {
                    BoardError::invalid(
                        "expectedRevision",
                        format!("{action} needs the ticket revision you read"),
                    )
                })
            };
            let action = match args.action {
                ControlArg::Delete => {
                    let expected_revision = revision("delete")?;
                    return tool_output(
                        &self
                            .board_delete(
                                caller,
                                luna_proto::BoardDeleteRequest {
                                    request_id: args.request_id,
                                    work_item_id: args.work_item_id,
                                    expected_revision,
                                },
                            )
                            .await?,
                    );
                }
                ControlArg::Archive | ControlArg::Unarchive => {
                    let receipt = self
                        .board_archive(
                            caller,
                            BoardArchiveRequest {
                                request_id: args.request_id,
                                work_item_id: args.work_item_id,
                                archived: args.action == ControlArg::Archive,
                                expected_revision: args.expected_revision,
                            },
                        )
                        .await?;
                    return tool_output(&receipt);
                }
                ControlArg::SendUpdate => {
                    let receipt = self
                        .board_send_update(
                            caller,
                            BoardSendUpdateRequest {
                                expected_revision: revision("sendUpdate")?,
                                request_id: args.request_id,
                                work_item_id: args.work_item_id,
                                mode: args.mode.unwrap_or_default(),
                            },
                        )
                        .await?;
                    return tool_output(&receipt);
                }
                ControlArg::Link | ControlArg::Unlink => {
                    let role = args.role.ok_or_else(|| {
                        BoardError::invalid("role", "name source, reference or attempt")
                    })?;
                    if !matches!(
                        role,
                        TaskLinkRole::Source | TaskLinkRole::Reference | TaskLinkRole::Attempt
                    ) {
                        return Err(BoardError::invalid(
                            "role",
                            "working chats come only from Start or conversion and reviewers only from review runs; link a source, reference or attempt",
                        ));
                    }
                    let task_id = args
                        .task_id
                        .clone()
                        .ok_or_else(|| BoardError::invalid("taskId", "name the chat to link"))?;
                    let receipt = self
                        .board_link_chat(
                            caller,
                            BoardLinkChatRequest {
                                expected_revision: revision("link")?,
                                request_id: args.request_id,
                                work_item_id: args.work_item_id,
                                task_id,
                                role,
                                linked: args.action == ControlArg::Link,
                            },
                        )
                        .await?;
                    return tool_output(&receipt);
                }
                ControlArg::Block | ControlArg::Unblock => {
                    let blocked_by_id = args.blocked_by_id.clone().ok_or_else(|| {
                        BoardError::invalid("blockedById", "name the ticket this one waits for")
                    })?;
                    let receipt = self
                        .board_set_dependency(
                            caller,
                            BoardSetDependencyRequest {
                                request_id: args.request_id,
                                work_item_id: args.work_item_id,
                                blocked_by_id,
                                present: args.action == ControlArg::Block,
                            },
                        )
                        .await?;
                    return tool_output(&receipt);
                }
                ControlArg::Accept => {
                    return Err(BoardError::InvalidRole {
                            operation: "board_control.accept".into(),
                            reason: "Accepting a result under manual acceptance is the user's decision on the Board; ask the user to accept it.".into(),
                            allowed: ["stop", "continue", "retry", "cancelQueued", "archive", "unarchive"]
                                .map(str::to_owned)
                                .to_vec(),
                        });
                }
                ControlArg::Stop => BoardControlAction::Stop,
                ControlArg::Continue => BoardControlAction::Continue,
                ControlArg::Retry => BoardControlAction::Retry,
                ControlArg::CancelQueued => BoardControlAction::CancelQueued,
            };
            let receipt = self
                .board_control(
                    caller,
                    BoardControlRequest {
                        request_id: args.request_id,
                        work_item_id: args.work_item_id,
                        action,
                        expected_revision: args.expected_revision,
                        adopt_latest_brief: args.adopt_latest_brief,
                    },
                )
                .await?;
            tool_output(&receipt)
        })
    }

    fn board_tool_set_status_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        context: BoardRuntimeContext,
        _tool: &'a str,
        _project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: StatusArgs = parse_args(arguments)?;
            claimed_self(&context, "callerTaskId", args.caller_task_id.as_deref())?;
            let summary = args.summary.trim();
            if summary.is_empty() || summary.chars().count() > STATUS_SUMMARY_CHARS {
                return Err(BoardError::invalid(
                    "summary",
                    format!("write 1-{STATUS_SUMMARY_CHARS} characters"),
                ));
            }
            if args.evidence.len() > STATUS_EVIDENCE_MAX {
                return Err(BoardError::invalid(
                    "evidence",
                    format!("at most {STATUS_EVIDENCE_MAX} references"),
                ));
            }
            let receipt = self
                .board_set_status(
                    caller,
                    BoardSetStatusRequest {
                        request_id: args.request_id,
                        work_item_id: args.work_item_id,
                        expected_revision: args.expected_revision,
                        execution_generation: args.execution_generation,
                        report: BoardStatusReport {
                            kind: args.kind,
                            summary: summary.into(),
                            evidence: args.evidence,
                        },
                    },
                )
                .await?;
            tool_output(&receipt)
        })
    }

    fn board_tool_convert_chat_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        context: BoardRuntimeContext,
        _tool: &'a str,
        _project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: ConvertArgs = parse_args(arguments)?;
            claimed_self(&context, "taskId", args.task_id.as_deref())?;
            claimed_self(&context, "callerTaskId", args.caller_task_id.as_deref())?;
            let task_id = context
                .task_id
                .clone()
                .ok_or_else(|| BoardError::NotConvertible {
                    reason: "Only an ordinary chat can become a ticket's working chat.".into(),
                })?;
            let receipt = self
                .board_convert_chat(
                    caller,
                    BoardConvertChatRequest {
                        request_id: args.request_id,
                        task_id,
                        title: args.title,
                        description: args.description,
                        implementation: args.implementation,
                        review: args.review,
                        completion: args.completion,
                    },
                )
                .await?;
            tool_output(&receipt)
        })
    }

    fn board_tool_review_start_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        context: BoardRuntimeContext,
        _tool: &'a str,
        _project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: ReviewStartArgs = parse_args(arguments)?;
            // A worker submits its own ticket by default; A5 authorizes
            // the working chat and ties checks to the captured snapshot.
            let work_item_id = args
                .work_item_id
                .or_else(|| {
                    context
                        .ticket
                        .as_ref()
                        .map(|ticket| ticket.work_item_id.clone())
                })
                .ok_or_else(|| BoardError::invalid("workItemId", "name the ticket to review"))?;
            if args.checks.len() > REVIEW_REPORTS_MAX || args.evidence.len() > REVIEW_REPORTS_MAX {
                return Err(BoardError::invalid(
                    "checks",
                    format!(
                        "report at most {REVIEW_REPORTS_MAX} checks and {REVIEW_REPORTS_MAX} evidence references"
                    ),
                ));
            }
            if args
                .summary
                .as_ref()
                .is_some_and(|summary| summary.chars().count() > STATUS_SUMMARY_CHARS)
            {
                return Err(BoardError::invalid(
                    "summary",
                    format!("use at most {STATUS_SUMMARY_CHARS} characters"),
                ));
            }
            let receipt = self
                .board_review_start(
                    caller,
                    BoardReviewStartRequest {
                        request_id: args.request_id,
                        work_item_id,
                        execution_generation: args.execution_generation,
                        profile: args.profile,
                        checks: args.checks,
                        summary: args.summary,
                        evidence: args.evidence,
                    },
                )
                .await?;
            tool_output(&receipt)
        })
    }

    fn board_tool_review_status_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        context: BoardRuntimeContext,
        _tool: &'a str,
        _project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: ReviewStatusArgs = parse_args(arguments)?;
            let own_run = context
                .ticket
                .as_ref()
                .and_then(|ticket| ticket.review_run_id.clone());
            let (work_item_id, review_run_id) = match (args.work_item_id, args.review_run_id) {
                (None, None) if context.audience == BoardToolAudience::Reviewer => (None, own_run),
                (None, None) => {
                    return Err(BoardError::invalid(
                        "workItemId",
                        "name a ticket (workItemId) or a review run (reviewRunId)",
                    ));
                }
                other => other,
            };
            let status = self.board_review_status(
                caller,
                BoardReviewStatusRequest {
                    work_item_id,
                    review_run_id,
                },
            )?;
            tool_output(&status)
        })
    }

    fn board_tool_review_cancel_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        _context: BoardRuntimeContext,
        _tool: &'a str,
        _project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: ReviewCancelArgs = parse_args(arguments)?;
            let receipt = self
                .board_review_cancel(
                    caller,
                    BoardReviewCancelRequest {
                        request_id: args.request_id,
                        review_run_id: args.review_run_id,
                    },
                )
                .await?;
            tool_output(&receipt)
        })
    }

    fn board_tool_review_submit_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        context: BoardRuntimeContext,
        _tool: &'a str,
        _project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: ReviewSubmitArgs = parse_args(arguments)?;
            // The run defaults to this reviewer's recorded run; A5 then
            // authorizes by that record (only its exact reviewer chat).
            let review_run_id = args
                .review_run_id
                .clone()
                .or_else(|| {
                    context
                        .ticket
                        .as_ref()
                        .and_then(|ticket| ticket.review_run_id.clone())
                })
                .ok_or_else(|| BoardError::NotAuthorized {
                    reason: "This chat has no recorded review run to submit for.".into(),
                })?;
            if args.summary.trim().is_empty() || args.summary.chars().count() > REVIEW_SUMMARY_CHARS
            {
                return Err(BoardError::invalid(
                    "summary",
                    format!("write 1-{REVIEW_SUMMARY_CHARS} characters"),
                ));
            }
            if args.findings.len() > REVIEW_FINDINGS_MAX {
                return Err(BoardError::invalid(
                    "findings",
                    format!("at most {REVIEW_FINDINGS_MAX} findings"),
                ));
            }
            let receipt = self
                .board_review_submit(
                    caller,
                    BoardReviewSubmitRequest {
                        request_id: args.request_id,
                        review_run_id,
                        verdict: args.verdict,
                        basis_packet_hash: args.basis_packet_hash,
                        findings: args.findings,
                        summary: args.summary,
                    },
                )
                .await?;
            tool_output(&receipt)
        })
    }

    fn board_tool_create_side_chat_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        context: BoardRuntimeContext,
        tool: &'a str,
        _project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: SideChatArgs = parse_args(arguments)?;
            claimed_self(&context, "callerTaskId", args.caller_task_id.as_deref())?;
            let caller_task_id =
                context
                    .task_id
                    .clone()
                    .ok_or_else(|| BoardError::InvalidRole {
                        operation: tool.into(),
                        reason: "Only chats create side chats.".into(),
                        allowed: Vec::new(),
                    })?;
            let mode = bound_context(&context, tool, args.context, args.checkpoint_message_id)?;
            if args
                .prompt
                .as_ref()
                .is_some_and(|prompt| prompt.chars().count() > PROMPT_CHARS)
            {
                return Err(BoardError::invalid(
                    "prompt",
                    format!("use at most {PROMPT_CHARS} characters"),
                ));
            }
            let created = self
                .board_create_side_chat(
                    caller,
                    BoardCreateSideChatRequest {
                        request_id: args.request_id,
                        caller_task_id,
                        context: mode,
                        title: args.title,
                        prompt: args.prompt,
                        start: args.start,
                        ticket: args.ticket.map(Into::into),
                        execution: args.execution,
                    },
                )
                .await?;
            tool_output(&created)
        })
    }

    fn board_tool_create_main_chat_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        context: BoardRuntimeContext,
        _tool: &'a str,
        project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: MainChatArgs = parse_args(arguments)?;
            claimed_self(&context, "fromTaskId", args.from_task_id.as_deref())?;
            claimed_self(&context, "callerTaskId", args.caller_task_id.as_deref())?;
            let brief = args
                .handoff_brief
                .as_deref()
                .map(str::trim)
                .filter(|brief| !brief.is_empty());
            if context.role == Some(BoardChatRole::Side) && brief.is_none() {
                return Err(BoardError::invalid(
                    "handoffBrief",
                    "a side chat hands work to a new main chat with a written brief: goal, completed work, relevant files, decisions, constraints and the next step",
                ));
            }
            if brief.is_some_and(|brief| brief.chars().count() > PROMPT_CHARS) {
                return Err(BoardError::invalid(
                    "handoffBrief",
                    format!("use at most {PROMPT_CHARS} characters"),
                ));
            }
            let created = self
                .board_create_main_chat(
                    caller,
                    BoardCreateMainChatRequest {
                        request_id: args.request_id,
                        project_id: args.project_id.unwrap_or(project),
                        from_task_id: context.task_id.clone(),
                        handoff_brief: brief.map(str::to_owned),
                        title: args.title,
                        start: args.start,
                        ticket: args.ticket.map(Into::into),
                        execution: args.execution,
                    },
                )
                .await?;
            tool_output(&created)
        })
    }

    fn board_tool_authoring_checkpoint_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        context: BoardRuntimeContext,
        tool: &'a str,
        _project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: CheckpointArgs = parse_args(arguments)?;
            let session_id =
                context
                    .authoring_session_id
                    .clone()
                    .ok_or_else(|| BoardError::InvalidRole {
                        operation: tool.into(),
                        reason: "Only a ticket discussion keeps a draft checkpoint.".into(),
                        allowed: Vec::new(),
                    })?;
            if args
                .session_id
                .as_ref()
                .is_some_and(|given| given != &session_id)
            {
                return Err(BoardError::NotAuthorized {
                    reason: format!("This discussion can only checkpoint itself ({session_id})."),
                });
            }
            let session = self
                .board_authoring_checkpoint(
                    caller,
                    BoardAuthoringCheckpointRequest {
                        session_id,
                        expected_version: args.expected_version,
                        proposal: args.proposal,
                    },
                )
                .await?;
            // The private runtime binding never leaves the engine.
            let mut session = session;
            session.runtime_binding = None;
            tool_output(&session)
        })
    }

    fn board_tool_issue_search_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        _context: BoardRuntimeContext,
        _tool: &'a str,
        project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: IssueSearchArgs = parse_args(arguments)?;
            let project_id = args.project_id.unwrap_or(project);
            let Some(source_id) = args.source_id else {
                let sources = self
                    .board_issue_sources(caller, BoardIssueSourcesRequest { project_id })
                    .await?;
                let mut value = tool_output(&sources)?;
                if let Some(object) = value.as_object_mut() {
                    object.insert(
                        "next".into(),
                        json!("Pass one sources[].id as sourceId to search its issues."),
                    );
                }
                return Ok(value);
            };
            let limit = bounded_limit("limit", args.limit, ISSUES_MAX, ISSUES_MAX)?;
            let page = self
                .board_issue_search(
                    caller,
                    BoardIssueSearchRequest {
                        project_id,
                        source_id,
                        query: args.query,
                        state: args.state,
                        labels: args.labels,
                        assignee: args.assignee,
                        cursor: args.cursor,
                        limit: Some(limit),
                        refresh: args.refresh,
                    },
                )
                .await?;
            Ok(untrusted(tool_output(&page)?))
        })
    }

    fn board_tool_issue_import_call<'a>(
        &'a self,
        caller: &'a BoardCaller,
        context: BoardRuntimeContext,
        _tool: &'a str,
        project: String,
        arguments: Value,
    ) -> BoardToolFuture<'a> {
        Box::pin(async move {
            let args: IssueImportArgs = parse_args(arguments)?;
            let start = match args.start {
                Some(start) => Some(BoardImportStart {
                    destination: bound_destination(&context, start.destination)?,
                    execution: start.execution,
                }),
                None => None,
            };
            let receipt = self
                .board_issue_import(
                    caller,
                    BoardIssueImportRequest {
                        request_id: args.request_id,
                        project_id: args.project_id.unwrap_or(project),
                        source_id: args.source_id,
                        number: args.number,
                        additions: args.additions,
                        comment_ids: args.comment_ids,
                        start,
                        // Agents import without a UI preview step.
                        previewed_updated_at: None,
                    },
                )
                .await?;
            Ok(untrusted(tool_output(&receipt)?))
        })
    }

    /// Diagnostics for `BoardToolCatalog`: the chat's current board context,
    /// the board tools its next run attaches on the current provider
    /// delivery, and the board tools its last acknowledged provider session
    /// actually received (with that session's delivery path).
    pub(crate) fn board_tool_catalog(&self, task_id: &str) -> BoardResult<Value> {
        let context = self.board_runtime_context(task_id)?;
        let chat = self.workspace.chat(task_id).map_err(storage)?;
        let binding = self
            .task_identity
            .view(task_id)
            .and_then(|view| view.current_binding);
        let provider = chat
            .as_ref()
            .and_then(|chat| chat.config.as_ref().map(|config| config.harness))
            .or_else(|| binding.as_ref().map(|binding| binding.provider_id));
        let delivery = provider
            .and_then(|provider| self.registry.resolve(provider).ok())
            .map(|adapter| adapter.luna_runtime_delivery())
            .unwrap_or(luna_proto::RuntimeDelivery::Unsupported);
        let session_id = binding
            .map(|binding| binding.provider_task_id)
            .filter(|id| !id.is_empty())
            .or_else(|| {
                chat.as_ref()
                    .and_then(|chat| chat.harness_session_id.clone())
            });
        let acknowledged = session_id
            .as_deref()
            .map(|id| self.sessions.runtime_contract_diagnostics(task_id, id))
            .transpose()
            .map_err(storage)?
            .flatten();
        let next_run: Vec<String> = match (&context, delivery.attaches_luna_tools()) {
            (Some(context), true) => board_tool_names(context),
            _ => Vec::new(),
        };
        let acknowledged = acknowledged.map(|diagnostics| {
            let attached: Vec<String> = diagnostics
                .attached_tools
                .iter()
                .filter(|tool| BOARD_TOOL_NAMES.contains(&tool.name.as_str()))
                .map(|tool| tool.name.clone())
                .collect();
            json!({
                "delivery": diagnostics.delivery,
                "contractHash": diagnostics.contract_hash,
                "acceptedAtMs": diagnostics.accepted_at_ms,
                "verified": diagnostics.delivery.serves_luna_mcp() && !attached.is_empty(),
                "boardTools": attached,
            })
        });
        let mut value = json!({
            "taskId": task_id,
            "available": context.is_some(),
            "provider": provider,
            "providerDelivery": delivery,
            "context": context,
            "nextRunTools": next_run,
            "acknowledged": acknowledged,
        });
        if context.is_none() {
            value["reason"] = json!("This chat has no project board on this host.");
        } else if !delivery.attaches_luna_tools() {
            value["reason"] = json!(
                "This provider has no verified LUNA tool delivery; board tools are not attached."
            );
        }
        prune_nulls(&mut value);
        Ok(value)
    }
}
