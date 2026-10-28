// What the platform page (docs/design/platform-connect.md) knows beyond the installer's report: how each platform
// looks, what the user does in it after a write, and how the first calls after a write are recognized.
import type { Calls, ConnectOptions, PlatformActivity, PlatformHost } from '../api/types'
import type { Tone } from '../components/ui'

/** A platform's lettermark on its card; a platform the page does not know shows the start of its name. */
const MARK: Record<string, { text: string; className: string }> = {
  codex: { text: 'Cx', className: 'bg-zinc-900 text-white' },
  'claude-code': { text: 'CC', className: 'bg-orange-100 text-orange-800' },
  workbuddy: { text: 'WB', className: 'bg-sky-100 text-sky-800' },
  'deepseek-harness': { text: 'DS', className: 'bg-indigo-100 text-indigo-800' },
}

export const platformMark = (host: PlatformHost) =>
  MARK[host.id] ?? { text: host.label.slice(0, 2), className: 'bg-zinc-100 text-zinc-700' }

export type PlatformState = 'error' | 'missing' | 'available' | 'outdated' | 'silent' | 'connected'

export const STATE_LOOK: Record<PlatformState, { label: string; tone: Tone }> = {
  error: { label: '读取出错', tone: 'red' },
  missing: { label: '未检测到', tone: 'gray' },
  available: { label: '可以接入', tone: 'blue' },
  outdated: { label: '需要更新', tone: 'amber' },
  silent: { label: '等待信任', tone: 'amber' },
  connected: { label: '已接入', tone: 'green' },
}

/** Where a platform stands: its configuration as the installer reads it, then whether it has called in 30 days. */
export function platformState(host: PlatformHost, activity: PlatformActivity | undefined): PlatformState {
  if (host.error) return 'error'
  if (!host.connected) return host.detected ? 'available' : 'missing'
  if (host.upToDate === false) return 'outdated'
  return activity ? 'connected' : 'silent'
}

/** What the wizard starts from: what is installed now, else dialogue capture on and recall off. */
export const defaultOptions = (host: PlatformHost): ConnectOptions =>
  host.options ?? { capture: host.capture, recall: false }

/** `connect` as a terminal command, for those who would rather run it themselves. */
export function connectCommand(host: PlatformHost, options: ConnectOptions): string {
  const flags = [
    ...(host.capture && !options.capture ? ['--no-capture'] : []),
    ...(host.recall && options.recall ? ['--recall'] : []),
  ]
  return ['bash scripts/install.sh connect --host', host.id, '--apply', ...flags].join(' ')
}

/** A home-relative path: `/Users/me/.codex` reads `~/.codex`. */
export const homePath = (path: string) => path.replace(/^\/(?:Users|home)\/[^/]+(?=\/|$)/, '~')

export interface Step {
  title: string
  detail: string
}

/** What only the user can do in the platform after a write, in order. */
export function trustSteps(host: PlatformHost, options: ConnectOptions): Step[] {
  const hooks = (host.capture && options.capture) || (host.recall && options.recall)
  switch (host.id) {
    case 'codex': {
      const names = [
        ...(options.capture ? ['“shared-memory capture”'] : []),
        ...(options.recall ? ['“shared-memory recall”'] : []),
      ].join(' 和 ')
      return [
        { title: '完全退出 Codex，再重新打开', detail: 'CLI 和桌面版共用 ~/.codex，开着的都要重启。' },
        ...(hooks
          ? [{
              title: `信任 hook：${names}`,
              detail:
                '在 Codex 里输入 /hooks（桌面版：设置 → Hooks），逐个信任。Codex 不运行没审核过的 hook；以后重新写入、命令变了，要再信任一次。',
            }]
          : []),
        { title: '确认 MCP 已连接', detail: '输入 /mcp，列表里的 shared-memory 应显示为已连接。' },
      ]
    }
    case 'claude-code':
      return [
        { title: '重启 Claude Code', detail: '关掉正在运行的会话，再重新打开。' },
        ...(hooks
          ? [{
              title: '确认 hook 已加载',
              detail: '输入 /hooks，确认 UserPromptSubmit、Stop 等事件下有 shared-memory 的 hook；提示有外部修改时，确认接受。',
            }]
          : []),
        { title: '确认 MCP 已连接', detail: '输入 /mcp，shared-memory 应显示为已连接。' },
      ]
    case 'workbuddy':
      return [
        { title: '重启 WorkBuddy', detail: '完全退出后重新打开。' },
        {
          title: '信任连接器',
          detail: '打开 连接器 → 自定义连接器，信任 shared-memory，并确认它的 7 个工具都已启用。',
        },
      ]
    case 'deepseek-harness':
      return [
        { title: '重启 DeepSeek Harness，新开一个会话', detail: '只有新会话会加载新的 MCP 服务。' },
        { title: '确认工具已出现', detail: '工具列表里应有 mcp__shared-memory__* 开头的工具。' },
        {
          title: '检查日志上传（可选）',
          detail: '用官方 DeepSeek API 时，dsh 默认会上传会话日志（含工具结果）；记忆必须留在本机的话，在 设置 → 通用 里关掉。',
        },
      ]
    default:
      return [{ title: `重启 ${host.label}`, detail: '按平台的要求信任 shared-memory 的 MCP 服务和 hook。' }]
  }
}

/** What to do in the platform so that the verification sees calls. */
export const verifyPrompt = (host: PlatformHost, options: ConnectOptions) =>
  (host.capture && options.capture) || (host.recall && options.recall)
    ? `在 ${host.label} 里开一个新会话，随便说一句话，等它回复完。`
    : `在 ${host.label} 的新会话里让 agent 查一下共享记忆，比如：“用 search_memories 查一下这个项目的记忆”。`

export interface Check {
  key: 'capture' | 'hookRecall' | 'mcp'
  label: string
  detail: string
  /** Not needed to call the connection working: the MCP tools of a platform that also has hooks. */
  optional: boolean
  /** When the first call of this kind at or after the write arrived, or null. */
  seenAt: string | null
}

/** The calls that show the platform works, and which arrived at or after `since` (ms). Activity times are whole
 * seconds, so a call in the second of `since` counts. */
export function verification(
  host: PlatformHost,
  options: ConnectOptions,
  activity: PlatformActivity | undefined,
  since: number,
): Check[] {
  const after = (calls: Calls | undefined) =>
    calls && Date.parse(calls.lastAt) >= Math.floor(since / 1000) * 1000 ? calls.lastAt : null
  const hooks = (host.capture && options.capture) || (host.recall && options.recall)
  return [
    ...(host.capture && options.capture
      ? [{ key: 'capture' as const, label: '对话抓取', detail: '一轮对话结束后，hook 把它交给 daemon', optional: false }]
      : []),
    ...(host.recall && options.recall
      ? [{ key: 'hookRecall' as const, label: '自动召回', detail: '会话开始和每次提问时，hook 注入记忆', optional: false }]
      : []),
    { key: 'mcp' as const, label: 'MCP 工具', detail: 'agent 主动查询或保存记忆时出现', optional: hooks },
  ].map((check) => ({ ...check, seenAt: after(activity?.[check.key]) }))
}
