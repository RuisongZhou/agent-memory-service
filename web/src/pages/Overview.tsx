import { useQuery } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import { Link } from 'react-router'
import { api } from '../api/client'
import type { Calls, Facet, LlmUsage, MemoryType, PlatformActivity, Stats } from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { Icon, type IconName } from '../components/icons'
import { OPEN_CONFLICTS_LIMIT, openConflictsQuery } from '../components/Layout'
import { Badge, Card, cx, Fields, Loading, Muted, TD, TH } from '../components/ui'
import {
  ALL_STATUSES,
  filtersToParams,
  NO_FILTERS,
  NO_RECALL_FILTERS,
  NO_TASK_FILTERS,
  recallFiltersToParams,
  taskFiltersToParams,
  type MemoryFilters,
  type RecallFilter,
} from '../lib/filters'
import { formatAgo, formatNumber, formatTime, ORIGIN_LABEL, projectName, STATUS_LABEL, TYPE_LABEL } from '../lib/format'

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

const failedTasksLink = `/tasks?${taskFiltersToParams({ ...NO_TASK_FILTERS, statuses: ['failed'] })}`

const PageLink = ({ to, children }: { to: string; children: ReactNode }) => (
  <Link to={to} className="text-brand-700 hover:underline">{children}</Link>
)

const TILE_TONE = {
  gray: 'bg-zinc-100 text-zinc-600',
  brand: 'bg-brand-50 text-brand-600',
  red: 'bg-red-50 text-red-600',
  amber: 'bg-amber-50 text-amber-600',
  green: 'bg-emerald-50 text-emerald-600',
}

/** One number the overview leads with, linking to the page that lists what it counts. */
function Tile({ to, icon, label, value, hint, tone = 'gray' }: {
  to: string
  icon: IconName
  label: string
  value: ReactNode
  hint: ReactNode
  tone?: keyof typeof TILE_TONE
}) {
  return (
    <Link
      to={to}
      className="group flex items-start gap-3 rounded-xl border border-zinc-200 bg-white p-4 shadow-xs transition hover:border-brand-200 hover:shadow-md focus-visible:ring-2 focus-visible:ring-brand-500/40 focus-visible:outline-none"
    >
      <span className={cx('hidden size-9 shrink-0 place-items-center rounded-lg sm:grid', TILE_TONE[tone])}>
        <Icon name={icon} className="size-5" />
      </span>
      <span className="min-w-0 flex-1">
        <span className="block text-sm text-zinc-500">{label}</span>
        <span className="mt-0.5 block text-2xl font-semibold text-zinc-900 tabular-nums">{value}</span>
        <span className="mt-0.5 block truncate text-xs text-zinc-500">{hint}</span>
      </span>
      <Icon name="arrowRight" className="mt-1 hidden size-4 text-zinc-300 sm:block transition group-hover:translate-x-0.5 group-hover:text-brand-600" />
    </Link>
  )
}

/** What needs a person first: open conflicts and failed writes, then how much is stored and how much is never used. */
function Tiles({ total, retrieval }: { total?: number; retrieval?: Stats['retrieval'] }) {
  const open = useQuery(openConflictsQuery)
  const failed = useQuery({
    queryKey: ['requests', 'any-failed'],
    queryFn: () => api.listRequests({ statuses: ['failed'], limit: 1 }),
  })
  const openCount = open.data?.uncertainties.length
  const anyFailed = failed.data && failed.data.requests.length > 0
  const dash = <span className="text-zinc-300">—</span>
  return (
    <>
      {(open.error ?? failed.error) && <ErrorNotice error={(open.error ?? failed.error)!} />}
      <div className="grid grid-cols-2 gap-3 sm:gap-4 xl:grid-cols-4">
        <Tile
          to={listLink({})}
          icon="list"
          tone="brand"
          label="记忆总数"
          value={total === undefined ? dash : formatNumber(total)}
          hint="含已取代和已失效"
        />
        <Tile
          to="/conflicts"
          icon="alert"
          tone={openCount ? 'red' : 'green'}
          label="未解决的冲突"
          value={
            openCount === undefined ? dash : openCount >= OPEN_CONFLICTS_LIMIT ? `${OPEN_CONFLICTS_LIMIT}+` : formatNumber(openCount)
          }
          hint={openCount ? '需要选择保留哪一条' : '没有待裁决的冲突'}
        />
        <Tile
          to={failedTasksLink}
          icon="tasks"
          tone={anyFailed ? 'red' : 'green'}
          label="失败的写入请求"
          value={failed.data === undefined ? dash : anyFailed ? '有' : '无'}
          hint={anyFailed ? '查看失败原因和模型原始响应' : '所有写入都已处理'}
        />
        <Tile
          to={recallLink('never')}
          icon="history"
          tone={retrieval?.neverRecalled ? 'amber' : 'gray'}
          label="从未被召回"
          value={retrieval === undefined ? dash : formatNumber(retrieval.neverRecalled)}
          hint={retrieval === undefined ? '' : `另有 ${formatNumber(retrieval.notRecalled30d)} 条 30 天没被召回`}
        />
      </div>
    </>
  )
}

/** Switches that gate the review pages, and where each kind of review happens. */
function Review({ features }: { features?: Stats['features'] }) {
  const on = (value: boolean | undefined, yes: string, no: string) =>
    value === undefined ? '—' : <Badge tone={value ? 'green' : 'gray'}>{value ? yes : no}</Badge>

  return (
    <Card title="审核与追溯">
      <Fields
        rows={[
          ['consolidation', on(features?.consolidation, '已开启', '未开启')],
          ['生成模型', on(features?.generation, '已配置', '未配置')],
        ]}
      />
      <ul className="mt-4 space-y-2 border-t border-zinc-100 pt-4 text-sm">
        <li><PageLink to="/imports">导入审核</PageLink>：审核、应用和回滚导入运行</li>
        <li><PageLink to="/consolidation">consolidation</PageLink>：发起提议、应用和回滚</li>
        <li><PageLink to="/recalls">召回记录</PageLink>：哪个平台、哪个会话取回了哪些记忆</li>
        <li><PageLink to="/tasks?tab=responses">模型调用</PageLink>：失败的生成调用和原始响应</li>
      </ul>
    </Card>
  )
}

/** The channels of `platform_activity`, as the card's columns. */
const CHANNELS: [keyof Omit<PlatformActivity, 'platform' | 'lastAt'>, string][] = [
  ['capture', 'hook：对话抓取'],
  ['hookRecall', 'hook：自动召回'],
  ['mcp', 'MCP 工具'],
  ['other', '其他'],
]

const platformRecallsLink = (platform: string) =>
  `/recalls?${recallFiltersToParams({ ...NO_RECALL_FILTERS, platform })}`

const ChannelCalls = ({ calls }: { calls?: Calls }) =>
  calls ? (
    <span title={formatTime(calls.lastAt)}>
      {formatAgo(calls.lastAt)} · {formatNumber(calls.count)} 次
    </span>
  ) : (
    <span className="text-zinc-300">—</span>
  )

/**
 * Which platforms reach the daemon, and through which of their programs. What they call shows they are connected
 * and work: an installed hook or MCP server does nothing until the platform trusts it and restarts.
 */
function Platforms() {
  const activity = useQuery({ queryKey: ['platform-activity'], queryFn: api.platformActivity })
  const platforms = activity.data?.platforms
  return (
    <Card title="接入平台（近 30 天）" actions={<PageLink to="/platforms">管理接入 →</PageLink>}>
      {activity.error && <ErrorNotice error={activity.error} />}
      {platforms === undefined ? (
        <Loading />
      ) : platforms.length === 0 ? (
        <Muted>近 30 天没有平台调用过记忆服务。</Muted>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr>
                <TH>平台</TH>
                {CHANNELS.map(([key, label]) => <TH key={key}>{label}</TH>)}
              </tr>
            </thead>
            <tbody>
              {platforms.map((p) => (
                <tr key={p.platform} className="border-t border-zinc-100">
                  <TD className="whitespace-nowrap">
                    <PageLink to={platformRecallsLink(p.platform)}>{p.platform}</PageLink>
                  </TD>
                  {CHANNELS.map(([key]) => (
                    <TD key={key} className="whitespace-nowrap"><ChannelCalls calls={p[key]} /></TD>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <p className="mt-3 text-sm text-zinc-500">
        按调用的程序分：hook 上传抓到的对话、在会话开始和提问时自动召回；MCP 是 agent 主动调用记忆工具，不用就没有记录。“其他”是直接调用 HTTP
        接口的，以及升级前没记下来源的调用。不含网页自己的调用。某个平台没出现或缺了一项：在
        <PageLink to="/platforms">接入平台</PageLink>页接入或更新它，并按步骤在平台里信任 hook 和 MCP、重启。
      </p>
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
  const most = Math.max(1, ...facets.map((f) => f.count))
  return (
    <Card title={title}>
      {facets.length === 0 ? (
        <Muted>没有记录。</Muted>
      ) : (
        <ul className="space-y-2.5 text-sm">
          {facets.map((f) => (
            <li key={f.name}>
              <Link to={link(f.name)} title={show && f.name} className="group block">
                <span className="flex justify-between gap-2">
                  <span className="truncate text-zinc-700 group-hover:text-brand-700">
                    {labels?.[f.name] ? `${labels[f.name]} (${f.name})` : show ? show(f.name) : f.name}
                  </span>
                  <span className="text-zinc-500 tabular-nums">{formatNumber(f.count)}</span>
                </span>
                <span className="mt-1 block h-1.5 overflow-hidden rounded-full bg-zinc-100">
                  <span
                    className="block h-full rounded-full bg-brand-500/70 group-hover:bg-brand-600"
                    style={{ width: `${(f.count / most) * 100}%` }}
                  />
                </span>
              </Link>
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
    <div className="space-y-6">
      {error && <ErrorNotice error={error} />}
      <Tiles total={facets.data?.total} retrieval={stats.data?.retrieval} />

      <Platforms />

      <div className="grid gap-4 lg:grid-cols-3">
        <Card
          className="lg:col-span-2"
          title="记忆记录"
          description="按类型和状态统计，点数字查看对应的记忆"
        >
          {stats.isPending ? (
            <Loading />
          ) : types.length === 0 ? (
            <Muted>没有记录。</Muted>
          ) : (
            <div className="-mx-4 overflow-x-auto sm:-mx-5">
              <table className="w-full text-sm">
                <thead>
                  <tr>
                    <TH className="pl-4 sm:pl-5">类型</TH>
                    {statuses.map((s) => (
                      <TH key={s} className="text-right last:pr-4 sm:last:pr-5">{STATUS_LABEL[s]}</TH>
                    ))}
                  </tr>
                </thead>
                <tbody className="divide-y divide-zinc-100">
                  {types.map((t) => (
                    <tr key={t}>
                      <TD className="pl-4 sm:pl-5">
                        <Link to={listLink({ type: t })} className="font-medium text-brand-700 hover:underline">
                          {TYPE_LABEL[t]}
                        </Link>
                      </TD>
                      {statuses.map((s) => (
                        <TD key={s} className="text-right tabular-nums last:pr-4 sm:last:pr-5">
                          {byType[t][s] ? (
                            <Link to={listLink({ type: t, statuses: [s] })} className="hover:text-brand-700 hover:underline">
                              {formatNumber(byType[t][s])}
                            </Link>
                          ) : (
                            <span className="text-zinc-300">0</span>
                          )}
                        </TD>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </Card>
        <Review features={stats.data?.features} />
      </div>

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

      <Staleness retrieval={stats.data?.retrieval} />

      <div className="grid gap-4 md:grid-cols-3">
        <Card title="服务">
          {health.data ? (
            <Fields
              rows={[
                ['状态', <Badge tone={health.data.ok ? 'green' : 'red'}>{health.data.ok ? '正常' : '异常'}</Badge>],
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
          ) : (
            <Loading />
          )}
        </Card>
        <Card title="生成模型用量">
          <h3 className="mb-2 text-xs font-medium text-zinc-500">本次进程</h3>
          {health.data && <Fields rows={usageRows(health.data.llm)} />}
          <h3 className="mt-4 mb-2 border-t border-zinc-100 pt-4 text-xs font-medium text-zinc-500">累计（llm_usage 表）</h3>
          {stats.data && (
            <Fields rows={Object.entries(stats.data.llm.persisted).map(([k, v]) => [k, formatNumber(v)])} />
          )}
        </Card>
        <Card title="索引与任务">
          {stats.data ? (
            <Fields
              rows={[
                ['记忆向量', formatNumber(stats.data.index.memoryVectors)],
                ['待建索引', formatNumber(stats.data.index.pendingTasks)],
                ['待处理写入', formatNumber(stats.data.jobs.pendingJobs)],
                ['无结果的检索', formatNumber(stats.data.retrieval.gapSearches)],
                ['已记录的生成调用', formatNumber(stats.data.llm.rows)],
              ]}
            />
          ) : (
            <Loading />
          )}
        </Card>
      </div>
    </div>
  )
}
