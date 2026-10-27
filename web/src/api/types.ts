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
  retrieval: { gapSearches: number }
  jobs: { pendingJobs: number }
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
  cursor?: string
  limit?: number
}

export interface ListMemoriesRes {
  memories: StatedMemory[]
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
