//! Data types: stored records (ch.3, 5.1) and API payloads (ch.2). JSON is camelCase.
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------- scope / source

/// Sharing scope of a memory (2.2): `global`, or bound to a project / platform.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemScope {
    /// "global" | "project" | "platform"
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

impl MemScope {
    pub fn global() -> Self {
        Self {
            kind: "global".into(),
            project: None,
            platform: None,
        }
    }
    pub fn project(p: &str) -> Self {
        Self {
            kind: "project".into(),
            project: Some(p.to_string()),
            platform: None,
        }
    }
}

/// Caller-side scope used in requests: `{project, platform, session}`; "*" = unrestricted.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReqScope {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    /// "project" (the default) | "global". `"global"` is a session with no project folder: it names no `project`,
    /// its writes are global memories and its reads see global and platform memories only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl ReqScope {
    /// Whether the request is for a session with no project folder (`kind: "global"`). Every entry point asks
    /// this once, so `kind` is checked in one place: absent or `"project"` leaves `project` as the caller wrote
    /// it, `"global"` must come without one.
    pub fn is_global(&self) -> ApiResult<bool> {
        match (self.kind.as_deref(), &self.project) {
            (None | Some("project"), _) => Ok(false),
            (Some("global"), None) => Ok(true),
            (Some("global"), Some(_)) => Err(ApiError::InvalidArgument(
                "scope.project and scope.kind \"global\" exclude each other".into(),
            )),
            (Some(other), _) => Err(ApiError::InvalidArgument(format!(
                "scope.kind must be project or global, not {other:?}"
            ))),
        }
    }
}

pub const ORIGIN_EXPLICIT: &str = "explicit";
pub const ORIGIN_USER: &str = "user";
pub const ORIGIN_ASSISTANT: &str = "assistant";
pub const ORIGIN_INFERRED: &str = "inferred";
/// Copied from a host's own memory by `agent-memoryd import` (docs/design/memory-import.md). Never
/// presented as established by default, and a consolidation candidate like inferred facts.
pub const ORIGIN_IMPORTED: &str = "imported";

/// Durable attribution attached to a stored memory.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub platform: String,
    #[serde(default)]
    pub turn_ids: Vec<String>,
    pub origin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelEdge {
    pub id: String,
    /// "conflicts" | "related" | (for experiences) "same"
    pub relation: String,
}

// ---------------------------------------------------------------- memories (2.2 + 5.1)

pub const T_FACT: &str = "fact";
pub const T_EXPERIENCE: &str = "experience";
pub const T_PROFILE: &str = "profile";
pub const T_SKILL: &str = "skill";
pub const T_INSTRUCTION: &str = "instruction";

pub const S_CURRENT: &str = "current";
pub const S_SUPERSEDED: &str = "superseded";
pub const S_CONFLICT: &str = "conflict";
pub const S_CANDIDATE: &str = "candidate";
pub const S_STALE: &str = "stale";

/// The application builds lineage; the model never assigns provenance or dependency IDs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Derivation {
    pub stage: String,
    pub input_memory_ids: Vec<String>,
    pub input_turn_ids: Vec<String>,
    /// Current views invalidate when inputs change; historical observations retain their evidence.
    pub current_view: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UncertaintyRec {
    pub id: String,
    pub memory_ids: Vec<String>,
    pub scope: MemScope,
    pub reason: String,
    pub status: String,
    pub created_at: String,
}

/// Statuses that are "active" (indexed, recallable): current, conflict, candidate (5.1).
pub fn is_active(status: &str) -> bool {
    matches!(status, S_CURRENT | S_CONFLICT | S_CANDIDATE)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRec {
    pub id: String,
    #[serde(rename = "type")]
    pub mtype: String,
    pub text: String,
    pub scope: MemScope,
    pub status: String,
    /// YYYY-MM-DD or RFC3339
    pub effective_at: String,
    pub recorded_at: String,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    #[serde(default)]
    pub related: Vec<RelEdge>,
    // ---- internal (5.1) ----
    #[serde(default)]
    pub entity_ids: Vec<String>,
    /// 说出时间: time of the turn that the supporting span came from (RFC3339)
    #[serde(default)]
    pub stated_at: String,
    /// position of the span in its turn (tie-break within one turn); user message spans < assistant spans
    #[serde(default)]
    pub stated_pos: i64,
    #[serde(default)]
    pub detail: Option<Value>,
    #[serde(default = "one")]
    pub evidence_count: i64,
    #[serde(default)]
    pub last_accessed_at: String,
    /// Validity end for superseded values: next value's effectiveAt (5.3 step 4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
    /// For profiles: "user" | "project"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_kind: Option<String>,
    /// For profiles/skills: memory ids the text was built from
    #[serde(default)]
    pub used_memory_ids: Vec<String>,
    /// For candidate skills: helpful feedback count (启用 at 3)
    #[serde(default)]
    pub helpful_count: i64,
    /// Proposal payload for merge/update proposals (4.5) — kept for the management UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation: Option<Derivation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
}

fn one() -> i64 {
    1
}

impl MemoryRec {
    /// Fields returned to callers (2.2) — strips internal fields.
    pub fn public(&self) -> Value {
        serde_json::json!({
            "id": self.id,
            "type": self.mtype,
            "text": self.text,
            "scope": self.scope,
            "status": self.status,
            "effectiveAt": self.effective_at,
            "recordedAt": self.recorded_at,
            "source": self.source,
            "supersedes": self.supersedes,
            "supersededBy": self.superseded_by,
            "related": self.related,
            "validUntil": self.valid_until,
            "detail": self.detail,
            "derivation": self.derivation,
            "staleReason": self.stale_reason,
        })
    }

    /// [`Self::public`] plus `statedAt`: the record views of `get_memory` and `list_memories`. Search results
    /// and their token budgets keep [`Self::public`].
    pub fn public_with_stated_at(&self) -> Value {
        let mut v = self.public();
        v["statedAt"] = Value::String(self.stated_at.clone());
        v
    }
}

// ---------------------------------------------------------------- turns / evidence

/// A `turns` row. Only the retired memos strategy wrote them; they stay readable.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TurnRec {
    /// storage key: "{platform}::{turnId}"
    pub key: String,
    pub turn_id: String,
    pub platform: String,
    pub project: String,
    pub session: String,
    pub user_message: String,
    pub assistant_message: String,
    pub user_ts: String,
    pub assistant_ts: String,
    pub started_at: String,
    pub ended_at: String,
    pub recorded_at: String,
    #[serde(default)]
    pub extracted_at: Option<String>,
    pub digest: String,
    /// texts of facts derived from this turn (3.4)
    #[serde(default)]
    pub derived_text: String,
    #[serde(default)]
    pub derived_memory_ids: Vec<String>,
    /// arrival order within the session
    pub seq: i64,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub extract_failures: i64,
    /// "pending" | "done" | "failed" (5 failures → 待处理)
    #[serde(default)]
    pub extract_state: String,
}

pub fn turn_key(platform: &str, turn_id: &str) -> String {
    format!("{platform}::{turn_id}")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceRec {
    pub id: String,
    pub memory_ids: Vec<String>,
    pub platform: String,
    pub turn_id: String,
    /// "user" | "assistant" | "text" | "correction"
    pub role: String,
    pub span: String,
    pub stated_at: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<EvidenceContext>,
}

/// A verbatim question paired with a first-hand reply, not an additional assertion.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceContext {
    pub span: String,
    pub stated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RequestRec {
    pub request_id: String,
    /// queued | running | completed | failed
    pub status: String,
    #[serde(default)]
    pub memory_ids: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub turn_keys: Vec<String>,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

// ---------------------------------------------------------------- API payloads (ch.2)

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InMessage {
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AddMemoriesReq {
    /// Mem0 strategy only: false stores each supplied message verbatim without generation.
    #[serde(default)]
    pub infer: Option<bool>,
    /// Explicit text may be stored as an instruction without a generative-model call.
    #[serde(default)]
    pub memory_type: Option<String>,
    #[serde(default)]
    pub memory_scope: Option<MemScope>,
    #[serde(default)]
    pub scope: ReqScope,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub messages: Option<Vec<InMessage>>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub session_ended: Option<bool>,
    #[serde(default)]
    pub sync: Option<bool>,
    /// Set by `agent-memoryd import apply` on a past session turn it queues: the import run the turn
    /// belongs to (docs/design/memory-import.md §6.10). Accepted only for a turn that run applied.
    #[serde(default)]
    pub import_run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchReq {
    /// Mem0 strategy: explicit opt-in to the configured local cross-encoder.
    #[serde(default)]
    pub rerank: Option<bool>,
    pub query: String,
    #[serde(default)]
    pub scope: Option<ReqScope>,
    #[serde(default)]
    pub types: Option<Vec<String>>,
    #[serde(default)]
    pub top_k: Option<usize>,
    #[serde(default)]
    pub include_recent: Option<bool>,
    #[serde(default)]
    pub include_history: Option<bool>,
    #[serde(default)]
    pub include_evidence: Option<bool>,
    #[serde(default)]
    pub token_budget: Option<usize>,
    /// eval-only: reference "now" (RFC3339)
    #[serde(default)]
    pub as_of: Option<String>,
    /// eval-only: false = do not update lastAccessedAt
    #[serde(default)]
    pub touch: Option<bool>,
    /// eval-only: add every pool candidate's signals and the query's score statistics as `explain`
    #[serde(default)]
    pub explain: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GetContextReq {
    #[serde(default)]
    pub scope: ReqScope,
    #[serde(default)]
    pub token_budget: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListTurnsReq {
    #[serde(default)]
    pub scope: ReqScope,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub token_budget: Option<usize>,
    #[serde(default)]
    pub turn_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GetMemoryReq {
    pub id: String,
    #[serde(default)]
    pub include_history: Option<bool>,
    #[serde(default)]
    pub include_evidence: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CorrectMemoryReq {
    pub id: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub effective_at: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct IdReq {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveUncertaintyReq {
    pub id: String,
    pub keep_memory_id: String,
    #[serde(default)]
    pub reason: String,
}

/// `POST /admin/consolidate`: propose consolidation ops for one project's facts.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConsolidateReq {
    pub project: String,
    /// Must be true: this action only proposes; `consolidation_apply` applies a run.
    #[serde(default)]
    pub dry_run: Option<bool>,
}

/// `POST /admin/consolidation_apply`, `/admin/consolidation_rollback` and `/admin/consolidation_run`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConsolidationRunReq {
    pub run_id: String,
}

/// `POST /admin/list_memories`: one page of stored records, newest recorded first. Every filter is optional.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListMemoriesReq {
    /// `scope.project` equals
    #[serde(default)]
    pub project: Option<String>,
    /// `scope.kind` equals: "global" | "project" | "platform"
    #[serde(default)]
    pub scope_kind: Option<String>,
    /// `source.platform` equals (the client that wrote the record)
    #[serde(default)]
    pub source_platform: Option<String>,
    #[serde(default)]
    pub types: Option<Vec<String>>,
    /// Default: the active statuses (current, conflict, candidate).
    #[serde(default)]
    pub statuses: Option<Vec<String>>,
    /// `source.origin` in
    #[serde(default)]
    pub origins: Option<Vec<String>>,
    /// Case-insensitive substring of `text`.
    #[serde(default)]
    pub text: Option<String>,
    /// 1-3650: records recorded more than this many days ago that no search or context read returned in the
    /// last that many days. Not together with `neverRecalled`.
    #[serde(default)]
    pub not_recalled_days: Option<u32>,
    /// Records no search or context read ever returned.
    #[serde(default)]
    pub never_recalled: bool,
    /// `nextCursor` of the previous page.
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `POST /admin/memory_lineage`: the records linked to one record, up to `depth` links away.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MemoryLineageReq {
    pub id: String,
    /// 1-6; default 3.
    #[serde(default)]
    pub depth: Option<usize>,
}

/// `POST /admin/list_entities`: one page of the algorithm's entities, most linked first. Every filter is optional.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListEntitiesReq {
    /// `scope.project` equals
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub entity_type: Option<String>,
    /// Case-insensitive substring of the entity's text.
    #[serde(default)]
    pub text: Option<String>,
    /// `nextCursor` of the previous page.
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `POST /admin/consolidation_runs`: the newest runs first, optionally of one project.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConsolidationRunsReq {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `POST /admin/list_uncertainties`: the newest questions of one status first. No cursor: the list is
/// truncated at `limit`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListUncertaintiesReq {
    /// "open" (the default) | "resolved" | "withdrawn"
    #[serde(default)]
    pub status: Option<String>,
    /// `scope.project` equals
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `POST /admin/list_retrievals`: one page of `retrieval_log`, newest first. Every filter is optional.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListRetrievalsReq {
    /// "search" | "context"
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    /// Leaves out rows of these platforms; rows without a platform are kept.
    #[serde(default)]
    pub exclude_platforms: Option<Vec<String>>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    /// Rows that returned this memory, logged in the last 30 days.
    #[serde(default)]
    pub memory_id: Option<String>,
    /// `nextCursor` of the previous page.
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `POST /admin/list_requests`: one page of write requests, newest first. Every filter is optional.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListRequestsReq {
    #[serde(default)]
    pub statuses: Option<Vec<String>>,
    /// "mem0" | "instruction"
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    /// `nextCursor` of the previous page.
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `POST /admin/list_model_responses`: one page of raw generation responses, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListModelResponsesReq {
    #[serde(default)]
    pub stage: Option<String>,
    /// Only calls that failed: a transport error, an HTTP status outside 2xx or an output of the wrong shape.
    #[serde(default)]
    pub failed_only: bool,
    /// `nextCursor` of the previous page.
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `get_task`, and `POST /admin/request`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GetTaskReq {
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackReq {
    pub retrieval_id: String,
    #[serde(default)]
    pub used: Vec<String>,
    pub outcome: String,
}

/// Connection identity filled by the access layer (2.2: platform comes from the connection).
#[derive(Debug, Clone)]
pub struct Caller {
    pub platform: String,
    pub client: ClientKind,
}

/// Which program of a platform made a call (`X-Memory-Client`): its hooks, including the uploader that sends
/// what they captured; its MCP bridge; or anything else on the HTTP interface, which sends no header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientKind {
    Hook,
    Mcp,
    Http,
}

impl ClientKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hook => "hook",
            Self::Mcp => "mcp",
            Self::Http => "http",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::Hook, Self::Mcp, Self::Http]
            .into_iter()
            .find(|kind| kind.as_str() == s)
    }
}

// ---------------------------------------------------------------- errors

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    InvalidArgument(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    #[error("memory is not current; current id = {0}")]
    NotCurrent(String),
    #[error("{0}")]
    PayloadTooLarge(String),
    #[error("{0}")]
    ModelUnavailable(String),
    #[error("{0}")]
    Internal(String),
}

impl ApiError {
    pub fn status_code(&self) -> u16 {
        match self {
            ApiError::InvalidArgument(_) => 400,
            ApiError::Forbidden(_) => 403,
            ApiError::NotFound(_) => 404,
            ApiError::NotCurrent(_) => 409,
            ApiError::PayloadTooLarge(_) => 413,
            ApiError::ModelUnavailable(_) => 503,
            ApiError::Internal(_) => 500,
        }
    }
    pub fn code(&self) -> &'static str {
        match self {
            ApiError::InvalidArgument(_) => "INVALID_ARGUMENT",
            ApiError::Forbidden(_) => "FORBIDDEN",
            ApiError::NotFound(_) => "NOT_FOUND",
            ApiError::NotCurrent(_) => "NOT_CURRENT",
            ApiError::PayloadTooLarge(_) => "PAYLOAD_TOO_LARGE",
            ApiError::ModelUnavailable(_) => "MODEL_UNAVAILABLE",
            ApiError::Internal(_) => "INTERNAL",
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(format!("{e:#}"))
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;
