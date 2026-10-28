import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { Fragment, useMemo, useState, type FormEvent } from 'react'
import { useSearchParams } from 'react-router'
import { api, PLATFORM } from '../api/client'
import type { MemorySummaries, Retrieval } from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { MemoryRef } from '../components/memory'
import { Badge, Button, Card, Check, Field, Input, Muted, ProjectName, RefreshButton, Select, TD, TH } from '../components/ui'
import {
  recallFiltersFromParams,
  recallFiltersToParams,
  recallRequest,
  RETRIEVAL_KINDS,
  type RecallFilters,
} from '../lib/filters'
import { CLIENT_LABEL, formatNumber, formatTime, labelled, RETRIEVAL_KIND_LABEL } from '../lib/format'

const RECENT_LIMIT = 10

function Where({ r }: { r: Retrieval }) {
  if (!r.project) return <>{r.session || '—'}</>
  return <><ProjectName id={r.project} />{r.session && ` / ${r.session}`}</>
}

function FeedbackText({ r }: { r: Retrieval }) {
  if (!r.feedback) return <span className="text-zinc-400">无</span>
  return (
    <span title={formatTime(r.feedback.at)}>
      {r.feedback.outcome} · 用上 {r.feedback.used.length}
    </span>
  )
}

/** The memories one call returned, by rank, marking those its feedback says were used. */
function Returned({ r, memories }: { r: Retrieval; memories: MemorySummaries }) {
  const used = new Set(r.feedback?.used ?? [])
  const ranked = [...r.returned.memories].sort((a, b) => a.rank - b.rank)
  return (
    <div className="space-y-2 py-1">
      {ranked.length === 0 ? (
        <Muted>没有返回记忆。</Muted>
      ) : (
        <ul className="space-y-1.5">
          {ranked.map((m) => (
            <li key={m.id} className="flex items-start gap-2">
              <div className="min-w-0 flex-1">
                <MemoryRef id={m.id} memories={memories} rank={m.rank} />
              </div>
              {used.has(m.id) && <Badge tone="green">用上</Badge>}
            </li>
          ))}
        </ul>
      )}
      <p className="text-xs text-zinc-500">
        <span className="font-mono">{r.id}</span>
        {r.latency && ` · 耗时 ${Object.entries(r.latency).map(([k, v]) => `${k} ${v.toFixed(1)} ms`).join('，')}`}
        {r.feedback && r.feedback.applied.length > 0 && ` · 采纳 ${r.feedback.applied.join('、')}`}
      </p>
    </div>
  )
}

function Filters({ filters, update }: { filters: RecallFilters; update: (set: Partial<RecallFilters>) => void }) {
  const submit = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    const form = new FormData(e.currentTarget)
    const text = (name: string) => String(form.get(name)).trim()
    update({ platform: text('platform'), project: text('project'), session: text('session') })
  }
  return (
    <Card>
      <form
        key={`${filters.platform}|${filters.project}|${filters.session}`}
        onSubmit={submit}
        className="grid items-end gap-3 sm:grid-cols-3 lg:grid-cols-5"
      >
        <Field label="类型">
          <Select value={filters.kind} onChange={(e) => update({ kind: e.target.value as RecallFilters['kind'] })}>
            <option value="">全部</option>
            {RETRIEVAL_KINDS.map((k) => (
              <option key={k} value={k}>{labelled(RETRIEVAL_KIND_LABEL, k)}</option>
            ))}
          </Select>
        </Field>
        <Field label="调用平台">
          <Input name="platform" defaultValue={filters.platform} placeholder="全部" />
        </Field>
        <Field label="项目">
          <Input name="project" defaultValue={filters.project} placeholder="全部" />
        </Field>
        <Field label="会话">
          <Input name="session" defaultValue={filters.session} placeholder="全部" />
        </Field>
        <Button type="submit" variant="secondary">筛选</Button>
      </form>
      <div className="mt-3">
        <Check
          label={`显示网页自己的调用（平台 ${PLATFORM}）`}
          checked={filters.showWeb}
          onChange={(e) => update({ showWeb: e.target.checked })}
        />
      </div>
    </Card>
  )
}

export function RecallsPage() {
  const [params, setParams] = useSearchParams()
  const filters = recallFiltersFromParams(params)
  const update = (set: Partial<RecallFilters>) =>
    setParams(recallFiltersToParams({ ...filters, ...set }), { replace: true })
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set())
  const toggle = (id: string) =>
    setExpanded((old) => {
      const next = new Set(old)
      if (!next.delete(id)) next.add(id)
      return next
    })

  const list = useInfiniteQuery({
    queryKey: ['retrievals', recallRequest(filters)],
    queryFn: ({ pageParam }) => api.listRetrievals(recallRequest(filters, pageParam)),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.nextCursor ?? undefined,
  })
  const rows = useMemo(() => list.data?.pages.flatMap((p) => p.retrievals) ?? [], [list.data])
  const memories = useMemo(
    () => Object.assign({}, ...(list.data?.pages.map((p) => p.memories) ?? [])) as MemorySummaries,
    [list.data],
  )

  return (
    <div className="space-y-4">
      <Filters filters={filters} update={update} />
      {list.error && <ErrorNotice error={list.error} />}
      <Card
        title={`召回记录${list.data ? `（已显示 ${formatNumber(rows.length)} 条）` : ''}`}
        actions={<RefreshButton busy={list.isFetching} onClick={() => list.refetch()} />}
      >
        <p className="mb-3 text-sm text-zinc-500">
          每一行是一次 <code>search_memories</code> 或 <code>get_context</code> 调用，按时间从新到旧。点一行看它按名次返回了哪些记忆。
        </p>
        {list.isPending ? (
          <Muted>加载中…</Muted>
        ) : rows.length === 0 ? (
          <Muted>没有符合条件的召回记录。</Muted>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr>
                  <TH>时间</TH>
                  <TH>类型</TH>
                  <TH>平台</TH>
                  <TH>项目 / 会话</TH>
                  <TH>查询</TH>
                  <TH className="text-right">返回</TH>
                  <TH className="text-right">token</TH>
                  <TH>反馈</TH>
                </tr>
              </thead>
              <tbody>
                {rows.map((r) => (
                  <Fragment key={r.id}>
                    <tr
                      className="cursor-pointer border-t border-zinc-100 hover:bg-zinc-50"
                      aria-expanded={expanded.has(r.id)}
                      onClick={() => toggle(r.id)}
                    >
                      <TD className="whitespace-nowrap">
                        <span className="mr-1 inline-block w-3 text-zinc-400">{expanded.has(r.id) ? '▾' : '▸'}</span>
                        {formatTime(r.createdAt)}
                      </TD>
                      <TD className="whitespace-nowrap">{RETRIEVAL_KIND_LABEL[r.kind] ?? r.kind}</TD>
                      <TD className="whitespace-nowrap">
                        {r.platform ?? <span className="text-zinc-400">未知</span>}
                        {r.client && <span className="ml-1 text-xs text-zinc-400">{CLIENT_LABEL[r.client]}</span>}
                      </TD>
                      <TD className="max-w-48 truncate whitespace-nowrap"><Where r={r} /></TD>
                      <TD className="max-w-xs truncate">
                        {r.query ?? <span className="text-zinc-400">—</span>}
                        {r.returned.gap && (
                          <span className="ml-1">
                            <Badge tone="red">无结果</Badge>
                          </span>
                        )}
                      </TD>
                      <TD className="text-right tabular-nums">{r.returned.memories.length}</TD>
                      <TD className="text-right tabular-nums">
                        {r.returned.tokens === undefined ? <span className="text-zinc-400">—</span> : formatNumber(r.returned.tokens)}
                      </TD>
                      <TD className="whitespace-nowrap"><FeedbackText r={r} /></TD>
                    </tr>
                    {expanded.has(r.id) && (
                      <tr className="bg-zinc-50/60">
                        <td colSpan={8} className="px-2 pb-3">
                          <Returned r={r} memories={memories} />
                        </td>
                      </tr>
                    )}
                  </Fragment>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {list.hasNextPage && (
          <div className="mt-3 text-center">
            <Button variant="secondary" disabled={list.isFetchingNextPage} onClick={() => list.fetchNextPage()}>
              {list.isFetchingNextPage ? '加载中…' : '加载更多'}
            </Button>
          </div>
        )}
      </Card>
    </div>
  )
}

/**
 * The memory page's "最近被召回": calls by agents, not the UI's own (`excludePlatforms: ["web"]`), so debugging a
 * search here does not fill it. The daemon scans only the last 30 days for a memory id.
 */
export function RecentRecalls({ id }: { id: string }) {
  const recalls = useQuery({
    queryKey: ['retrievals', 'memory', id],
    queryFn: () => api.listRetrievals({ memoryId: id, excludePlatforms: [PLATFORM], limit: RECENT_LIMIT }),
  })
  const span = recalls.data?.window === '30d' ? '近 30 天' : recalls.data?.window
  const rows = recalls.data?.retrievals ?? []
  return (
    <Card
      title={`最近被召回${span ? `（${span}，不含网页自己的调用）` : ''}`}
      actions={<RefreshButton busy={recalls.isFetching} onClick={() => recalls.refetch()} />}
    >
      {recalls.error && <ErrorNotice error={recalls.error} />}
      {recalls.isPending ? (
        <Muted>加载中…</Muted>
      ) : rows.length === 0 ? (
        <Muted>{span ?? '近 30 天'}没有被网页以外的调用召回。</Muted>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr>
                <TH>时间</TH>
                <TH>类型</TH>
                <TH>平台</TH>
                <TH>项目 / 会话</TH>
                <TH>查询</TH>
                <TH className="text-right">名次</TH>
                <TH>用上</TH>
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => (
                <tr key={r.id} className="border-t border-zinc-100">
                  <TD className="whitespace-nowrap">{formatTime(r.createdAt)}</TD>
                  <TD>{RETRIEVAL_KIND_LABEL[r.kind] ?? r.kind}</TD>
                  <TD>{r.platform ?? <span className="text-zinc-400">未知</span>}</TD>
                  <TD className="max-w-48 truncate"><Where r={r} /></TD>
                  <TD className="max-w-xs truncate">{r.query ?? '—'}</TD>
                  <TD className="text-right tabular-nums">
                    {r.returned.memories.find((m) => m.id === id)?.rank ?? '—'} / {r.returned.memories.length}
                  </TD>
                  <TD>{!r.feedback ? <span className="text-zinc-400">无反馈</span> : r.feedback.used.includes(id) ? '是' : '否'}</TD>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {rows.length === RECENT_LIMIT && <p className="mt-2 text-xs text-zinc-500">只显示最近 {RECENT_LIMIT} 次。</p>}
    </Card>
  )
}
