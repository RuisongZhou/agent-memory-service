import type { ListMemoriesReq, MemoryStatus, MemoryType, ScopeKind } from '../api/types'

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
