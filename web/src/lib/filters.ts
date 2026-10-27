import { PLATFORM } from '../api/client'
import type {
  ListMemoriesReq,
  ListModelResponsesReq,
  ListRequestsReq,
  ListRetrievalsReq,
  MemoryStatus,
  MemoryType,
  RequestKind,
  RetrievalKind,
  ScopeKind,
  TaskStatus,
} from '../api/types'

export const MEMORY_TYPES: MemoryType[] = ['fact', 'instruction', 'experience', 'profile', 'skill']
export const SCOPE_KINDS: ScopeKind[] = ['global', 'project', 'platform']
/** What `list_memories` returns when no status is given: the indexed, recallable records. */
export const ACTIVE_STATUSES: MemoryStatus[] = ['current', 'conflict', 'candidate']
export const ALL_STATUSES: MemoryStatus[] = [...ACTIVE_STATUSES, 'superseded', 'stale']

/** The memory list's filters; `""` is no filter. They live in the hash query so a view survives a reload. */
export interface MemoryFilters {
  project: string
  scopeKind: ScopeKind | ''
  sourcePlatform: string
  type: MemoryType | ''
  origin: string
  /** Never empty, in `ALL_STATUSES` order. */
  statuses: MemoryStatus[]
  text: string
}

export const NO_FILTERS: MemoryFilters = {
  project: '',
  scopeKind: '',
  sourcePlatform: '',
  type: '',
  origin: '',
  statuses: ACTIVE_STATUSES,
  text: '',
}

const oneOf = <T extends string>(values: readonly T[], value: string | null): T | '' =>
  values.includes(value as T) ? (value as T) : ''

const isDefault = (statuses: MemoryStatus[]) =>
  statuses.length === ACTIVE_STATUSES.length && ACTIVE_STATUSES.every((s) => statuses.includes(s))

/** The URL is where filters enter: unknown types, scopes and statuses are dropped here. */
export function filtersFromParams(params: URLSearchParams): MemoryFilters {
  const listed = params.get('status')?.split(',') ?? []
  const statuses = ALL_STATUSES.filter((s) => listed.includes(s))
  return {
    project: params.get('project') ?? '',
    scopeKind: oneOf(SCOPE_KINDS, params.get('scope')),
    sourcePlatform: params.get('platform') ?? '',
    type: oneOf(MEMORY_TYPES, params.get('type')),
    origin: params.get('origin') ?? '',
    statuses: statuses.length ? statuses : ACTIVE_STATUSES,
    text: params.get('q') ?? '',
  }
}

export function filtersToParams(f: MemoryFilters): URLSearchParams {
  const params = new URLSearchParams()
  const set = (key: string, value: string) => value && params.set(key, value)
  set('project', f.project)
  set('scope', f.scopeKind)
  set('platform', f.sourcePlatform)
  set('type', f.type)
  set('origin', f.origin)
  if (!isDefault(f.statuses)) set('status', f.statuses.join(','))
  set('q', f.text.trim())
  return params
}

/** The `list_memories` body for one page; unset filters and the default statuses are left to the daemon. */
export function listRequest(f: MemoryFilters, cursor?: string): ListMemoriesReq {
  const text = f.text.trim()
  return {
    ...(f.project && { project: f.project }),
    ...(f.scopeKind && { scopeKind: f.scopeKind }),
    ...(f.sourcePlatform && { sourcePlatform: f.sourcePlatform }),
    ...(f.type && { types: [f.type] }),
    ...(f.origin && { origins: [f.origin] }),
    ...(!isDefault(f.statuses) && { statuses: f.statuses }),
    ...(text && { text }),
    ...(cursor && { cursor }),
  }
}

// ---------------------------------------------------------------- recall log

export const RETRIEVAL_KINDS: RetrievalKind[] = ['search', 'context']

/** The recall log's filters; `""` is no filter. */
export interface RecallFilters {
  kind: RetrievalKind | ''
  platform: string
  project: string
  session: string
  /** Off by default: the UI's own searches and previews would otherwise fill the log. */
  showWeb: boolean
}

export const NO_RECALL_FILTERS: RecallFilters = { kind: '', platform: '', project: '', session: '', showWeb: false }

export function recallFiltersFromParams(params: URLSearchParams): RecallFilters {
  return {
    kind: oneOf(RETRIEVAL_KINDS, params.get('kind')),
    platform: params.get('platform') ?? '',
    project: params.get('project') ?? '',
    session: params.get('session') ?? '',
    showWeb: params.get('web') === '1',
  }
}

export function recallFiltersToParams(f: RecallFilters): URLSearchParams {
  const params = new URLSearchParams()
  const set = (key: string, value: string) => value && params.set(key, value)
  set('kind', f.kind)
  set('platform', f.platform.trim())
  set('project', f.project.trim())
  set('session', f.session.trim())
  if (f.showWeb) params.set('web', '1')
  return params
}

/** The `list_retrievals` body for one page. Asking for the web platform by name shows its calls either way. */
export function recallRequest(f: RecallFilters, cursor?: string): ListRetrievalsReq {
  const platform = f.platform.trim()
  const project = f.project.trim()
  const session = f.session.trim()
  return {
    ...(f.kind && { kind: f.kind }),
    ...(platform && { platform }),
    ...(!f.showWeb && platform !== PLATFORM && { excludePlatforms: [PLATFORM] }),
    ...(project && { project }),
    ...(session && { session }),
    ...(cursor && { cursor }),
  }
}

// ---------------------------------------------------------------- tasks and model responses

export const TASK_STATUSES: TaskStatus[] = ['queued', 'running', 'indexing', 'completed', 'failed']
export const REQUEST_KINDS: RequestKind[] = ['mem0', 'instruction']

/** The task pages' filters: the request list's and the model-call tab's, which share one URL. */
export interface TaskFilters {
  tab: 'requests' | 'responses'
  /** Never empty, in `TASK_STATUSES` order; all of them is no filter. */
  statuses: TaskStatus[]
  kind: RequestKind | ''
  platform: string
  project: string
  stage: string
  /** On by default: the tab is for finding what went wrong. */
  failedOnly: boolean
}

export const NO_TASK_FILTERS: TaskFilters = {
  tab: 'requests',
  statuses: TASK_STATUSES,
  kind: '',
  platform: '',
  project: '',
  stage: '',
  failedOnly: true,
}

const everyStatus = (statuses: TaskStatus[]) => statuses.length === TASK_STATUSES.length

export function taskFiltersFromParams(params: URLSearchParams): TaskFilters {
  const listed = params.get('status')?.split(',') ?? []
  const statuses = TASK_STATUSES.filter((s) => listed.includes(s))
  return {
    tab: params.get('tab') === 'responses' ? 'responses' : 'requests',
    statuses: statuses.length ? statuses : TASK_STATUSES,
    kind: oneOf(REQUEST_KINDS, params.get('kind')),
    platform: params.get('platform') ?? '',
    project: params.get('project') ?? '',
    stage: params.get('stage') ?? '',
    failedOnly: params.get('all') !== '1',
  }
}

export function taskFiltersToParams(f: TaskFilters): URLSearchParams {
  const params = new URLSearchParams()
  const set = (key: string, value: string) => value && params.set(key, value)
  if (f.tab === 'responses') params.set('tab', f.tab)
  if (!everyStatus(f.statuses)) set('status', f.statuses.join(','))
  set('kind', f.kind)
  set('platform', f.platform.trim())
  set('project', f.project.trim())
  set('stage', f.stage.trim())
  if (!f.failedOnly) params.set('all', '1')
  return params
}

/** The `list_requests` body for one page. */
export function taskRequest(f: TaskFilters, cursor?: string): ListRequestsReq {
  const platform = f.platform.trim()
  const project = f.project.trim()
  return {
    ...(!everyStatus(f.statuses) && { statuses: f.statuses }),
    ...(f.kind && { kind: f.kind }),
    ...(platform && { platform }),
    ...(project && { project }),
    ...(cursor && { cursor }),
  }
}

/** The `list_model_responses` body for one page; `failedOnly` is sent either way, since the daemon's default is off. */
export function responsesRequest(f: TaskFilters, cursor?: string): ListModelResponsesReq {
  const stage = f.stage.trim()
  return { ...(stage && { stage }), failedOnly: f.failedOnly, ...(cursor && { cursor }) }
}
