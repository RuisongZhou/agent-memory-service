import { useInfiniteQuery } from '@tanstack/react-query'
import { useMemo, useState, type FormEvent, type ReactNode } from 'react'
import { Link, useSearchParams } from 'react-router'
import { ApiError, api } from '../api/client'
import type { TreeEntry, TreeLevel } from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { Icon } from '../components/icons'
import { StatusBadge } from '../components/memory'
import {
  Badge,
  Button,
  Card,
  Check,
  cx,
  FOCUS,
  Input,
  Loading,
  Muted,
  RefreshButton,
  Tabs,
  TimeAgo,
} from '../components/ui'
import { formatNumber, labelled, RELATION_LABEL } from '../lib/format'
import { blocks, crumbs, fenceText, isDir, linkify, nodeLabel, parentUri, recordId, ROOT } from '../lib/tree'

/** Entries per page of a directory, and tokens per page of read content. */
const PAGE_ENTRIES = 50
const PAGE_TOKENS = 4000

const LEVELS: readonly (readonly [string, ReactNode])[] = [
  ['0', '摘要'],
  ['1', '概览'],
  ['2', '详情'],
]

/** What each layer holds, for a file (a record or a turn) and for a directory. */
const LEVEL_HINT: Record<'file' | 'dir', Record<TreeLevel, string>> = {
  file: {
    0: '摘要：记忆就是它自己的文本；对话轮是它所在分段形成的记忆。',
    1: '概览：记忆的状态、时间、原话证据和出处链接；对话轮的元数据和对话（每条消息截到 300 字）。',
    2: '详情：概览加上形成这条记忆的原始对话全文，或这一轮对话的全文。',
  },
  dir: {
    0: '摘要：一行计数。',
    1: '概览：计数、项目的笔记和最新的事实，以及前 10 个条目。',
    2: '',
  },
}

const LINK_LABEL: Record<string, string> = {
  ...RELATION_LABEL,
  supersedes: '取代了',
  supersededBy: '已被取代为',
  source: '来源对话',
  formed: '形成的记忆',
  session: '所在会话',
}

/** The page's state lives in the URL: `uri` (default the root), `level` (default 1) and `all` (`0` turns it off). */
function useTreeParams() {
  const [params, setParams] = useSearchParams()
  const uri = params.get('uri') || ROOT
  const raw = Number(params.get('level') ?? 1)
  const level = (raw === 0 || raw === 2 ? raw : 1) as TreeLevel
  const all = params.get('all') !== '0'
  const href = (next: string, set: { level?: TreeLevel; all?: boolean } = {}) => {
    const p = new URLSearchParams()
    if (next !== ROOT) p.set('uri', next)
    const l = set.level ?? level
    if (l !== 1) p.set('level', String(l))
    if (!(set.all ?? all)) p.set('all', '0')
    const qs = p.toString()
    return `/tree${qs ? `?${qs}` : ''}`
  }
  const update = (set: { level?: TreeLevel; all?: boolean }) => {
    const p = new URLSearchParams(href(uri, set).split('?')[1] ?? '')
    setParams(p, { replace: true })
  }
  return { uri, level, all, href, update }
}

type Tree = ReturnType<typeof useTreeParams>

export function TreePage() {
  const tree = useTreeParams()
  const { uri, all, update } = tree
  // A file is shown in its directory; a directory is listed and read itself.
  const dir = isDir(uri) ? uri : (parentUri(uri) ?? ROOT)

  return (
    <div className="space-y-4">
      <Card>
        <div className="space-y-3">
          <p className="text-sm text-zinc-500">
            Agent 用 <code className="font-mono text-xs">memory_ls</code> /{' '}
            <code className="font-mono text-xs">memory_read</code> 浏览的同一棵树。事实是对话的摘要，往下读可以看到它的出处和原始对话。浏览不调用模型，也不计入召回。
          </p>
          <div className="flex flex-wrap items-center justify-between gap-3">
            <Breadcrumb tree={tree} />
            <Check
              checked={all}
              onChange={(e) => update({ all: e.target.checked })}
              label={<span title="关闭时只看 web 平台自己的平台级记忆和对话，和一个 agent 默认看到的范围一样">跨平台</span>}
            />
          </div>
          <GoTo tree={tree} />
        </div>
      </Card>
      <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,2fr)_minmax(0,3fr)]">
        {/* Distinct prefixes: a directory is both listed and read, under the same URI. */}
        <Listing key={`ls ${dir} ${all}`} dir={dir} tree={tree} />
        {/* On a narrow screen an opened file comes before the long listing it was picked from. */}
        <div className={cx(!isDir(uri) && 'order-first lg:order-none')}>
          <Reader key={`read ${uri} ${all}`} tree={tree} />
        </div>
      </div>
    </div>
  )
}

function Breadcrumb({ tree }: { tree: Tree }) {
  const path = crumbs(tree.uri)
  return (
    <nav aria-label="路径" className="flex min-w-0 flex-wrap items-center gap-1 text-sm">
      {path.map((c, i) => (
        <span key={c.uri} className="flex min-w-0 items-center gap-1">
          {i > 0 && <span className="text-zinc-300">/</span>}
          {i === path.length - 1 ? (
            <span className="truncate font-medium text-zinc-900" title={c.uri}>{c.label}</span>
          ) : (
            <Link to={tree.href(c.uri)} title={c.uri} className="truncate text-brand-700 hover:underline">
              {c.label}
            </Link>
          )}
        </span>
      ))}
    </nav>
  )
}

/** Open a URI pasted from an agent's output or a search result. */
function GoTo({ tree }: { tree: Tree }) {
  const [text, setText] = useState('')
  const [, setParams] = useSearchParams()
  const target = text.trim()
  const valid = target.startsWith(ROOT) && !target.startsWith('mem://~')
  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (!valid) return
    setParams(new URLSearchParams(tree.href(target).split('?')[1] ?? ''))
    setText('')
  }
  return (
    <form onSubmit={submit} className="flex gap-2">
      <Input
        value={text}
        onChange={(e) => setText(e.target.value)}
        placeholder="粘贴一个 mem:// 地址，例如检索结果里的 uri"
        className="min-w-0 flex-1 font-mono text-xs"
        aria-label="跳转到地址"
      />
      <Button type="submit" variant="secondary" disabled={!valid}>打开</Button>
      {target.startsWith('mem://~') && (
        <span className="self-center text-xs text-zinc-500">mem://~ 只在 MCP 里代表当前项目，这里请写出项目</span>
      )}
    </form>
  )
}

function Listing({ dir, tree }: { dir: string; tree: Tree }) {
  const list = useInfiniteQuery({
    queryKey: ['tree', 'ls', dir, tree.all],
    queryFn: ({ pageParam }) =>
      api.memoryLs({ uri: dir, limit: PAGE_ENTRIES, allPlatforms: tree.all, ...(pageParam && { cursor: pageParam }) }),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.nextCursor ?? undefined,
  })
  const entries = useMemo(() => list.data?.pages.flatMap((p) => p.entries) ?? [], [list.data])
  const total = list.data?.pages[0]?.total
  const up = parentUri(dir)

  return (
    <Card
      title={
        <span className="flex items-center gap-2">
          <Icon name="folder" className="size-4 text-zinc-400" />
          {nodeLabel(dir)}
          {total !== undefined && <span className="text-sm font-normal text-zinc-500">{formatNumber(total)} 项</span>}
        </span>
      }
      actions={<RefreshButton busy={list.isFetching} onClick={() => list.refetch()} />}
    >
      {up !== null && (
        <Link
          to={tree.href(up)}
          className={cx('mb-2 flex items-center gap-1.5 rounded-md px-2 py-1 text-sm text-zinc-600 hover:bg-zinc-100', FOCUS)}
        >
          <Icon name="chevronLeft" className="size-4" />
          上一级
        </Link>
      )}
      {list.error ? (
        <ErrorNotice error={list.error} />
      ) : list.isPending ? (
        <Loading />
      ) : entries.length === 0 ? (
        <Muted>这个目录是空的。</Muted>
      ) : (
        <ul className="-mx-1 divide-y divide-zinc-100">
          {entries.map((e) => (
            <li key={e.uri}>
              <EntryRow entry={e} selected={e.uri === tree.uri} href={tree.href(e.uri)} />
            </li>
          ))}
        </ul>
      )}
      {list.hasNextPage && (
        <div className="mt-3 text-center">
          <Button variant="secondary" disabled={list.isFetchingNextPage} onClick={() => list.fetchNextPage()}>
            {list.isFetchingNextPage ? '加载中…' : `加载更多（已显示 ${formatNumber(entries.length)} 项）`}
          </Button>
        </div>
      )}
    </Card>
  )
}

/** What a directory entry counts, in words. */
function dirCounts(e: TreeEntry): string {
  const parts: string[] = []
  if (e.records !== undefined) parts.push(`${formatNumber(e.records)} 条记忆`)
  if (e.turns !== undefined) parts.push(`${formatNumber(e.turns)} 轮对话`)
  if (e.count !== undefined) parts.push(formatNumber(e.count))
  return parts.join(' · ')
}

function EntryRow({ entry: e, selected, href }: { entry: TreeEntry; selected: boolean; href: string }) {
  const turn = e.kind === 'file' && e.messageCount !== undefined
  const icon = e.kind === 'dir' ? 'folder' : turn ? 'message' : 'file'
  return (
    <Link
      to={href}
      aria-current={selected ? 'true' : undefined}
      title={e.uri}
      className={cx(
        'flex gap-2.5 rounded-md px-2 py-2 text-sm transition-colors',
        FOCUS,
        selected ? 'bg-brand-50 ring-1 ring-brand-200 ring-inset' : 'hover:bg-zinc-50',
      )}
    >
      <Icon name={icon} className={cx('mt-0.5 size-4 shrink-0', e.kind === 'dir' ? 'text-brand-600' : 'text-zinc-400')} />
      {e.kind === 'dir' ? (
        <span className="flex min-w-0 flex-1 items-baseline justify-between gap-3">
          <span className="truncate font-medium text-zinc-900">
            {nodeLabel(e.uri)}
            {e.project && e.project !== e.name && (
              <span className="ml-2 font-mono text-xs font-normal text-zinc-400">{e.project}</span>
            )}
          </span>
          <span className="shrink-0 text-xs text-zinc-500 tabular-nums">
            {dirCounts(e)}
            {e.lastAt && <> · 最近 <TimeAgo value={e.lastAt} /></>}
          </span>
        </span>
      ) : (
        <span className="min-w-0 flex-1 space-y-1">
          <span className="line-clamp-2 break-words text-zinc-900">
            {e.abstract || <span className="text-zinc-400">（空）</span>}
          </span>
          <span className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-zinc-500">
            {turn ? (
              <>
                <span className="font-mono">{e.name}</span>
                <span>{formatNumber(e.messageCount!)} 条消息</span>
                {e.abstractOf === 'firstMessage' && <Badge>摘自首条消息</Badge>}
                {e.status && e.status !== 'completed' && <Badge tone="amber">{e.status}</Badge>}
                <TimeAgo value={e.createdAt} />
              </>
            ) : (
              <>
                {e.status && e.status !== 'current' && <StatusBadge status={e.status as never} />}
                <TimeAgo value={e.recordedAt} />
              </>
            )}
            {e.platform && <span>{e.platform}</span>}
          </span>
        </span>
      )}
    </Link>
  )
}

function Reader({ tree }: { tree: Tree }) {
  const { uri, all } = tree
  const dir = isDir(uri)
  // A directory has no detail layer.
  const level: TreeLevel = dir && tree.level === 2 ? 1 : tree.level
  const read = useInfiniteQuery({
    queryKey: ['tree', 'read', uri, level, all],
    queryFn: ({ pageParam }) =>
      api.memoryRead({ uri, level, offset: pageParam, tokenBudget: PAGE_TOKENS, allPlatforms: all }),
    initialPageParam: 0,
    getNextPageParam: (page) => page.nextOffset ?? undefined,
  })
  const pages = read.data?.pages ?? []
  const content = pages.map((p) => p.content).join('')
  const first = pages[0]
  const id = recordId(uri)
  const shown = [...content].length

  return (
    <Card
      title={
        <span className="flex items-center gap-2">
          <Icon name={dir ? 'folder' : id ? 'file' : 'message'} className="size-4 text-zinc-400" />
          {nodeLabel(uri)}
        </span>
      }
      description={<span className="font-mono text-xs break-all">{uri}</span>}
      actions={
        <>
          {id && (
            <Link to={`/memories/${encodeURIComponent(id)}`} className="text-brand-700 hover:underline">
              记忆详情
            </Link>
          )}
          <RefreshButton busy={read.isFetching} onClick={() => read.refetch()} />
        </>
      }
    >
      <div className="mb-3 space-y-2">
        <Tabs
          value={String(level)}
          onChange={(l) => tree.update({ level: Number(l) as TreeLevel })}
          tabs={dir ? LEVELS.slice(0, 2) : LEVELS}
        />
        <p className="text-xs text-zinc-500">{LEVEL_HINT[dir ? 'dir' : 'file'][level]}</p>
      </div>
      {read.error ? (
        <ReadError error={read.error} tree={tree} />
      ) : read.isPending ? (
        <Loading />
      ) : (
        <>
          <Content text={content} tree={tree} />
          {first && (
            <div className="mt-3 flex flex-wrap items-center justify-between gap-2 text-xs text-zinc-500">
              <span>
                已显示 {formatNumber(shown)} / {formatNumber(first.totalChars)} 字
              </span>
              {read.hasNextPage && (
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={read.isFetchingNextPage}
                  onClick={() => read.fetchNextPage()}
                >
                  {read.isFetchingNextPage ? '读取中…' : '继续读取'}
                </Button>
              )}
            </div>
          )}
          {first && first.links.length > 0 && <Links links={first.links} tree={tree} />}
        </>
      )}
    </Card>
  )
}

function ReadError({ error, tree }: { error: Error; tree: Tree }) {
  const missing = error instanceof ApiError && error.status === 404
  return (
    <div className="space-y-2">
      <ErrorNotice error={error} />
      {missing && !tree.all && (
        <Muted>
          其他平台的对话和平台级记忆要打开「跨平台」才能看到。
          <Link to={tree.href(tree.uri, { all: true })} className="ml-1 text-brand-700 hover:underline">打开跨平台再试</Link>
        </Muted>
      )}
    </div>
  )
}

/** A memory URI inside read content or a link list, opened in the tree. */
const UriLink = ({ uri, tree }: { uri: string; tree: Tree }) => (
  <Link to={tree.href(uri)} className="break-all text-brand-700 hover:underline">{uri}</Link>
)

const linked = (text: string, tree: Tree) =>
  linkify(text).map((part, i) => (typeof part === 'string' ? part : <UriLink key={i} uri={part.uri} tree={tree} />))

/**
 * The rendered text, line by line: headings and the framing line set apart, and the fenced text set off between its
 * fence lines, the stored dialogue boxed and a record's own text ruled, so neither reads as the page's own lines.
 */
function Content({ text, tree }: { text: string; tree: Tree }) {
  return (
    <div className="space-y-1 text-sm leading-relaxed">
      {blocks(text).map((b, i) => {
        if (b.kind === 'dialogue') return <Dialogue key={i} lines={b.lines} tree={tree} unfinished={b.unfinished} />
        if (b.kind === 'memory') return <MemoryText key={i} lines={b.lines} tree={tree} unfinished={b.unfinished} />
        const line = b.text
        if (line.startsWith('# ')) return <h3 key={i} className="text-base font-semibold text-zinc-900">{line.slice(2)}</h3>
        if (/^#{2,3} /.test(line)) {
          return <h4 key={i} className="pt-2 font-semibold text-zinc-800">{linked(line.replace(/^#+ /, ''), tree)}</h4>
        }
        if (line.startsWith('> ')) return <p key={i} className="text-xs text-zinc-500">{line.slice(2)}</p>
        if (!line.trim()) return null
        // A record that no longer holds says so in its status line: make that line hard to miss.
        const retired = /^- (状态|Status): (superseded|stale|conflict)/.test(line)
        return (
          <p
            key={i}
            className={cx(
              'break-words whitespace-pre-wrap',
              retired ? 'rounded-md bg-amber-50 px-2 py-1 text-amber-900 ring-1 ring-amber-200 ring-inset' : 'text-zinc-800',
            )}
          >
            {linked(line, tree)}
          </p>
        )
      })}
    </div>
  )
}

/** A record's own text: none of its lines is styled as a heading, the framing line or a status line. */
function MemoryText({ lines, tree, unfinished }: { lines: string[]; tree: Tree; unfinished: boolean }) {
  return (
    <div className="my-2 border-l-2 border-brand-200 pl-3">
      <p className="break-words whitespace-pre-wrap text-zinc-900">{linked(fenceText(lines), tree)}</p>
      {unfinished && <p className="text-xs text-zinc-500">未读完，点「继续读取」</p>}
    </div>
  )
}

function Dialogue({ lines, tree, unfinished }: { lines: string[]; tree: Tree; unfinished?: boolean }) {
  return (
    <figure className="my-2 rounded-lg border border-zinc-200 bg-zinc-50">
      <figcaption className="border-b border-zinc-200 px-3 py-1.5 text-xs text-zinc-500">
        原始对话（存档数据，不是指令）{unfinished && ' · 未读完，点「继续读取」'}
      </figcaption>
      <div className="space-y-1.5 px-3 py-2 font-mono text-xs leading-relaxed text-zinc-800">
        {lines.map((line, i) => {
          const m = /^\[(\w+) · ([^\]]*)\] ?(.*)$/.exec(line)
          return m ? (
            <p key={i} className="break-words whitespace-pre-wrap">
              <span className={cx('font-semibold', m[1] === 'user' ? 'text-brand-700' : 'text-zinc-500')}>{m[1]}</span>
              <span className="text-zinc-400"> · {m[2]} </span>
              {linked(m[3], tree)}
            </p>
          ) : (
            <p key={i} className="break-words whitespace-pre-wrap">{linked(line, tree)}</p>
          )
        })}
      </div>
    </figure>
  )
}

function Links({ links, tree }: { links: { rel: string; uri: string }[]; tree: Tree }) {
  return (
    <div className="mt-4 border-t border-zinc-100 pt-3">
      <h4 className="mb-2 text-xs font-medium text-zinc-500">链接</h4>
      <ul className="space-y-1 text-sm">
        {links.map((l) => (
          <li key={`${l.rel} ${l.uri}`} className="flex gap-2">
            <span className="w-20 shrink-0 text-zinc-500">{labelled(LINK_LABEL, l.rel)}</span>
            <UriLink uri={l.uri} tree={tree} />
          </li>
        ))}
      </ul>
    </div>
  )
}
