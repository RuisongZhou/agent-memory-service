import type { Memory, MemoryType } from '../api/types'
import { projectName, TYPE_LABEL } from './format'

/** The memory tree's root (docs/design/memory-fs.md). Directories end with `/`, files do not. */
export const ROOT = 'mem://'

const TYPE_DIR: Record<MemoryType, string> = {
  instruction: 'instructions',
  fact: 'facts',
  experience: 'experiences',
  profile: 'profiles',
  skill: 'skills',
}

/** What a fixed directory name reads as. Project, session and platform names are shown as they are. */
const DIR_LABEL: Record<string, string> = {
  projects: '项目',
  global: '全局',
  platform: '平台',
  sessions: '会话',
  ...Object.fromEntries(Object.entries(TYPE_DIR).map(([type, dir]) => [dir, TYPE_LABEL[type as MemoryType]])),
}

/** Python's `quote(part, safe='')`, as the daemon quotes a segment: `encodeURIComponent` also keeps `!'()*`. */
export const quote = (part: string) =>
  encodeURIComponent(part).replace(/[!'()*]/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`)

/** A quoted segment as text; a malformed one as it is. */
export function unquote(segment: string): string {
  try {
    return decodeURIComponent(segment)
  } catch {
    return segment
  }
}

export const isDir = (uri: string) => uri === ROOT || uri.endsWith('/')

/** The record's URI, in the directory of its scope and type; null for one the tree has no place for. */
export function memoryUri(m: Pick<Memory, 'id' | 'type' | 'scope'>): string | null {
  const dir = TYPE_DIR[m.type]
  const { kind, project, platform } = m.scope
  const space =
    kind === 'global' ? 'global/' : kind === 'project' && project ? `projects/${quote(project)}/`
      : kind === 'platform' && platform ? `platform/${quote(platform)}/` : null
  return dir && space ? `${ROOT}${space}${dir}/${quote(m.id)}` : null
}

const segments = (uri: string) => uri.slice(ROOT.length).replace(/\/$/, '').split('/').filter(Boolean)

/** The directory holding `uri`; the root has none. */
export function parentUri(uri: string): string | null {
  const parts = segments(uri)
  if (!uri.startsWith(ROOT) || parts.length === 0) return null
  return ROOT + parts.slice(0, -1).map((p) => `${p}/`).join('')
}

/** How the last segment of `uri` reads: a fixed directory by its label, a project by its folder name. */
export function nodeLabel(uri: string): string {
  const parts = segments(uri)
  const last = parts.at(-1)
  if (last === undefined) return '根目录'
  const parent = parts.at(-2)
  // A project directory, a session or a platform named like a fixed directory is still its own name.
  if (parent === 'projects') return projectName(unquote(last))
  if (parts.length === 2 && parent === 'platform') return unquote(last)
  if (parent === 'sessions') return unquote(last)
  return DIR_LABEL[last] ?? unquote(last)
}

/** The breadcrumb of `uri`: the root, then each directory down to it. */
export function crumbs(uri: string): { uri: string; label: string }[] {
  const parts = segments(uri)
  const out = [{ uri: ROOT, label: nodeLabel(ROOT) }]
  parts.forEach((_, i) => {
    const file = i === parts.length - 1 && !isDir(uri)
    const at = ROOT + parts.slice(0, i + 1).join('/') + (file ? '' : '/')
    out.push({ uri: at, label: nodeLabel(at) })
  })
  return out
}

/** The record id of a record's URI (`…/{type dir}/{id}`), or null for anything else. */
export function recordId(uri: string): string | null {
  const parts = segments(uri)
  if (isDir(uri) || parts.length < 3 || !Object.values(TYPE_DIR).includes(parts.at(-2)!)) return null
  return unquote(parts.at(-1)!)
}

/** Text split into plain runs and the memory URIs inside it, which the page links. */
export function linkify(text: string): (string | { uri: string })[] {
  const out: (string | { uri: string })[] = []
  let last = 0
  for (const m of text.matchAll(/mem:\/\/[A-Za-z0-9%._~\-/]*/g)) {
    if (m.index > last) out.push(text.slice(last, m.index))
    out.push({ uri: m[0] })
    last = m.index + m[0].length
  }
  if (last < text.length) out.push(text.slice(last))
  return out
}

/** What `memory_read` fences: dialogue (and evidence cut from it), and a record's own text. */
export type Fence = 'dialogue' | 'memory'

export type Block =
  | { kind: 'line'; text: string }
  | { kind: 'dialogue'; lines: string[]; unfinished: boolean }
  | { kind: 'memory'; lines: string[]; unfinished: boolean }

/**
 * Read content as lines and fenced text: dialogue (`<<<dialogue {tag} …>>>` to `<<<end dialogue {tag}>>>`) and a
 * record's own text (`<<<memory {tag} …>>>` to `<<<end memory {tag}>>>`). Only the close of the opening kind and tag
 * ends a fence: the tag hashes the fenced text, so text inside it cannot carry its own close, and a forged one stays
 * inside the box. A page can end inside a fence: that one is `unfinished`.
 */
export function blocks(text: string): Block[] {
  const out: Block[] = []
  let open: { kind: Fence; tag: string; lines: string[] } | null = null
  for (const line of text.split('\n')) {
    if (open) {
      if (line === `<<<end ${open.kind} ${open.tag}>>>`) {
        out.push({ kind: open.kind, lines: open.lines, unfinished: false })
        open = null
      } else {
        open.lines.push(line)
      }
      continue
    }
    const start = /^<<<(dialogue|memory) ([0-9a-f]{12}) .*>>>$/.exec(line)
    if (start) open = { kind: start[1] as Fence, tag: start[2], lines: [] }
    else out.push({ kind: 'line', text: line })
  }
  if (open) out.push({ kind: open.kind, lines: open.lines, unfinished: true })
  return out
}

/** A record text's fence lines as the text: the daemon indents every one of them by two spaces. */
export const fenceText = (lines: string[]) => lines.map((l) => (l.startsWith('  ') ? l.slice(2) : l)).join('\n')
