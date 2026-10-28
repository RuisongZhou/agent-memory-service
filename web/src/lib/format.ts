import type { ClientKind, MemScope, MemoryStatus, MemoryType, ScopeKind } from '../api/types'

export const TYPE_LABEL: Record<MemoryType, string> = {
  fact: '事实',
  instruction: '指令',
  experience: '经验',
  profile: '画像',
  skill: '技能',
}

export const STATUS_LABEL: Record<MemoryStatus, string> = {
  current: '当前',
  conflict: '冲突',
  candidate: '候选',
  superseded: '已取代',
  stale: '已失效',
}

export const SCOPE_LABEL: Record<ScopeKind, string> = { global: '全局', project: '项目', platform: '平台' }

/** Origins are open-ended strings; these are the ones the daemon writes today. */
export const ORIGIN_LABEL: Record<string, string> = {
  explicit: '显式',
  user: '用户说的',
  assistant: '助手说的',
  inferred: '推断',
  imported: '导入',
}

/** Who made a call on a platform (`X-Memory-Client`). */
export const CLIENT_LABEL: Record<ClientKind, string> = { hook: 'hook', mcp: 'MCP', http: 'HTTP' }

/** The memory list's recall filter (`RECALL_FILTERS`). */
export const RECALL_FILTER_LABEL = { never: '从未被召回', '30': '30 天没被召回', '90': '90 天没被召回' } as const

export const RELATION_LABEL: Record<string, string> = { conflicts: '冲突', related: '相关', same: '相同' }

export const EVIDENCE_ROLE_LABEL: Record<string, string> = {
  user: '用户',
  assistant: '助手',
  text: '文本',
  correction: '更正',
}

/** Reads as `web` see only `web`'s platform-scoped records unless they ask for every platform. */
export const ALL_PLATFORMS_LABEL = '跨平台（scope.platform: "*"）：含所有平台的平台级记忆；关闭时只含 web 平台的'

export const labelled = (labels: Record<string, string>, value: string) =>
  labels[value] ? `${labels[value]} (${value})` : value

/**
 * A project id is the absolute path of its folder, such as `/Users/me/code/repo`: tables show its last segment and
 * keep the whole path for a `title`. Any other id (older projects, names an MCP client chose) is shown as it is.
 */
export function projectName(id: string): string {
  if (!id.startsWith('/')) return id
  return id.split('/').filter(Boolean).pop() ?? id
}

/** A project in a dropdown: its short name, then the full path when the id is one. */
export function projectOption(id: string): string {
  const name = projectName(id)
  return name === id ? id : `${name} — ${id}`
}

export function scopeText(s: MemScope): string {
  if (s.kind === 'project') return `项目 · ${projectName(s.project ?? '')}`
  if (s.kind === 'platform') return `平台 · ${s.platform}`
  return SCOPE_LABEL.global
}

/** Times are RFC3339 or a bare date; a bare date stays as written, and `""`/null is shown as a dash. */
export function formatTime(value: string | null | undefined): string {
  if (!value) return '—'
  if (/^\d{4}-\d{2}-\d{2}$/.test(value)) return value
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString('zh-CN', { hour12: false })
}

/** How long before `now` an RFC3339 time was, in its largest whole unit; under a minute (or ahead) is 刚刚. */
export function formatAgo(value: string, now = Date.now()): string {
  const minutes = Math.floor((now - new Date(value).getTime()) / 60_000)
  if (minutes < 1) return '刚刚'
  if (minutes < 60) return `${minutes} 分钟前`
  if (minutes < 24 * 60) return `${Math.floor(minutes / 60)} 小时前`
  return `${Math.floor(minutes / (24 * 60))} 天前`
}

export const formatNumber = (n: number) => n.toLocaleString('zh-CN')

/** UTF-8 length: the daemon's 4 KB text limit counts bytes. */
export const byteLength = (text: string) => new TextEncoder().encode(text).length

/** Unicode scalar count: the 1024-character instruction limit counts these, not UTF-16 units. */
export const charLength = (text: string) => [...text].length

/** Statuses of import and consolidation runs and their proposals (docs/api.md). */
export const REVIEW_STATUS_LABEL: Record<string, string> = {
  proposing: '提议中',
  proposed: '待审核',
  rejected: '被拒',
  dropped: '未采纳',
  aborted: '已中止',
  failed: '失败',
  applying: '应用中',
  applied: '已应用',
  skipped: '已跳过',
  rolling_back: '回滚中',
  rolled_back: '已回滚',
  partially_rolled_back: '部分回滚',
}

export const ITEM_KIND_LABEL: Record<string, string> = {
  fact: '事实',
  preference: '偏好',
  reference: '参考',
  'session-turn': '会话轮次',
}

export const UNIT_OUTCOME_LABEL: Record<string, string> = {
  new: '新',
  changed: '有变化',
  unchanged: '未变化',
  reconsidered: '重新提议',
  failed: '失败',
}

export const TASK_STATUS_LABEL: Record<string, string> = {
  queued: '排队中',
  running: '处理中',
  indexing: '建索引',
  completed: '完成',
  failed: '失败',
}

export const RETRIEVAL_KIND_LABEL: Record<string, string> = { search: '检索', context: '上下文' }

/** A share such as `retireFraction`, as a percentage. */
export const formatPercent = (n: number) => `${(n * 100).toFixed(1)}%`
