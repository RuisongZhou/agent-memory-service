import { useQuery } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import { Link } from 'react-router'
import { api } from '../api/client'
import type { Facet, LlmUsage, MemoryType, Stats } from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { Badge, Card, Fields, Muted } from '../components/ui'
import {
  ALL_STATUSES,
  filtersToParams,
  NO_FILTERS,
  NO_TASK_FILTERS,
  taskFiltersToParams,
  type MemoryFilters,
  type RecallFilter,
} from '../lib/filters'
import { formatNumber, ORIGIN_LABEL, projectName, STATUS_LABEL, TYPE_LABEL } from '../lib/format'

/** The memory list behind a count. Counts cover every status, so the list does too unless one is named. */
const listLink = (set: Partial<MemoryFilters>) =>
  `/memories?${filtersToParams({ ...NO_FILTERS, statuses: ALL_STATUSES, ...set })}`

/** The memory list behind a recall count. These count active records, so the list keeps its default statuses. */
const recallLink = (recall: RecallFilter) => `/memories?${filtersToParams({ ...NO_FILTERS, recall })}`

const usageRows = (u: LlmUsage): [string, string][] => [
  ['模型', u.model],
  ['调用', formatNumber(u.calls)],
  ['尝试（含重试）', formatNumber(u.attempts)],
  ['输入 token', formatNumber(u.promptTokens)],
  ['输出 token', formatNumber(u.completionTokens)],
  ['缓存命中 token', formatNumber(u.cacheHitTokens)],
]

/** `list_uncertainties` stops at its limit, so a full page means at least that many. */
const OPEN_LIMIT = 200

const failedTasksLink = `/tasks?${taskFiltersToParams({ ...NO_TASK_FILTERS, statuses: ['failed'] })}`

const PageLink = ({ to, children }: { to: string; children: ReactNode }) => (
  <Link to={to} className="text-sky-700 hover:underline">{children}</Link>
)

/** What needs a person: switches that gate the review pages, open questions and failed writes. */
function Review({ features }: { features?: Stats['features'] }) {
  const open = useQuery({
    queryKey: ['uncertainties', { status: 'open', limit: OPEN_LIMIT }],
    queryFn: () => api.listUncertainties({ status: 'open', limit: OPEN_LIMIT }),
  })
  const failed = useQuery({
    queryKey: ['requests', 'any-failed'],
    queryFn: () => api.listRequests({ statuses: ['failed'], limit: 1 }),
  })
  const openCount = open.data?.uncertainties.length
  const on = (value: boolean | undefined, yes: string, no: string) =>
    value === undefined ? '—' : <Badge tone={value ? 'green' : 'gray'}>{value ? yes : no}</Badge>

  return (
    <Card title="审核与追溯">
      {(open.error ?? failed.error) && <ErrorNotice error={(open.error ?? failed.error)!} />}
      <div className="grid gap-4 md:grid-cols-2">
        <Fields
          rows={[
            ['consolidation', on(features?.consolidation, '已开启', '未开启')],
            ['生成模型', on(features?.generation, '已配置', '未配置')],
            [
              '未解决的冲突',
              openCount === undefined ? (
                '—'
              ) : (
                <PageLink to="/conflicts">
                  {openCount >= OPEN_LIMIT ? `${OPEN_LIMIT}+` : formatNumber(openCount)}
                </PageLink>
              ),
            ],
            [
              '失败的写入请求',
              failed.data === undefined ? (
                '—'
              ) : failed.data.requests.length ? (
                <PageLink to={failedTasksLink}>有</PageLink>
              ) : (
                '无'
              ),
            ],
          ]}
        />
        <ul className="space-y-1 text-sm">
          <li><PageLink to="/imports">导入审核</PageLink>：审核、应用和回滚导入运行</li>
          <li><PageLink to="/consolidation">consolidation</PageLink>：发起提议、应用和回滚</li>
          <li><PageLink to="/recalls">召回记录</PageLink>：哪个平台、哪个会话取回了哪些记忆</li>
          <li><PageLink to="/tasks?tab=responses">模型调用</PageLink>：失败的生成调用和原始响应</li>
        </ul>
      </div>
    </Card>
  )
}

/** Active records agents have not recalled: candidates for review. The UI's own calls are not counted. */
function Staleness({ retrieval }: { retrieval?: Stats['retrieval'] }) {
  const count = (n: number | undefined, recall: RecallFilter) =>
    n === undefined ? '—' : <PageLink to={recallLink(recall)}>{formatNumber(n)}</PageLink>
  return (
    <Card title="召回情况">
      <Fields
        rows={[
          ['从未被召回', count(retrieval?.neverRecalled, 'never')],
          ['30 天没被召回', count(retrieval?.notRecalled30d, '30')],
        ]}
      />
      <p className="mt-3 text-sm text-zinc-500">
        只统计活跃记忆（当前、冲突、候选）。“30 天没被召回”是入库超过 30 天、且最近 30 天没被召回的记忆。召回只算 agent 的调用，不含网页自己的检索和上下文预览。
      </p>
    </Card>
  )
}

function FacetList({ title, facets, link, labels, show }: {
  title: string
  facets: Facet[]
  link: (name: string) => string
  labels?: Record<string, string>
  /** How a name reads when it is not the whole story; the name itself is then the tooltip. */
  show?: (name: string) => string
}) {
  return (
    <Card title={title}>
      {facets.length === 0 ? (
        <Muted>没有记录。</Muted>
      ) : (
        <ul className="space-y-1 text-sm">
          {facets.map((f) => (
            <li key={f.name} className="flex justify-between gap-2">
              <Link to={link(f.name)} title={show && f.name} className="truncate text-sky-700 hover:underline">
                {labels?.[f.name] ? `${labels[f.name]} (${f.name})` : show ? show(f.name) : f.name}
              </Link>
              <span className="text-zinc-600 tabular-nums">{formatNumber(f.count)}</span>
            </li>
          ))}
        </ul>
      )}
    </Card>
  )
}

export function OverviewPage() {
  const health = useQuery({ queryKey: ['health'], queryFn: api.health })
  const stats = useQuery({ queryKey: ['stats'], queryFn: api.stats })
  const facets = useQuery({ queryKey: ['facets'], queryFn: api.facets })
  const error = health.error ?? stats.error ?? facets.error

  const byType = stats.data?.memories ?? {}
  const types = Object.keys(byType) as MemoryType[]
  const statuses = ALL_STATUSES.filter((s) => types.some((t) => byType[t][s]))

  return (
    <div className="space-y-4">
      {error && <ErrorNotice error={error} />}
      <div className="grid gap-4 md:grid-cols-3">
        <Card title="服务">
          {health.data && (
            <Fields
              rows={[
                ['状态', health.data.ok ? '正常' : '异常'],
                ['版本', health.data.version ?? '（未报告）'],
                ['算法', health.data.memoryStrategy],
                ['模型后端', health.data.modelBackend],
                ['嵌入模型', health.data.embedModel],
                ['重排模型', health.data.rerankModel ?? '未连接'],
                ['模型地址', health.data.modelDevice],
                ['精度', health.data.modelDtype],
                ['提示语言', health.data.promptVariant],
              ]}
            />
          )}
        </Card>
        <Card title="生成模型用量">
          <h3 className="mb-1 text-sm font-medium">本次进程</h3>
          {health.data && <Fields rows={usageRows(health.data.llm)} />}
          <h3 className="mt-3 mb-1 text-sm font-medium">累计（llm_usage 表）</h3>
          {stats.data && (
            <Fields rows={Object.entries(stats.data.llm.persisted).map(([k, v]) => [k, formatNumber(v)])} />
          )}
        </Card>
        <Card title="索引与任务">
          {stats.data && (
            <Fields
              rows={[
                ['记忆向量', formatNumber(stats.data.index.memoryVectors)],
                ['待建索引', formatNumber(stats.data.index.pendingTasks)],
                ['待处理写入', formatNumber(stats.data.jobs.pendingJobs)],
                ['无结果的检索', formatNumber(stats.data.retrieval.gapSearches)],
                ['已记录的生成调用', formatNumber(stats.data.llm.rows)],
              ]}
            />
          )}
        </Card>
      </div>

      <Review features={stats.data?.features} />

      <Staleness retrieval={stats.data?.retrieval} />

      <Card title={`记忆记录${facets.data ? `（共 ${formatNumber(facets.data.total)} 条）` : ''}`}>
        {types.length === 0 ? (
          <Muted>没有记录。</Muted>
        ) : (
          <table className="w-full text-sm">
            <thead>
              <tr className="text-left text-zinc-500">
                <th className="py-1 font-normal">类型</th>
                {statuses.map((s) => (
                  <th key={s} className="py-1 text-right font-normal">{STATUS_LABEL[s]}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {types.map((t) => (
                <tr key={t} className="border-t border-zinc-100">
                  <td className="py-1">
                    <Link to={listLink({ type: t })} className="text-sky-700 hover:underline">
                      {TYPE_LABEL[t]}
                    </Link>
                  </td>
                  {statuses.map((s) => (
                    <td key={s} className="py-1 text-right tabular-nums">
                      {byType[t][s] ? (
                        <Link to={listLink({ type: t, statuses: [s] })} className="hover:underline">
                          {formatNumber(byType[t][s])}
                        </Link>
                      ) : (
                        <span className="text-zinc-300">0</span>
                      )}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Card>

      {facets.data && (
        <div className="grid gap-4 md:grid-cols-3">
          <FacetList
            title="项目"
            facets={facets.data.projects}
            link={(project) => listLink({ project })}
            show={projectName}
          />
          <FacetList
            title="写入平台"
            facets={facets.data.sourcePlatforms}
            link={(sourcePlatform) => listLink({ sourcePlatform })}
          />
          <FacetList
            title="来源"
            facets={facets.data.origins}
            link={(origin) => listLink({ origin })}
            labels={ORIGIN_LABEL}
          />
        </div>
      )}
    </div>
  )
}
