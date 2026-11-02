import { keepPreviousData, useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { createColumnHelper, flexRender, getCoreRowModel, useReactTable } from '@tanstack/react-table'
import { useMemo, type ReactNode } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { api } from '../api/client'
import type { ListedMemory } from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { Icon } from '../components/icons'
import { StatusBadge, TypeBadge } from '../components/memory'
import {
  Button,
  Card,
  Chip,
  cx,
  Empty,
  FacetOptions,
  LiveSearch,
  Loading,
  RefreshButton,
  ScopeText,
  Select,
  TH,
  TimeAgo,
} from '../components/ui'
import {
  ACTIVE_STATUSES,
  ALL_STATUSES,
  filtersFromParams,
  filtersToParams,
  listRequest,
  MEMORY_TYPES,
  NO_FILTERS,
  RECALL_FILTERS,
  SCOPE_KINDS,
  type MemoryFilters,
} from '../lib/filters'
import {
  formatNumber,
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
      <Link
        to={`/memories/${c.row.original.id}`}
        title={c.getValue()}
        className="block max-w-[22rem] truncate font-medium text-zinc-900 group-hover:text-brand-700"
      >
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
  column.accessor('source.platform', {
    header: '写入平台 · 来源',
    cell: (c) => (
      <>
        {c.getValue()}
        <span className="text-zinc-400"> · {ORIGIN_LABEL[c.row.original.source.origin] ?? c.row.original.source.origin}</span>
      </>
    ),
  }),
  column.accessor('recordedAt', { header: '记录时间', cell: (c) => <TimeAgo value={c.getValue()} /> }),
  column.accessor('lastRecalledAt', {
    header: '最近召回',
    cell: (c) =>
      c.getValue() ? (
        <>
          <TimeAgo value={c.getValue()} />
          <span className="text-zinc-400"> · {formatNumber(c.row.original.recallCount)} 次</span>
        </>
      ) : (
        <span className="text-zinc-400">从未</span>
      ),
  }),
]

/** A filter dropdown labelled by its "all" option; one that filters stands out. */
function FilterSelect({ label, value, onChange, children }: {
  label: string
  value: string
  onChange: (value: string) => void
  children: ReactNode
}) {
  return (
    <Select
      aria-label={label}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className={cx('w-full', value && 'border-brand-200 bg-brand-50 text-brand-700')}
    >
      <option value="">全部{label}</option>
      {children}
    </Select>
  )
}

function Filters({ filters, update }: { filters: MemoryFilters; update: (set: Partial<MemoryFilters>) => void }) {
  const facets = useQuery({ queryKey: ['facets'], queryFn: api.facets })
  const filtered = filtersToParams(filters).size > 0

  return (
    <Card className="space-y-3">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-3">
        <LiveSearch
          value={filters.text}
          onCommit={(text) => update({ text })}
          placeholder="搜索记忆内容"
          aria-label="文本包含"
          className="w-full lg:w-80"
        />
        <Statuses filters={filters} update={update} />
        {filtered && (
          <Button variant="ghost" size="sm" className="ml-auto text-brand-700" onClick={() => update(NO_FILTERS)}>
            <Icon name="x" className="size-3.5" />
            清除筛选
          </Button>
        )}
      </div>
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 xl:grid-cols-6">
        <FilterSelect label="项目" value={filters.project} onChange={(project) => update({ project })}>
          <FacetOptions facets={facets.data?.projects ?? []} selected={filters.project} show={projectOption} />
        </FilterSelect>
        <FilterSelect label="类型" value={filters.type} onChange={(type) => update({ type: type as MemoryFilters['type'] })}>
          {MEMORY_TYPES.map((t) => (
            <option key={t} value={t}>{labelled(TYPE_LABEL, t)}</option>
          ))}
        </FilterSelect>
        <FilterSelect
          label="范围"
          value={filters.scopeKind}
          onChange={(scopeKind) => update({ scopeKind: scopeKind as MemoryFilters['scopeKind'] })}
        >
          {SCOPE_KINDS.map((k) => (
            <option key={k} value={k}>{labelled(SCOPE_LABEL, k)}</option>
          ))}
        </FilterSelect>
        <FilterSelect label="写入平台" value={filters.sourcePlatform} onChange={(sourcePlatform) => update({ sourcePlatform })}>
          <FacetOptions facets={facets.data?.sourcePlatforms ?? []} selected={filters.sourcePlatform} />
        </FilterSelect>
        <FilterSelect label="来源" value={filters.origin} onChange={(origin) => update({ origin })}>
          <FacetOptions facets={facets.data?.origins ?? []} selected={filters.origin} labels={ORIGIN_LABEL} />
        </FilterSelect>
        <FilterSelect
          label="召回情况"
          value={filters.recall}
          onChange={(recall) => update({ recall: recall as MemoryFilters['recall'] })}
        >
          {RECALL_FILTERS.map((r) => (
            <option key={r} value={r}>{RECALL_FILTER_LABEL[r]}</option>
          ))}
        </FilterSelect>
      </div>
    </Card>
  )
}

/** The status toggles: at least one stays on, and one button switches between active only and every status. */
function Statuses({ filters, update }: { filters: MemoryFilters; update: (set: Partial<MemoryFilters>) => void }) {
  const toggle = (status: (typeof ALL_STATUSES)[number], on: boolean) =>
    update({ statuses: ALL_STATUSES.filter((s) => (s === status ? on : filters.statuses.includes(s))) })
  const everyStatus = filters.statuses.length === ALL_STATUSES.length
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      <span className="mr-1 text-xs font-medium text-zinc-500">状态</span>
      {ALL_STATUSES.map((s) => {
        const on = filters.statuses.includes(s)
        return (
          <Chip
            key={s}
            label={STATUS_LABEL[s]}
            checked={on}
            // At least one status stays selected; an empty list would mean "the default" to the daemon.
            disabled={on && filters.statuses.length === 1}
            onChange={(e) => toggle(s, e.target.checked)}
          />
        )
      })}
      <Button variant="ghost" size="sm" onClick={() => update({ statuses: everyStatus ? ACTIVE_STATUSES : ALL_STATUSES })}>
        {everyStatus ? '只看活跃' : '全部状态'}
      </Button>
    </div>
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
    // A new filter keeps the old rows on screen until its own arrive, instead of flashing an empty list.
    placeholderData: keepPreviousData,
  })
  const rows = useMemo(() => list.data?.pages.flatMap((p) => p.memories) ?? [], [list.data])
  const table = useReactTable({ data: rows, columns, getCoreRowModel: getCoreRowModel() })
  const total = list.data?.pages[0].total

  return (
    <div className="space-y-4">
      <Filters filters={filters} update={update} />
      {list.error && <ErrorNotice error={list.error} />}
      <Card
        title={
          total === undefined ? (
            '记忆'
          ) : (
            <>
              共 {formatNumber(total)} 条
              {rows.length < total && (
                <span className="ml-2 text-sm font-normal text-zinc-500">已显示 {formatNumber(rows.length)} 条</span>
              )}
            </>
          )
        }
        description={
          <>
            按记录时间从新到旧。最近召回是 agent 调用 <code>search_memories</code> 或 <code>get_context</code>{' '}
            取回这条记忆的时间和次数，不含网页自己的调用。
          </>
        }
        actions={<RefreshButton busy={list.isFetching} onClick={() => list.refetch()} />}
      >
        {list.isPending ? (
          <Loading />
        ) : rows.length === 0 ? (
          <Empty
            action={
              filtersToParams(filters).size > 0 && (
                <Button variant="secondary" onClick={() => update(NO_FILTERS)}>清除筛选</Button>
              )
            }
          >
            没有符合条件的记忆。
          </Empty>
        ) : (
          <div className={cx('-mx-4 overflow-x-auto sm:-mx-5', list.isPlaceholderData && 'opacity-60 transition-opacity')}>
            <table className="w-full text-sm">
              <thead>
                {table.getHeaderGroups().map((g) => (
                  <tr key={g.id}>
                    {g.headers.map((h) => (
                      <TH key={h.id} className="first:pl-4 last:pr-4 sm:first:pl-5 sm:last:pr-5">
                        {flexRender(h.column.columnDef.header, h.getContext())}
                      </TH>
                    ))}
                  </tr>
                ))}
              </thead>
              <tbody className="divide-y divide-zinc-100">
                {table.getRowModel().rows.map((r) => (
                  <tr
                    key={r.id}
                    className="group cursor-pointer transition-colors hover:bg-brand-50/40"
                    onClick={(e) => {
                      if (!(e.target as HTMLElement).closest('a')) navigate(`/memories/${r.original.id}`)
                    }}
                  >
                    {r.getVisibleCells().map((c) => (
                      <td key={c.id} className="px-3 py-2.5 whitespace-nowrap text-zinc-700 first:pl-4 last:pr-4 sm:first:pl-5 sm:last:pr-5">
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
          <div className="mt-4 text-center">
            <Button variant="secondary" disabled={list.isFetchingNextPage} onClick={() => list.fetchNextPage()}>
              {list.isFetchingNextPage ? '加载中…' : '加载更多'}
            </Button>
          </div>
        )}
      </Card>
    </div>
  )
}
