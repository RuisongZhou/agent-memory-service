import type {
  AddMemoriesReq,
  ApiErrorBody,
  ContextRes,
  CorrectMemoryReq,
  CorrectMemoryRes,
  DeleteMemoryRes,
  Facets,
  GetContextReq,
  GetMemoryRes,
  Health,
  ListMemoriesReq,
  ListMemoriesRes,
  ListTurnsRes,
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
}
