// Request and response shapes of the daemon, written by hand after the Rust types: `memory-domain/src/types.rs`
// (records, requests), `memory-engine` (get_memory, correct/delete, context, tasks), `memory-algo-mem0` (search,
// list_turns) and `memory-service/src/api/http.rs` (health, errors). JSON is camelCase.

export type MemoryType = 'fact' | 'instruction' | 'experience' | 'profile' | 'skill'
export type MemoryStatus = 'current' | 'conflict' | 'candidate' | 'superseded' | 'stale'
export type ScopeKind = 'global' | 'project' | 'platform'

export interface MemScope {
  kind: ScopeKind
  project?: string
  platform?: string
}

/** Request-side scope; `"*"` means unrestricted. The platform otherwise comes from the connection. */
export interface ReqScope {
  project?: string
  platform?: string
  session?: string
}

export interface Source {
  platform: string
  turnIds: string[]
  origin: string
}

export interface RelEdge {
  id: string
  /** "conflicts" | "related" | "same" */
  relation: string
}

export interface Derivation {
  stage: string
  inputMemoryIds: string[]
  inputTurnIds: string[]
  currentView: boolean
}

/** `MemoryRec::public()`. Optional links are `null`, never absent; `effectiveAt` is `""` when unknown. */
export interface Memory {
  id: string
  type: MemoryType
  text: string
  scope: MemScope
  status: MemoryStatus
  effectiveAt: string
  recordedAt: string
  source: Source
  supersedes: string | null
  supersededBy: string | null
  related: RelEdge[]
  validUntil: string | null
  detail: unknown
  derivation: Derivation | null
  staleReason: string | null
}

/** `list_memories` rows and `get_memory` records also carry when the fact was said (RFC3339 or `""`). */
export type StatedMemory = Memory & { statedAt: string }

export interface ApiErrorBody {
  code: string
  message: string
  /** Set on 409 NOT_CURRENT: the current value of the record's chain. */
  currentId?: string
}

// ---------------------------------------------------------------- health / admin

export interface LlmUsage {
  calls: number
  attempts: number
  promptTokens: number
  completionTokens: number
  cacheHitTokens: number
  model: string
}

export interface Health {
  ok: boolean
  /** Absent on a daemon older than the web UI. */
  version?: string
  promptVariant: string
  embedModel: string
  rerankModel: string | null
  modelBackend: string
  modelDevice: string
  modelDtype: string
  llm: LlmUsage
  memoryStrategy: string
}

export interface Stats {
  counts: Record<string, number>
  /** type → status → count */
  memories: Record<string, Record<string, number>>
  llm: { process: LlmUsage; persisted: Record<string, number>; rows: number }
  index: { memoryVectors: number; pendingTasks: number }
  /** The two counts cover active records and never count the UI's own calls (`memory_recalls`). */
  retrieval: { gapSearches: number; neverRecalled: number; notRecalled30d: number }
  jobs: { pendingJobs: number }
  /** Whether consolidation may run (`consolidationEnabled`) and a generation model is configured. */
  features: { consolidation: boolean; generation: boolean }
}

export interface Facet {
  name: string
  count: number
}

export interface Facets {
  projects: Facet[]
  sourcePlatforms: Facet[]
  origins: Facet[]
  byTypeStatus: Record<string, Record<string, number>>
  total: number
}

export interface ListMemoriesReq {
  project?: string
  scopeKind?: ScopeKind
  sourcePlatform?: string
  types?: MemoryType[]
  /** Omitted = current, conflict and candidate. */
  statuses?: MemoryStatus[]
  origins?: string[]
  text?: string
  /** Recorded more than N days ago with no recall in the last N days (1–3650). Not with `neverRecalled`. */
  notRecalledDays?: number
  /** Records with no `memory_recalls` row. */
  neverRecalled?: true
  cursor?: string
  limit?: number
}

/** A `list_memories` row: also when agents last recalled it and how often (the UI's own calls do not count). */
export type ListedMemory = StatedMemory & { lastRecalledAt: string | null; recallCount: number }

export interface ListMemoriesRes {
  memories: ListedMemory[]
  nextCursor: string | null
  total: number
}

// ---------------------------------------------------------------- reads

export interface Evidence {
  id: string
  turnId: string
  platform: string
  /** "user" | "assistant" | "text" | "correction" */
  role: string
  span: string
  statedAt: string
  createdAt: string
  /** The question a first-hand reply answered. */
  context: { span: string; statedAt: string } | null
}

/** Batch-level provenance of a Mem0 fact: the whole input request, not a verified quote of the claim. */
export interface SourceBatch {
  level: string
  inputRequestIds: string[]
  observedAt?: string
  attributedTo?: unknown
  verifiedClaimSpan: boolean
}

export interface GetMemoryRes {
  memory: StatedMemory
  /** The whole correction chain, oldest first, including `memory`. */
  history: StatedMemory[]
  sourceBatch?: SourceBatch
  evidence: Evidence[]
}

export interface InMessage {
  role: string
  content: string
  timestamp: string
}

export interface Turn {
  turnId: string
  requestId: string
  platform: string
  project: string
  session: string
  messages: InMessage[]
  startedAt: string
  endedAt: string
  extractState: string
}

export interface ListTurnsRes {
  turns: Turn[]
  tokensUsed: number
}

export interface SearchReq {
  query: string
  scope: ReqScope
  topK?: number
  tokenBudget?: number
  types?: MemoryType[]
  rerank?: boolean
}

export interface SearchHit {
  memory: Memory
  /** Hybrid score; reranking adds `rerankScore` and reorders. */
  score: number
  slot: number
  rerankScore?: number
}

export interface SearchLatency {
  embed: number
  recall: number
  rerank: number
  assemble: number
  total: number
}

export interface SearchRes {
  retrievalId: string
  results: SearchHit[]
  gap: boolean
  tokensUsed: number
  latencyMs: SearchLatency
}

export interface GetContextReq {
  scope: ReqScope
  tokenBudget?: number
}

export interface ContextRes {
  text: string
  instructions: Memory[]
  profile: Memory | null
  projectNotes: Memory | null
  projectFacts: Memory[]
  facts: (Memory & { pendingConfirmation?: boolean })[]
  skills: { id: string; name: string; preconditions: string }[]
  tokensUsed: number
  retrievalId: string
}

// ---------------------------------------------------------------- writes

export interface AddMemoriesReq {
  scope: { project: string; session: string }
  text: string
  infer?: boolean
  sync?: boolean
  memoryType?: 'fact' | 'instruction'
  memoryScope?: MemScope
}

export type TaskStatus = 'queued' | 'running' | 'indexing' | 'completed' | 'failed'

/** `get_task`, and what `add_memories` answers with. */
export interface Task {
  requestId: string
  status: TaskStatus
  memoryIds: string[]
  /** Absent on an instruction receipt. */
  error?: string | null
  formed?: boolean
  searchable?: boolean
  duplicate?: boolean
}

export interface CorrectMemoryReq {
  id: string
  /** Omitted: retract (the record stops holding, with no successor). */
  text?: string
  effectiveAt?: string
  reason?: string
}

export interface CorrectMemoryRes {
  /** The new value, or the retracted record. */
  memory: Memory
  correctedId: string
  retracted?: boolean
}

export interface DeleteMemoryRes {
  deleted: string
  /** Profiles, skills and consolidation merges deleted with it. */
  derivedDeleted: string[]
  /** Inputs of a deleted merge that are current again. */
  restoredInputs: string[]
  relinked: { prev: string; next: string } | null
  edgesRemoved: number
  evidenceDeleted: number
  evidenceUpdated: number
  turnsUpdated: number
}

// ---------------------------------------------------------------- phase 2: review and tracing
// Import runs after `memory-domain/src/import.rs`; consolidation after `memory-domain/src/consolidation.rs` and
// `memory-engine/src/app/consolidation.rs`; the admin lists after docs/api.md.

/** A record as a page's `memories` map summarises it; an id missing from the map no longer exists. */
export interface MemorySummary {
  id: string
  type: MemoryType
  status: MemoryStatus
  text: string
  scope: MemScope
}

export type MemorySummaries = Record<string, MemorySummary>

// -------------------------------- import runs

export type ImportRunStatus =
  | 'proposing'
  | 'proposed'
  | 'applying'
  | 'applied'
  | 'rolling_back'
  | 'rolled_back'
  | 'partially_rolled_back'

export type ImportProposalStatus = 'proposed' | 'rejected' | 'dropped' | 'applied' | 'skipped' | 'rolled_back'

export type Decision = 'accept' | 'reject'

export interface SessionTurn {
  turnId: string
  session: string
  nativeId: string
  messages: InMessage[]
}

export interface SimilarMemory {
  memoryId: string
  text: string
  score: number
}

export interface ImportProposal {
  proposalId: string
  runId: string
  project: string
  host: string
  /** The unit's index in the run; proposals arrive ordered by it and `position`. */
  cluster: number
  position: number
  unit: string
  /** "verbatim" | "split" | "extract" | "session-turn" */
  mode: string
  /** "fact" | "preference" | "reference" | "session-turn" */
  itemKind: string
  /** "project" | "global"; a hint only. */
  scopeHint: string
  text: string
  evidence: string
  statedAt: string
  headingPath: string[]
  source: unknown
  status: ImportProposalStatus
  rejectReason: string | null
  duplicateOf: string | null
  supersedes: string | null
  similar: SimilarMemory[]
  createdAt: string
  decision: Decision | null
  memoryId: string | null
  skipReason: string | null
  rollbackSkipReason: string | null
  /** A session turn's: `{queued, requestId?, duplicate?}`. */
  applied: unknown
  appliedAt: string | null
  rolledBackAt: string | null
  turn?: SessionTurn
}

export interface ImportUnitOutcome {
  unit: string
  index: number
  mode: string
  /** "new" | "changed" | "unchanged" | "reconsidered" | "failed" */
  outcome: string
  proposals: number
  rejected: number
  coverage: number | null
  cached: boolean
  previousMemoryIds: string[]
  previousRecords?: { memoryId: string; type: string; text: string }[]
  error: string | null
}

export interface ImportCounts {
  units: number
  unchanged: number
  proposals: number
  rejected: number
  duplicates: number
  generationCalls: number
  cacheHits: number
  applied: number
  dropped: number
  skipped: number
  rolledBack: number
  failed: number
  sessionTurns: number
  turnsQueued: number
  turnsRolledBack: number
}

export interface ImportRun {
  runId: string
  project: string
  host: string
  status: ImportRunStatus
  createdAt: string
  updatedAt: string
  finishedAt: string | null
  limits: { recallBudget: number; recallTopK: number }
  llmModel: string
  embeddingModel: string
  units: ImportUnitOutcome[]
  counts: ImportCounts
  confirmedVia?: string
  error: string | null
}

export interface ImportRunsReq {
  project?: string
  limit?: number
}

export interface ImportRunRes {
  run: ImportRun
  proposals: ImportProposal[]
}

/** `import_apply`: a proposal `decisions` does not name is accepted. The web UI never sends `confirmedVia`. */
export interface ImportApplyReq {
  runId: string
  decisions: Record<string, Decision>
}

// -------------------------------- consolidation

export type ConsolidationRunStatus =
  | 'proposed'
  | 'aborted'
  | 'failed'
  | 'applying'
  | 'applied'
  | 'rolling_back'
  | 'rolled_back'
  | 'partially_rolled_back'

export type ConsolidationProposalStatus =
  | 'proposed'
  | 'rejected'
  | 'aborted'
  | 'failed'
  | 'applied'
  | 'skipped'
  | 'rolled_back'

export interface ConsolidationLimits {
  vectorLink: number
  minCluster: number
  maxCluster: number
  maxClustersPerRun: number
  maxMergedTextBytes: number
  maxRetireFraction: number
}

/** A `consolidation_runs` row: the run without `clusters`, `proposalIds` and `responseLogIds`. */
export interface ConsolidationRunSummary {
  runId: string
  project: string
  status: ConsolidationRunStatus
  createdAt: string
  finishedAt?: string
  error: string | null
  projectFacts: number
  eligibleFacts: number
  clusterCount: number
  processedClusters: number
  acceptedOps: number
  rejectedOps: number
  retireCount: number
  retireFraction: number
  limits: ConsolidationLimits
  llmModel: string
  /** Set by apply and rollback. */
  appliedOps?: number
  skippedOps?: number
  rolledBackOps?: number
  remainingAppliedOps?: number
}

export interface ConsolidationCluster {
  index: number
  memberIds: string[]
  score: number
  error?: string
}

export type ConsolidationRun = ConsolidationRunSummary & {
  clusters: ConsolidationCluster[]
  responseLogIds: string[]
}

/**
 * The op is flattened into the proposal row (`#[serde(tag = "op")]`): `op` names it and its fields are siblings,
 * `{"op": "merge", "ids": [...], "text": "...", "proposalId": ...}`.
 */
export type ConsolidationOp =
  | { op: 'merge'; ids: string[]; text: string }
  | { op: 'supersede'; old: string; new: string }
  | { op: 'conflict'; ids: string[]; reason: string }
  | { op: 'keep'; ids: string[] }

export interface FieldPatch {
  id: string
  before: Record<string, unknown>
  after: Record<string, unknown>
}

export type ConsolidationProposal = ConsolidationOp & {
  proposalId: string
  runId: string
  cluster: number
  position: number
  memoryIds: string[]
  status: ConsolidationProposalStatus
  rejectReason: string | null
  skipReason?: string | null
  rollbackSkipReason?: string | null
  /** What apply changed: the merged record, the opened question and every field it wrote. */
  applied?: { mergedId?: string; uncertaintyId?: string; patches: FieldPatch[] }
}

/** `consolidate`, `consolidation_apply` and `consolidation_rollback`. */
export interface ConsolidationRunRes {
  run: ConsolidationRun
  proposals: ConsolidationProposal[]
}

export type ConsolidationRunDetail = ConsolidationRunRes & { memories: MemorySummaries }

// -------------------------------- uncertainties

export type UncertaintyStatus = 'open' | 'resolved' | 'withdrawn'

export interface ListUncertaintiesReq {
  status?: UncertaintyStatus
  project?: string
  limit?: number
}

export interface Uncertainty {
  id: string
  memoryIds: string[]
  scope: MemScope
  reason: string
  status: UncertaintyStatus
  createdAt: string
  resolvedAt?: string
  resolution?: { keepMemoryId: string; platform: string; reason: string }
  withdrawnAt?: string
  withdrawnBy?: string
  /** The members that still exist, in `memoryIds` order, as `get_memory` returns them. */
  memories: StatedMemory[]
}

export interface ListUncertaintiesRes {
  uncertainties: Uncertainty[]
}

export interface ResolveUncertaintyReq {
  id: string
  keepMemoryId: string
  reason: string
}

export interface ResolveUncertaintyRes {
  id: string
  status: 'resolved'
  keepMemoryId: string
}

// -------------------------------- retrieval log

export type RetrievalKind = 'search' | 'context'

export interface ListRetrievalsReq {
  kind?: RetrievalKind
  platform?: string
  excludePlatforms?: string[]
  project?: string
  session?: string
  /** Only rows of the last 30 days. */
  memoryId?: string
  cursor?: string
  limit?: number
}

export interface Retrieval {
  id: string
  kind: RetrievalKind
  /** Absent on legacy searches. */
  platform?: string
  project?: string
  session?: string
  /** Null for context rows. */
  query: string | null
  createdAt: string
  createdTs: number
  /** `tokens` is missing on searches logged before it was recorded (startup could not fill it in). */
  returned: { memories: { id: string; rank: number }[]; tokens?: number; gap?: boolean }
  latency?: Record<string, number>
  feedback?: { used: string[]; applied: string[]; outcome: string; at: string }
}

export interface ListRetrievalsRes {
  retrievals: Retrieval[]
  memories: MemorySummaries
  nextCursor: string | null
  /** `"30d"`, only when `memoryId` was sent. */
  window?: string
}

// -------------------------------- requests and model responses

export type RequestKind = 'mem0' | 'instruction'

export interface ListRequestsReq {
  statuses?: TaskStatus[]
  kind?: RequestKind
  platform?: string
  project?: string
  cursor?: string
  limit?: number
}

/** A `requests` row. Instruction receipts carry only the first block of fields. */
export interface RequestRow {
  id: string
  requestId: string
  kind: RequestKind
  status: TaskStatus
  memoryIds: string[]
  platform: string
  project: string
  session: string
  createdAt: string
  updatedAt: string
  turnId?: string | null
  importRunId?: string | null
  infer?: boolean
  formed?: boolean
  searchable?: boolean
  failures?: number
  error?: string | null
  retryAtTs?: number
  latencyMs?: Record<string, number>
}

export type ListedRequest = RequestRow & { messageCount: number }

export interface ListRequestsRes {
  requests: ListedRequest[]
  nextCursor: string | null
}

export interface Extraction {
  id: string
  stage: string
  requestId: string
  status: string
  output?: unknown
  responseLogId?: string | null
  createdAt?: string
}

export interface ModelResponse {
  id: string
  stage: string
  model: string
  createdAt: string
  attempt: number
  httpStatus?: number
  transportError?: string
  request: unknown
  rawBody?: string
  rawContent?: string
  validationError?: string | null
  validOutputShape?: boolean
  latencyMs?: number
  finishReason?: string
}

export interface RequestDetail {
  request: RequestRow & { inputMessages?: InMessage[] }
  /** Oldest first. */
  extractions: Extraction[]
  responses: ModelResponse[]
}

export interface ListModelResponsesReq {
  stage?: string
  failedOnly?: boolean
  cursor?: string
  limit?: number
}

export interface ListModelResponsesRes {
  responses: ModelResponse[]
  nextCursor: string | null
}

// ---------------------------------------------------------------- phase 3: lineage, entities
// `memory_lineage` after `memory-engine/app/management`; the entity reads after the `MemoryAlgorithm` methods that
// Mem0 implements (the projection is its private `mem0_entities` table).

/** A record in the lineage: its summary, its times and where it came from; `depth` is its distance from the root. */
export interface LineageNode extends MemorySummary {
  recordedAt: string
  effectiveAt: string
  statedAt: string
  origin: string
  platform: string
  depth: number
}

/**
 * An edge points from the record that names the link to the record it names. `derived` carries the derivation stage
 * of `from`; `related` one edge per unordered pair and relation ("conflicts" | "related" | "same").
 */
export type LineageEdge = { from: string; to: string } & (
  | { kind: 'supersedes' | 'uses' }
  | { kind: 'derived'; stage: string }
  | { kind: 'related'; relation: string }
)
export type LineageEdgeKind = LineageEdge['kind']

export interface LineageRes {
  root: string
  nodes: LineageNode[]
  edges: LineageEdge[]
  /** The walk stopped at 200 nodes. */
  truncated: boolean
}

export interface Entity {
  id: string
  text: string
  entityType: string
  scope: MemScope
  /** How many memories the projection links to it. */
  memoryCount: number
}

/** The entity projection is read as stored; this is when it was last written, or null before it exists. */
export interface Projected {
  projectionUpdatedAt: string | null
}

export interface ListEntitiesReq {
  project?: string
  entityType?: string
  /** Case-insensitive substring of the text. */
  text?: string
  cursor?: string
  /** 1–200, default 50. */
  limit?: number
}

export interface ListEntitiesRes extends Projected {
  entities: Entity[]
  /** Entity types under the request's `project` filter only, by name. */
  types: Facet[]
  nextCursor: string | null
}

export interface EntityRes extends Projected {
  entity: Entity & { normalizedText: string }
  /** The linked records that still exist, newest recorded first. */
  memories: StatedMemory[]
}

export interface MemoryEntitiesRes extends Projected {
  /** By text. */
  entities: Entity[]
}
