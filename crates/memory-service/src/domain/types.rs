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
}

pub const ORIGIN_EXPLICIT: &str = "explicit";
pub const ORIGIN_USER: &str = "user";
pub const ORIGIN_ASSISTANT: &str = "assistant";
pub const ORIGIN_INFERRED: &str = "inferred";

/// 来源优先级: 用户明确要求 > 用户原话 > 助手回复 > 智能体推断 (4.3).
pub fn origin_priority(o: &str) -> i32 {
    match o {
        ORIGIN_EXPLICIT => 3,
        ORIGIN_USER => 2,
        ORIGIN_ASSISTANT => 1,
        _ => 0,
    }
}

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
}

// ---------------------------------------------------------------- turns / sessions / evidence

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
    /// texts of facts derived from this turn (merged into the full-text key after extraction, 3.4)
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
    /// Windows of messages longer than their digest share, each indexed with its own vector.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chunks: Vec<TurnChunk>,
}

/// A byte range of one stored message.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnChunk {
    /// "user" | "assistant"
    pub role: String,
    pub start: usize,
    pub end: usize,
}

impl TurnRec {
    pub fn full_text(&self) -> String {
        let mut s = format!("{}\n{}", self.user_message, self.assistant_message);
        if !self.derived_text.is_empty() {
            s.push('\n');
            s.push_str(&self.derived_text);
        }
        s
    }

    /// Byte shares of the user and assistant heads; together with the separator they fill `digest_max`.
    pub fn digest_limits(digest_max: usize) -> (usize, usize) {
        let half = digest_max / 2;
        (half.saturating_sub(1), half)
    }

    /// The digest (both message heads) and the chunk windows of every message longer than its share.
    pub fn views(user: &str, assistant: &str, digest_max: usize) -> (String, Vec<TurnChunk>) {
        let (user_max, assistant_max) = Self::digest_limits(digest_max);
        let digest = format!(
            "{}\n{}",
            memory_rules::util::truncate_bytes(user, user_max),
            memory_rules::util::truncate_bytes(assistant, assistant_max)
        );
        let mut chunks = Vec::new();
        for (role, text, max) in [
            ("user", user, user_max),
            ("assistant", assistant, assistant_max),
        ] {
            if text.len() > max {
                chunks.extend(
                    memory_rules::util::text_chunks(text, assistant_max)
                        .into_iter()
                        .map(|(start, end)| TurnChunk {
                            role: role.into(),
                            start,
                            end,
                        }),
                );
            }
        }
        (digest, chunks)
    }

    pub fn message(&self, role: &str) -> &str {
        if role == "user" {
            &self.user_message
        } else {
            &self.assistant_message
        }
    }

    pub fn chunk_text(&self, i: usize) -> Option<&str> {
        let c = self.chunks.get(i)?;
        self.message(&c.role).get(c.start..c.end)
    }

    /// Whether the digest holds both messages in full.
    pub fn digest_complete(&self, digest_max: usize) -> bool {
        let (user_max, assistant_max) = Self::digest_limits(digest_max);
        self.user_message.len() <= user_max && self.assistant_message.len() <= assistant_max
    }

    /// The digest with the head of the message containing chunk `i` replaced by that window.
    pub fn chunk_view(&self, i: usize, digest_max: usize) -> Option<String> {
        let c = self.chunks.get(i)?;
        let (user_max, assistant_max) = Self::digest_limits(digest_max);
        let window = memory_rules::util::excerpt(self.message(&c.role), c.start, c.end)?;
        Some(if c.role == "user" {
            format!(
                "{window}\n{}",
                memory_rules::util::truncate_bytes(&self.assistant_message, assistant_max)
            )
        } else {
            format!(
                "{}\n{window}",
                memory_rules::util::truncate_bytes(&self.user_message, user_max)
            )
        })
    }

    /// The bounded view formation reads: a message over `max_bytes` keeps its head and tail halves.
    pub fn clipped(&self, max_bytes: usize) -> TurnRec {
        let mut tr = self.clone();
        tr.user_message = memory_rules::util::clip_message(&self.user_message, max_bytes);
        tr.assistant_message = memory_rules::util::clip_message(&self.assistant_message, max_bytes);
        tr.chunks.clear(); // offsets refer to the stored messages
        tr
    }
}

pub fn turn_key(platform: &str, turn_id: &str) -> String {
    format!("{platform}::{turn_id}")
}

pub fn session_key(platform: &str, session: &str) -> String {
    format!("{platform}::{session}")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionRec {
    pub key: String,
    pub platform: String,
    pub session: String,
    pub project: String,
    pub goal: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub open_issues: Vec<String>,
    pub turn_count: i64,
    pub updated_at: String,
    #[serde(default)]
    pub last_turn_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    /// Last turn covered by a successful session-end experience/summary job.
    #[serde(default)]
    pub last_experience_seq: i64,
    /// Unfinished tasks retain their original turns across formation batches and buffer purge.
    #[serde(default)]
    pub pending_tasks: Vec<PendingTask>,
    /// Highest turn already reported by a session-end receipt; older failures are not reported again.
    #[serde(default)]
    pub end_checked_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PendingTask {
    pub task: String,
    pub turn_ids: Vec<String>,
    /// Watermark of the batch that last extended this task; stale tasks expire.
    #[serde(default)]
    pub updated_seq: i64,
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
pub struct EntityRec {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub scope: MemScope,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub tenant: String,
    pub platform: String,
}

// ---------------------------------------------------------------- errors

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    InvalidArgument(String),
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
