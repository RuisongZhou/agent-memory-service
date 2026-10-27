import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { useMemo, type FormEvent } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'
import { api, ApiError } from '../api/client'
import { ErrorNotice } from '../components/ErrorNotice'
import { StatusBadge, TypeBadge } from '../components/memory'
import { Button, Card, FacetOptions, Field, Fields, Input, Muted, RefreshButton, Select, TD, TH } from '../components/ui'
import {
  entityFiltersFromParams,
  entityFiltersToParams,
  entityRequest,
  type EntityFilters,
} from '../lib/filters'
import { formatNumber, formatTime, scopeText } from '../lib/format'

/** How current the entity view is. It is read as stored: only searches and writes bring it up to date. */
export function ProjectionNote({ updatedAt }: { updatedAt: string | null }) {
  return (
    <Muted>{updatedAt ? `实体投影更新于 ${formatTime(updatedAt)}（检索或写入时更新）` : '实体投影尚未建立'}</Muted>
  )
}

function Filters({ filters, types, update }: {
  filters: EntityFilters
  types: { name: string; count: number }[]
  update: (set: Partial<EntityFilters>) => void
}) {
  const facets = useQuery({ queryKey: ['facets'], queryFn: api.facets })
  const searchText = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    update({ text: String(new FormData(e.currentTarget).get('text')) })
  }
  return (
    <Card>
      <div className="grid gap-3 sm:grid-cols-3">
        <Field label="项目">
          <Select value={filters.project} onChange={(e) => update({ project: e.target.value })}>
            <option value="">全部</option>
            {/* The facet counts memories, not entities. */}
            <FacetOptions facets={facets.data?.projects ?? []} selected={filters.project} counts={false} />
          </Select>
        </Field>
        <Field label="类型" hint="数量按上面选的项目统计">
          <Select value={filters.entityType} onChange={(e) => update({ entityType: e.target.value })}>
            <option value="">全部</option>
            <FacetOptions facets={types} selected={filters.entityType} />
          </Select>
        </Field>
        <form key={filters.text} onSubmit={searchText}>
          <Field label="文本包含">
            <Input name="text" defaultValue={filters.text} placeholder="回车筛选" />
          </Field>
        </form>
      </div>
    </Card>
  )
}

export function EntitiesPage() {
  const [params, setParams] = useSearchParams()
  const filters = entityFiltersFromParams(params)
  const update = (set: Partial<EntityFilters>) =>
    setParams(entityFiltersToParams({ ...filters, ...set }), { replace: true })
  const navigate = useNavigate()

  const list = useInfiniteQuery({
    queryKey: ['entities', 'list', entityRequest(filters)],
    queryFn: ({ pageParam }) => api.listEntities(entityRequest(filters, pageParam)),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.nextCursor ?? undefined,
  })
  const rows = useMemo(() => list.data?.pages.flatMap((p) => p.entities) ?? [], [list.data])
  const first = list.data?.pages[0]

  return (
    <div className="space-y-4">
      <Filters filters={filters} types={first?.types ?? []} update={update} />
      {list.error && <ErrorNotice error={list.error} />}
      <Card
        title={`实体${list.data ? `（已显示 ${formatNumber(rows.length)} 个）` : ''}`}
        actions={<RefreshButton busy={list.isFetching} onClick={() => list.refetch()} />}
      >
        <div className="mb-3 space-y-1">
          <p className="text-sm text-zinc-500">Mem0 从记忆里抽取的人、物和概念，按关联的记忆数从多到少。点一行看它关联的记忆。</p>
          {first && <ProjectionNote updatedAt={first.projectionUpdatedAt} />}
        </div>
        {list.isPending ? (
          <Muted>加载中…</Muted>
        ) : rows.length === 0 ? (
          <Muted>没有符合条件的实体。</Muted>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr>
                  <TH>文本</TH>
                  <TH>类型</TH>
                  <TH>范围</TH>
                  <TH className="text-right">关联记忆</TH>
                </tr>
              </thead>
              <tbody>
                {rows.map((e) => (
                  <tr
                    key={e.id}
                    className="cursor-pointer border-t border-zinc-100 hover:bg-zinc-50"
                    onClick={(ev) => {
                      if (!(ev.target as HTMLElement).closest('a')) navigate(`/entities/${e.id}`)
                    }}
                  >
                    <TD className="max-w-md truncate">
                      <Link to={`/entities/${e.id}`} title={e.text} className="hover:underline">{e.text}</Link>
                    </TD>
                    <TD className="whitespace-nowrap">{e.entityType}</TD>
                    <TD className="whitespace-nowrap">{scopeText(e.scope)}</TD>
                    <TD className="text-right tabular-nums">{formatNumber(e.memoryCount)}</TD>
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

export function EntityPage() {
  const { id } = useParams() as { id: string }
  const query = useQuery({ queryKey: ['entities', 'detail', id], queryFn: () => api.entity(id) })

  if (query.error) {
    return (
      <div className="space-y-3">
        <ErrorNotice error={query.error} />
        {query.error instanceof ApiError && query.error.status === 404 && (
          <Muted>
            这个实体已不在投影里：投影重算时，实体可能被合并或移除。
            <Link to="/entities" className="ml-1 text-sky-700 hover:underline">返回实体列表</Link>
          </Muted>
        )}
      </div>
    )
  }
  if (!query.data) return <Muted>加载中…</Muted>
  const { entity, memories, projectionUpdatedAt } = query.data
  const gone = entity.memoryCount - memories.length

  return (
    <div className="space-y-4">
      <Card
        title={entity.text}
        actions={<RefreshButton busy={query.isFetching} onClick={() => query.refetch()} />}
      >
        <Fields
          rows={[
            ['类型', entity.entityType],
            ['规范化文本', entity.normalizedText],
            ['范围', scopeText(entity.scope)],
            ['关联记忆数', formatNumber(entity.memoryCount)],
            ['实体 id', <span className="font-mono">{entity.id}</span>],
          ]}
        />
        <div className="mt-3">
          <ProjectionNote updatedAt={projectionUpdatedAt} />
        </div>
      </Card>
      <Card title={`关联的记忆（${formatNumber(memories.length)}）`}>
        {gone > 0 && (
          <p className="mb-3 text-sm text-zinc-500">
            投影记着 {formatNumber(entity.memoryCount)} 条，其中 {formatNumber(gone)} 条已不存在，下次检索或写入后投影会更新。
          </p>
        )}
        {memories.length === 0 ? (
          <Muted>没有关联的记忆。</Muted>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr>
                  <TH>内容</TH>
                  <TH>类型</TH>
                  <TH>状态</TH>
                  <TH>范围</TH>
                  <TH>记录时间</TH>
                </tr>
              </thead>
              <tbody>
                {memories.map((m) => (
                  <tr key={m.id} className="border-t border-zinc-100 hover:bg-zinc-50">
                    <TD className="max-w-md truncate">
                      <Link to={`/memories/${m.id}`} title={m.text} className="hover:underline">{m.text}</Link>
                    </TD>
                    <TD><TypeBadge type={m.type} /></TD>
                    <TD><StatusBadge status={m.status} /></TD>
                    <TD className="whitespace-nowrap">{scopeText(m.scope)}</TD>
                    <TD className="whitespace-nowrap">{formatTime(m.recordedAt)}</TD>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>
    </div>
  )
}

/** The memory page's chips: the entities the projection links to this record. */
export function MemoryEntities({ id }: { id: string }) {
  const query = useQuery({ queryKey: ['entities', 'memory', id], queryFn: () => api.memoryEntities(id) })
  const data = query.data
  return (
    <Card title="实体" actions={<RefreshButton busy={query.isFetching} onClick={() => query.refetch()} />}>
      {query.error && <ErrorNotice error={query.error} />}
      {query.isPending && !query.error && <Muted>加载中…</Muted>}
      {data && (
        <div className="space-y-2">
          {data.entities.length === 0 ? (
            <Muted>没有关联的实体。</Muted>
          ) : (
            <ul className="flex flex-wrap gap-2">
              {data.entities.map((e) => (
                <li key={e.id}>
                  <Link
                    to={`/entities/${e.id}`}
                    title={`${e.entityType} · 关联 ${formatNumber(e.memoryCount)} 条记忆`}
                    className="inline-flex items-baseline gap-1 rounded-md bg-zinc-100 px-2 py-0.5 text-sm text-zinc-800 hover:bg-zinc-200"
                  >
                    {e.text}
                    <span className="text-xs text-zinc-500">{e.entityType}</span>
                  </Link>
                </li>
              ))}
            </ul>
          )}
          <ProjectionNote updatedAt={data.projectionUpdatedAt} />
        </div>
      )}
    </Card>
  )
}
