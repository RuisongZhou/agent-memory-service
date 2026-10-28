import type {
  AddMemoriesReq,
  ApiErrorBody,
  ConnectReq,
  ConnectRes,
  ConsolidationRunDetail,
  ConsolidationRunRes,
  ConsolidationRunSummary,
  ContextRes,
  CorrectMemoryReq,
  CorrectMemoryRes,
  DeleteMemoryRes,
  EntityRes,
  Facets,
  GetContextReq,
  GetMemoryRes,
  Health,
  ImportApplyReq,
  ImportRun,
  ImportRunRes,
  ImportRunsReq,
  LineageRes,
  ListEntitiesReq,
  ListEntitiesRes,
  ListMemoriesReq,
  ListMemoriesRes,
  ListModelResponsesReq,
  ListModelResponsesRes,
  ListRequestsReq,
  ListRequestsRes,
  ListRetrievalsReq,
  ListRetrievalsRes,
  ListTurnsRes,
  ListUncertaintiesReq,
  ListUncertaintiesRes,
  MemoryEntitiesRes,
  PlatformActivityRes,
  PlatformsRes,
  RequestDetail,
  ResolveUncertaintyReq,
  ResolveUncertaintyRes,
  SearchReq,
  SearchRes,
  Stats,
  Task,
} from './types'

/** The platform this client writes and reads as; the daemon records it on writes and retrieval logs. */
export const PLATFORM = 'web'

/** `list_turns` skips a batch larger than the budget instead of truncating it, so ask for a generous one. */
const TURNS_TOKEN_BUDGET = 100_000

/** A non-2xx answer: the daemon's `{code, message, currentId?}` body, or the bare status when there is none. */
export class ApiError extends Error {
  readonly status: number
  readonly code: string
  readonly currentId?: string

  constructor(status: number, body: ApiErrorBody) {
    super(body.message)
    this.name = 'ApiError'
    this.status = status
    this.code = body.code
    this.currentId = body.currentId
  }
}

async function read<T>(res: Response): Promise<T> {
  if (res.ok) return (await res.json()) as T
  const text = await res.text()
  let body: ApiErrorBody
  try {
    body = JSON.parse(text) as ApiErrorBody
  } catch {
    // Not the daemon's error body: a proxy error page or a daemon that is not running.
    body = { code: `HTTP_${res.status}`, message: text || res.statusText }
  }
  throw new ApiError(res.status, body)
}

async function post<T>(path: string, body: unknown): Promise<T> {
  const res = await fetch(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Memory-Platform': PLATFORM },
    body: JSON.stringify(body),
  })
  return read<T>(res)
}

export const api = {
  health: async () => read<Health>(await fetch('/health')),
  /** The version this UI was built as (`dist/version.json`). */
  uiVersion: async () => read<{ version: string }>(await fetch(`${import.meta.env.BASE_URL}version.json`)),
  stats: () => post<Stats>('/admin/stats', {}),
  facets: () => post<Facets>('/admin/memory_facets', {}),
  listMemories: (req: ListMemoriesReq) => post<ListMemoriesRes>('/admin/list_memories', req),
  getMemory: (id: string) =>
    post<GetMemoryRes>('/v1/get_memory', { id, includeHistory: true, includeEvidence: true }),
  /** Raw input batches across every platform. */
  listTurns: (turnIds: string[]) =>
    post<ListTurnsRes>('/v1/list_turns', { scope: { platform: '*' }, turnIds, tokenBudget: TURNS_TOKEN_BUDGET }),
  addMemories: (req: AddMemoriesReq) => post<Task>('/v1/add_memories', req),
  getTask: (requestId: string) => post<Task>('/v1/get_task', { requestId }),
  /** Browsing never changes a memory's access time. */
  search: (req: SearchReq) => post<SearchRes>('/v1/search_memories', { ...req, touch: false }),
  getContext: (req: GetContextReq) => post<ContextRes>('/v1/get_context', req),
  correctMemory: (req: CorrectMemoryReq) => post<CorrectMemoryRes>('/v1/correct_memory', req),
  deleteMemory: (id: string) => post<DeleteMemoryRes>('/v1/delete_memory', { id }),

  importRuns: (req: ImportRunsReq) => post<{ runs: ImportRun[] }>('/admin/import_runs', req),
  importRun: (runId: string) => post<ImportRunRes>('/admin/import_run', { runId }),
  importApply: (req: ImportApplyReq) => post<ImportRunRes>('/admin/import_apply', req),
  importRollback: (runId: string) => post<ImportRunRes>('/admin/import_rollback', { runId }),

  consolidationRuns: (req: { project?: string; limit?: number }) =>
    post<{ runs: ConsolidationRunSummary[] }>('/admin/consolidation_runs', req),
  consolidationRun: (runId: string) => post<ConsolidationRunDetail>('/admin/consolidation_run', { runId }),
  /** Proposes only: one generation call per cluster, no record changes. */
  consolidate: (project: string) => post<ConsolidationRunRes>('/admin/consolidate', { project, dryRun: true }),
  consolidationApply: (runId: string) => post<ConsolidationRunRes>('/admin/consolidation_apply', { runId }),
  consolidationRollback: (runId: string) => post<ConsolidationRunRes>('/admin/consolidation_rollback', { runId }),

  listUncertainties: (req: ListUncertaintiesReq) => post<ListUncertaintiesRes>('/admin/list_uncertainties', req),
  resolveUncertainty: (req: ResolveUncertaintyReq) => post<ResolveUncertaintyRes>('/v1/resolve_uncertainty', req),

  listRetrievals: (req: ListRetrievalsReq) => post<ListRetrievalsRes>('/admin/list_retrievals', req),
  platformActivity: () => post<PlatformActivityRes>('/admin/platform_activity', {}),
  /** The installer's report of every platform; 404 NOT_FOUND when the daemon has no installer copy. */
  platforms: () => post<PlatformsRes>('/admin/platforms', {}),
  connect: (req: ConnectReq) => post<ConnectRes>('/admin/connect', req),
  listRequests: (req: ListRequestsReq) => post<ListRequestsRes>('/admin/list_requests', req),
  getRequest: (requestId: string) => post<RequestDetail>('/admin/request', { requestId }),
  listModelResponses: (req: ListModelResponsesReq) =>
    post<ListModelResponsesRes>('/admin/list_model_responses', req),

  /** The record's links in both directions, `depth` steps out (1–6). */
  memoryLineage: (id: string, depth: number) => post<LineageRes>('/admin/memory_lineage', { id, depth }),
  /** The entity reads show the projection as stored: they never call a model. */
  listEntities: (req: ListEntitiesReq) => post<ListEntitiesRes>('/admin/list_entities', req),
  entity: (id: string) => post<EntityRes>('/admin/entity', { id }),
  memoryEntities: (id: string) => post<MemoryEntitiesRes>('/admin/memory_entities', { id }),
}
