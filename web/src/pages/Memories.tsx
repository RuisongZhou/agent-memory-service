import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { createColumnHelper, flexRender, getCoreRowModel, useReactTable } from '@tanstack/react-table'
import { useMemo, type FormEvent } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { api } from '../api/client'
import type { ListedMemory } from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { StatusBadge, TypeBadge } from '../components/memory'
import { Button, Card, Check, FacetOptions, Field, Input, Muted, RefreshButton, ScopeText, Select } from '../components/ui'
import {
  ACTIVE_STATUSES,
  ALL_STATUSES,
  filtersFromParams,
  filtersToParams,
  listRequest,
  MEMORY_TYPES,
  RECALL_FILTERS,
  SCOPE_KINDS,
  type MemoryFilters,
} from '../lib/filters'
import {
  formatNumber,
  formatTime,
  labelled,
  ORIGIN_LABEL,
  projectOption,
  RECALL_FILTER_LABEL,
  SCOPE_LABEL,
  scopeText,
  STATUS_LABEL,
  TYPE_LABEL,
} from '../lib/format'

const column = createColumnHelper<ListedMemory>()
const columns = [
  column.accessor('text', {
    header: '内容',
    cell: (c) => (
      <Link to={`/memories/${c.row.original.id}`} title={c.getValue()} className="block max-w-sm truncate hover:underline">
        {c.getValue()}
      </Link>
    ),
  }),
  column.accessor('type', { header: '类型', cell: (c) => <TypeBadge type={c.getValue()} /> }),
  column.accessor('status', { header: '状态', cell: (c) => <StatusBadge status={c.getValue()} /> }),
  column.accessor((m) => scopeText(m.scope), {
    id: 'scope',
    header: '范围',
    cell: (c) => <ScopeText scope={c.row.original.scope} />,
  }),
  column.accessor('source.platform', { header: '写入平台' }),
  column.accessor('source.origin', { header: '来源', cell: (c) => ORIGIN_LABEL[c.getValue()] ?? c.getValue() }),
  column.accessor('recordedAt', { header: '记录时间', cell: (c) => formatTime(c.getValue()) }),
  column.accessor('lastRecalledAt', {
    header: '最近召回',
    cell: (c) =>
      c.getValue() ? (
        <>{formatTime(c.getValue())} · {formatNumber(c.row.original.recallCount)} 次</>
      ) : (
        <span className="text-zinc-400">从未</span>
      ),
  }),
  column.accessor('effectiveAt', { header: '生效时间', cell: (c) => formatTime(c.getValue()) }),
]

function Filters({ filters, update }: { filters: MemoryFilters; update: (set: Partial<MemoryFilters>) => void }) {
  const facets = useQuery({ queryKey: ['facets'], queryFn: api.facets })
  const toggle = (status: (typeof ALL_STATUSES)[number], on: boolean) =>
    update({ statuses: ALL_STATUSES.filter((s) => (s === status ? on : filters.statuses.includes(s))) })
  const searchText = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    update({ text: String(new FormData(e.currentTarget).get('text')) })
  }

  return (
    <Card>
      <div className="grid gap-3 sm:grid-cols-3 lg:grid-cols-4">
        <Field label="项目">
          <Select value={filters.project} onChange={(e) => update({ project: e.target.value })}>
            <option value="">全部</option>
            <FacetOptions facets={facets.data?.projects ?? []} selected={filters.project} show={projectOption} />
          </Select>
        </Field>
        <Field label="范围">
          <Select value={filters.scopeKind} onChange={(e) => update({ scopeKind: e.target.value as MemoryFilters['scopeKind'] })}>
            <option value="">全部</option>
            {SCOPE_KINDS.map((k) => (
              <option key={k} value={k}>{labelled(SCOPE_LABEL, k)}</option>
            ))}
          </Select>
        </Field>
        <Field label="写入平台">
          <Select value={filters.sourcePlatform} onChange={(e) => update({ sourcePlatform: e.target.value })}>
            <option value="">全部</option>
            <FacetOptions facets={facets.data?.sourcePlatforms ?? []} selected={filters.sourcePlatform} />
          </Select>
        </Field>
        <Field label="类型">
          <Select value={filters.type} onChange={(e) => update({ type: e.target.value as MemoryFilters['type'] })}>
            <option value="">全部</option>
            {MEMORY_TYPES.map((t) => (
              <option key={t} value={t}>{labelled(TYPE_LABEL, t)}</option>
            ))}
          </Select>
        </Field>
        <Field label="来源">
          <Select value={filters.origin} onChange={(e) => update({ origin: e.target.value })}>
            <option value="">全部</option>
            <FacetOptions facets={facets.data?.origins ?? []} selected={filters.origin} labels={ORIGIN_LABEL} />
          </Select>
        </Field>
        <Field label="召回">
          <Select value={filters.recall} onChange={(e) => update({ recall: e.target.value as MemoryFilters['recall'] })}>
            <option value="">全部</option>
            {RECALL_FILTERS.map((r) => (
              <option key={r} value={r}>{RECALL_FILTER_LABEL[r]}</option>
            ))}
          </Select>
        </Field>
        <form key={filters.text} onSubmit={searchText}>
          <Field label="文本包含">
            <Input name="text" defaultValue={filters.text} placeholder="回车筛选" />
          </Field>
        </form>
      </div>
      <div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-2">
        <span className="text-sm font-medium text-zinc-700">状态</span>
        {ALL_STATUSES.map((s) => {
          const on = filters.statuses.includes(s)
          return (
            <Check
              key={s}
              label={labelled(STATUS_LABEL, s)}
              checked={on}
              // At least one status stays selected; an empty list would mean "the default" to the daemon.
              disabled={on && filters.statuses.length === 1}
              onChange={(e) => toggle(s, e.target.checked)}
            />
          )
        })}
        <Button variant="secondary" onClick={() => update({ statuses: ACTIVE_STATUSES })}>只看活跃</Button>
        <Button variant="secondary" onClick={() => update({ statuses: ALL_STATUSES })}>含已取代和已失效</Button>
      </div>
    </Card>
  )
}

export function MemoriesPage() {
  const [params, setParams] = useSearchParams()
  const filters = filtersFromParams(params)
  const update = (set: Partial<MemoryFilters>) => setParams(filtersToParams({ ...filters, ...set }), { replace: true })
  const navigate = useNavigate()

  const list = useInfiniteQuery({
    queryKey: ['memories', listRequest(filters)],
    queryFn: ({ pageParam }) => api.listMemories(listRequest(filters, pageParam)),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.nextCursor ?? undefined,
  })
  const rows = useMemo(() => list.data?.pages.flatMap((p) => p.memories) ?? [], [list.data])
  const table = useReactTable({ data: rows, columns, getCoreRowModel: getCoreRowModel() })
  const total = list.data?.pages[0].total

  return (
    <div className="space-y-4">
      <Filters filters={filters} update={update} />
      {list.error && <ErrorNotice error={list.error} />}
      <Card
        title={total === undefined ? '记忆' : `共 ${formatNumber(total)} 条，已显示 ${formatNumber(rows.length)} 条`}
        actions={<RefreshButton busy={list.isFetching} onClick={() => list.refetch()} />}
      >
        <p className="mb-3 text-sm text-zinc-500">
          按记录时间从新到旧。最近召回是 agent 调用 <code>search_memories</code> 或 <code>get_context</code> 取回这条记忆的时间和次数，不含网页自己的调用。
        </p>
        {list.isPending ? (
          <Muted>加载中…</Muted>
        ) : rows.length === 0 ? (
          <Muted>没有符合条件的记忆。</Muted>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                {table.getHeaderGroups().map((g) => (
                  <tr key={g.id} className="text-left text-zinc-500">
                    {g.headers.map((h) => (
                      <th key={h.id} className="px-2 py-1.5 font-normal whitespace-nowrap">
                        {flexRender(h.column.columnDef.header, h.getContext())}
                      </th>
                    ))}
                  </tr>
                ))}
              </thead>
              <tbody>
                {table.getRowModel().rows.map((r) => (
                  <tr
                    key={r.id}
                    className="cursor-pointer border-t border-zinc-100 hover:bg-zinc-50"
                    onClick={(e) => {
                      if (!(e.target as HTMLElement).closest('a')) navigate(`/memories/${r.original.id}`)
                    }}
                  >
                    {r.getVisibleCells().map((c) => (
                      <td key={c.id} className="px-2 py-1.5 whitespace-nowrap">
                        {flexRender(c.column.columnDef.cell, c.getContext())}
                      </td>
                    ))}
                  </tr>
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
