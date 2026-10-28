import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { Fragment, useMemo, useState, type FormEvent, type ReactNode } from 'react'
import { Link, useParams, useSearchParams } from 'react-router'
import { api } from '../api/client'
import type { Extraction, ModelResponse, RequestDetail } from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { ReviewStatusBadge } from '../components/memory'
import {
  Badge,
  Button,
  Card,
  Check,
  Field,
  Fields,
  Input,
  MemoryLink,
  Muted,
  Pre,
  ProjectName,
  RefreshButton,
  Select,
  TD,
  TH,
} from '../components/ui'
import {
  REQUEST_KINDS,
  responsesRequest,
  TASK_STATUSES,
  taskFiltersFromParams,
  taskFiltersToParams,
  taskRequest,
  type TaskFilters,
} from '../lib/filters'
import { EVIDENCE_ROLE_LABEL, formatNumber, formatTime, labelled, TASK_STATUS_LABEL } from '../lib/format'
import { responseFailed } from '../lib/review'

const REQUEST_KIND_LABEL: Record<string, string> = { mem0: '对话 / 事实写入', instruction: '指令' }

const json = (value: unknown) => JSON.stringify(value, null, 2)

const ids = (list: string[]) =>
  list.length ? (
    <span className="flex flex-wrap gap-x-3">
      {list.map((id) => <MemoryLink key={id} id={id} />)}
    </span>
  ) : (
    '—'
  )

const Tab = ({ active, onClick, children }: { active: boolean; onClick: () => void; children: ReactNode }) => (
  <button
    type="button"
    role="tab"
    aria-selected={active}
    onClick={onClick}
    className={`px-3 py-1.5 text-sm ${active ? 'bg-zinc-900 text-white' : 'bg-white text-zinc-700 hover:bg-zinc-50'}`}
  >
    {children}
  </button>
)

export function TasksPage() {
  const [params, setParams] = useSearchParams()
  const filters = taskFiltersFromParams(params)
  const update = (set: Partial<TaskFilters>) => setParams(taskFiltersToParams({ ...filters, ...set }), { replace: true })
  return (
    <div className="space-y-4">
      <div role="tablist" className="inline-flex overflow-hidden rounded-md border border-zinc-300">
        <Tab active={filters.tab === 'requests'} onClick={() => update({ tab: 'requests' })}>写入请求</Tab>
        <Tab active={filters.tab === 'responses'} onClick={() => update({ tab: 'responses' })}>模型调用</Tab>
      </div>
      {filters.tab === 'requests' ? (
        <Requests filters={filters} update={update} />
      ) : (
        <Responses filters={filters} update={update} />
      )}
    </div>
  )
}

// ---------------------------------------------------------------- requests

interface TabProps {
  filters: TaskFilters
  update: (set: Partial<TaskFilters>) => void
}

function Requests({ filters, update }: TabProps) {
  const list = useInfiniteQuery({
    queryKey: ['requests', 'list', taskRequest(filters)],
    queryFn: ({ pageParam }) => api.listRequests(taskRequest(filters, pageParam)),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.nextCursor ?? undefined,
  })
  const rows = useMemo(() => list.data?.pages.flatMap((p) => p.requests) ?? [], [list.data])
  const submit = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    const form = new FormData(e.currentTarget)
    update({ platform: String(form.get('platform')).trim(), project: String(form.get('project')).trim() })
  }
  const toggle = (status: (typeof TASK_STATUSES)[number], on: boolean) =>
    update({ statuses: TASK_STATUSES.filter((s) => (s === status ? on : filters.statuses.includes(s))) })

  return (
    <>
      <Card>
        <form key={`${filters.platform}|${filters.project}`} onSubmit={submit} className="grid items-end gap-3 sm:grid-cols-4">
          <Field label="类型">
            <Select value={filters.kind} onChange={(e) => update({ kind: e.target.value as TaskFilters['kind'] })}>
              <option value="">全部</option>
              {REQUEST_KINDS.map((k) => (
                <option key={k} value={k}>{labelled(REQUEST_KIND_LABEL, k)}</option>
              ))}
            </Select>
          </Field>
          <Field label="平台">
            <Input name="platform" defaultValue={filters.platform} placeholder="全部" />
          </Field>
          <Field label="项目">
            <Input name="project" defaultValue={filters.project} placeholder="全部" />
          </Field>
          <Button type="submit" variant="secondary">筛选</Button>
        </form>
        <div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-2">
          <span className="text-sm font-medium text-zinc-700">状态</span>
          {TASK_STATUSES.map((s) => {
            const on = filters.statuses.includes(s)
            return (
              <Check
                key={s}
                label={labelled(TASK_STATUS_LABEL, s)}
                checked={on}
                disabled={on && filters.statuses.length === 1}
                onChange={(e) => toggle(s, e.target.checked)}
              />
            )
          })}
          <Button variant="secondary" onClick={() => update({ statuses: TASK_STATUSES })}>全部</Button>
          <Button variant="secondary" onClick={() => update({ statuses: ['failed'] })}>只看失败</Button>
        </div>
      </Card>
      {list.error && <ErrorNotice error={list.error} />}
      <Card
        title={`写入请求${list.data ? `（已显示 ${formatNumber(rows.length)} 条）` : ''}`}
        actions={<RefreshButton busy={list.isFetching} onClick={() => list.refetch()} />}
      >
        {list.isPending ? (
          <Muted>加载中…</Muted>
        ) : rows.length === 0 ? (
          <Muted>没有符合条件的请求。</Muted>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr>
                  <TH>请求</TH>
                  <TH>时间</TH>
                  <TH>类型</TH>
                  <TH>平台</TH>
                  <TH>项目 / 会话</TH>
                  <TH>状态</TH>
                  <TH className="text-right">失败次数</TH>
                  <TH>错误</TH>
                  <TH className="text-right">记忆</TH>
                  <TH className="text-right">消息</TH>
                </tr>
              </thead>
              <tbody>
                {rows.map((r) => (
                  <tr key={r.id} className="border-t border-zinc-100">
                    <TD>
                      <Link to={`/tasks/${r.requestId}`} className="font-mono text-sky-700 hover:underline">{r.requestId}</Link>
                    </TD>
                    <TD className="whitespace-nowrap">{formatTime(r.createdAt)}</TD>
                    <TD className="whitespace-nowrap">{REQUEST_KIND_LABEL[r.kind] ?? r.kind}</TD>
                    <TD className="whitespace-nowrap">{r.platform}</TD>
                    <TD className="max-w-48 truncate whitespace-nowrap"><ProjectName id={r.project} /> / {r.session}</TD>
                    <TD><ReviewStatusBadge status={r.status} /></TD>
                    <TD className="text-right tabular-nums">{r.failures ?? 0}</TD>
                    <TD className="max-w-xs truncate text-red-700">
                      <span title={r.error ?? undefined}>{r.error}</span>
                    </TD>
                    <TD className="text-right tabular-nums">{r.memoryIds.length}</TD>
                    <TD className="text-right tabular-nums">{r.messageCount}</TD>
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
    </>
  )
}

// ---------------------------------------------------------------- model responses

function Responses({ filters, update }: TabProps) {
  const [expanded, setExpanded] = useState<string>()
  const list = useInfiniteQuery({
    queryKey: ['requests', 'responses', responsesRequest(filters)],
    queryFn: ({ pageParam }) => api.listModelResponses(responsesRequest(filters, pageParam)),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.nextCursor ?? undefined,
  })
  const rows = useMemo(() => list.data?.pages.flatMap((p) => p.responses) ?? [], [list.data])
  const submit = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    update({ stage: String(new FormData(e.currentTarget).get('stage')).trim() })
  }

  return (
    <>
      <Card>
        <form key={filters.stage} onSubmit={submit} className="flex flex-wrap items-end gap-3">
          <Field label="阶段" hint="例如 Mem0-add">
            <Input name="stage" defaultValue={filters.stage} placeholder="全部" />
          </Field>
          <Button type="submit" variant="secondary">筛选</Button>
          <Check
            className="pb-2"
            label="只看失败（无响应、非 2xx、响应不是 JSON 或输出格式不对）"
            checked={filters.failedOnly}
            onChange={(e) => update({ failedOnly: e.target.checked })}
          />
        </form>
      </Card>
      {list.error && <ErrorNotice error={list.error} />}
      <Card
        title={`模型调用${list.data ? `（已显示 ${formatNumber(rows.length)} 条）` : ''}`}
        actions={<RefreshButton busy={list.isFetching} onClick={() => list.refetch()} />}
      >
        <p className="mb-3 text-sm text-zinc-500">每一行是一次生成模型请求（含重试），记录原样的请求和响应。点一行展开。</p>
        {list.isPending ? (
          <Muted>加载中…</Muted>
        ) : rows.length === 0 ? (
          <Muted>没有符合条件的模型调用。</Muted>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr>
                  <TH>时间</TH>
                  <TH>阶段</TH>
                  <TH>模型</TH>
                  <TH className="text-right">尝试</TH>
                  <TH className="text-right">HTTP</TH>
                  <TH className="text-right">耗时</TH>
                  <TH>问题</TH>
                </tr>
              </thead>
              <tbody>
                {rows.map((r) => (
                  <Fragment key={r.id}>
                    <tr
                      className="cursor-pointer border-t border-zinc-100 hover:bg-zinc-50"
                      aria-expanded={expanded === r.id}
                      onClick={() => setExpanded(expanded === r.id ? undefined : r.id)}
                    >
                      <TD className="whitespace-nowrap">
                        <span className="mr-1 inline-block w-3 text-zinc-400">{expanded === r.id ? '▾' : '▸'}</span>
                        {formatTime(r.createdAt)}
                      </TD>
                      <TD>{r.stage}</TD>
                      <TD>{r.model}</TD>
                      <TD className="text-right tabular-nums">{r.attempt}</TD>
                      <TD className="text-right tabular-nums">{r.httpStatus ?? '—'}</TD>
                      <TD className="text-right tabular-nums whitespace-nowrap">
                        {r.latencyMs === undefined ? '—' : `${formatNumber(r.latencyMs)} ms`}
                      </TD>
                      <TD className="max-w-sm truncate text-red-700">{problem(r)}</TD>
                    </tr>
                    {expanded === r.id && (
                      <tr className="bg-zinc-50/60">
                        <td colSpan={7} className="px-2 pb-3">
                          <ResponseBody r={r} />
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
    </>
  )
}

/** What went wrong with one model call, in one line; empty when nothing did. */
function problem(r: ModelResponse): string {
  if (r.transportError) return r.transportError
  if (r.httpStatus != null && (r.httpStatus < 200 || r.httpStatus > 299)) return `HTTP ${r.httpStatus}`
  if (r.validOutputShape === false) return r.validationError ?? '输出格式不对'
  if (r.validOutputShape !== true) return '响应不是 JSON'
  return ''
}

function ResponseBody({ r }: { r: ModelResponse }) {
  return (
    <div className="space-y-2 py-1 text-sm">
      <div className="flex flex-wrap items-center gap-2 text-xs text-zinc-500">
        <span className="font-mono">{r.id}</span>
        {responseFailed(r) ? <Badge tone="red">失败</Badge> : <Badge tone="green">成功</Badge>}
        {r.finishReason && <span>结束原因 {r.finishReason}</span>}
      </div>
      {r.transportError && <p className="text-red-700">传输错误：{r.transportError}</p>}
      {r.validationError && <p className="text-red-700">校验错误：{r.validationError}</p>}
      {r.rawContent !== undefined && (
        <div>
          <p className="mb-1 text-xs text-zinc-500">模型输出（rawContent）</p>
          <Pre>{r.rawContent || '（空）'}</Pre>
        </div>
      )}
      {r.rawBody !== undefined && (
        <details>
          <summary className="cursor-pointer text-xs text-zinc-500">HTTP 响应原文（rawBody）</summary>
          <Pre>{r.rawBody}</Pre>
        </details>
      )}
      <details>
        <summary className="cursor-pointer text-xs text-zinc-500">请求（request）</summary>
        <Pre>{json(r.request)}</Pre>
      </details>
    </div>
  )
}

// ---------------------------------------------------------------- one request

export function RequestPage() {
  const { requestId } = useParams() as { requestId: string }
  const query = useQuery({ queryKey: ['requests', 'one', requestId], queryFn: () => api.getRequest(requestId) })
  if (query.error) return <ErrorNotice error={query.error} />
  if (!query.data) return <Muted>加载中…</Muted>
  return <RequestView detail={query.data} refresh={<RefreshButton busy={query.isFetching} onClick={() => query.refetch()} />} />
}

function RequestView({ detail, refresh }: { detail: RequestDetail; refresh: ReactNode }) {
  const { request: r, extractions, responses } = detail
  const messages = r.inputMessages ?? []
  const rows: [string, ReactNode][] = [
    ['类型', labelled(REQUEST_KIND_LABEL, r.kind)],
    ['平台', r.platform],
    ['项目 / 会话', <><ProjectName id={r.project} /> / {r.session}</>],
    ['创建时间', formatTime(r.createdAt)],
    ['更新时间', formatTime(r.updatedAt)],
    ['写入的记忆', ids(r.memoryIds)],
  ]
  if (r.turnId) rows.push(['轮次', <span className="font-mono">{r.turnId}</span>])
  if (r.importRunId) {
    rows.push([
      '导入运行',
      <Link to={`/imports/${r.importRunId}`} className="font-mono text-sky-700 hover:underline">{r.importRunId}</Link>,
    ])
  }
  if (r.infer !== undefined) rows.push(['推断（infer）', r.infer ? '是' : '否'])
  if (r.formed !== undefined) rows.push(['已形成 / 可检索', `${r.formed ? '是' : '否'} / ${r.searchable ? '是' : '否'}`])
  if (r.failures) rows.push(['失败次数', String(r.failures)])
  if (r.retryAtTs && r.status === 'queued') rows.push(['下次重试', formatTime(new Date(r.retryAtTs * 1000).toISOString())])
  if (r.latencyMs) {
    rows.push(['耗时', Object.entries(r.latencyMs).map(([k, v]) => `${k} ${v.toFixed(1)} ms`).join('，')])
  }

  return (
    <div className="space-y-4">
      <Card
        title={
          <span className="flex flex-wrap items-center gap-2">
            写入请求 <span className="font-mono text-sm font-normal text-zinc-500">{r.requestId}</span>
            <ReviewStatusBadge status={r.status} />
          </span>
        }
        actions={refresh}
      >
        {r.error && (
          <div className="mb-3">
            <ErrorNotice error={new Error(r.error)} />
          </div>
        )}
        <Fields rows={rows} />
      </Card>
      <Card title={`输入消息（${messages.length}）`}>
        {messages.length === 0 ? (
          <Muted>这个请求没有保存输入消息。</Muted>
        ) : (
          <ul className="space-y-1">
            {messages.map((m, i) => (
              <li key={i} className={m.role === 'user' ? 'rounded-md bg-zinc-50 p-2 text-sm' : 'p-2 text-sm'}>
                <span className="mr-2 text-xs text-zinc-500">
                  {EVIDENCE_ROLE_LABEL[m.role] ?? m.role} · {formatTime(m.timestamp)}
                </span>
                <span className="whitespace-pre-wrap">{m.content}</span>
              </li>
            ))}
          </ul>
        )}
      </Card>
      <Card title={`抽取（${extractions.length}）`}>
        {extractions.length === 0 ? (
          <Muted>没有抽取记录：请求还没处理，或者模型调用全部失败。失败的调用见下方的模型调用。</Muted>
        ) : (
          <ul className="space-y-3">
            {extractions.map((e) => <ExtractionItem key={e.id} e={e} />)}
          </ul>
        )}
      </Card>
      <Card title={`模型调用（${responses.length}）`}>
        {responses.length === 0 ? (
          <Muted>没有关联的模型调用。</Muted>
        ) : (
          <ul className="space-y-4">
            {responses.map((resp) => (
              <li key={resp.id} className="rounded-md border border-zinc-200 p-3">
                <p className="mb-1 text-xs text-zinc-500">
                  {resp.stage} · {resp.model} · 第 {resp.attempt} 次尝试 · {formatTime(resp.createdAt)}
                  {resp.httpStatus != null && ` · HTTP ${resp.httpStatus}`}
                  {resp.latencyMs !== undefined && ` · ${formatNumber(resp.latencyMs)} ms`}
                </p>
                <ResponseBody r={resp} />
              </li>
            ))}
          </ul>
        )}
      </Card>
    </div>
  )
}

function ExtractionItem({ e }: { e: Extraction }) {
  return (
    <li className="rounded-md border border-zinc-200 p-3">
      <div className="mb-1 flex flex-wrap items-center gap-2 text-xs text-zinc-500">
        <Badge>{e.stage}</Badge>
        <span>{e.status}</span>
        {e.createdAt && <span>{formatTime(e.createdAt)}</span>}
        {e.responseLogId && <span>模型调用 <span className="font-mono">{e.responseLogId}</span></span>}
      </div>
      {e.output !== undefined && <Pre>{json(e.output)}</Pre>}
    </li>
  )
}
