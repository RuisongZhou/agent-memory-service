import { useMutation, useQuery } from '@tanstack/react-query'
import { useState, type FormEvent, type ReactNode } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'
import { api } from '../api/client'
import { useRefresh } from '../api/refresh'
import type {
  ConsolidationOp,
  ConsolidationProposal,
  ConsolidationRun,
  ConsolidationRunSummary,
  MemorySummaries,
  Stats,
} from '../api/types'
import { DiffView } from '../components/DiffView'
import { ErrorNotice } from '../components/ErrorNotice'
import { MemoryRef, ReviewStatusBadge } from '../components/memory'
import { Modal } from '../components/Modal'
import { Badge, Button, Card, Field, Fields, Input, MemoryLink, ModelCall, Muted, Note, RefreshButton, Select, TD, TH } from '../components/ui'
import { formatNumber, formatPercent, formatTime } from '../lib/format'

const RUN_LIMIT = 50
const ROLLBACK_STATUSES = ['applied', 'partially_rolled_back', 'rolling_back']

const OPS: { op: ConsolidationOp['op']; label: string; effect: string }[] = [
  { op: 'merge', label: '合并（merge）', effect: '写一条新的合并事实，输入变为已取代' },
  { op: 'supersede', label: '取代（supersede）', effect: '旧值变为已取代，新值记下它取代了谁' },
  { op: 'conflict', label: '冲突（conflict）', effect: '打开一个冲突问题，成员变为冲突，到“冲突”页解决' },
  { op: 'keep', label: '保留（keep）', effect: '不改动' },
]

/** Why consolidation cannot be proposed, or `undefined` when it can. */
function proposeBlocked(stats: Stats | undefined): string | undefined {
  if (!stats) return '正在读取 daemon 的功能开关…'
  if (!stats.features.consolidation) return 'daemon 没有开启 consolidation（配置 consolidationEnabled: true）。'
  if (!stats.features.generation) return 'daemon 没有配置生成模型的 key，无法提议。'
  return undefined
}

// ---------------------------------------------------------------- run list

export function ConsolidationPage() {
  const [params, setParams] = useSearchParams()
  const project = params.get('project') ?? ''
  const runs = useQuery({
    queryKey: ['consolidation', 'runs', project],
    queryFn: () => api.consolidationRuns({ ...(project && { project }), limit: RUN_LIMIT }),
  })
  const filter = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    const value = String(new FormData(e.currentTarget).get('project')).trim()
    setParams(value ? { project: value } : {}, { replace: true })
  }

  return (
    <div className="space-y-4">
      <Propose />
      {runs.error && <ErrorNotice error={runs.error} />}
      <Card
        title={`consolidation 运行（最近 ${RUN_LIMIT} 次）`}
        actions={<RefreshButton busy={runs.isFetching} onClick={() => runs.refetch()} />}
      >
        <form key={project} onSubmit={filter} className="mb-3 flex flex-wrap items-end gap-2">
          <Field label="项目">
            <Input name="project" defaultValue={project} placeholder="全部项目，回车筛选" />
          </Field>
          <Button type="submit" variant="secondary">筛选</Button>
        </form>
        {runs.isPending ? (
          <Muted>加载中…</Muted>
        ) : !runs.data?.runs.length ? (
          <Muted>没有 consolidation 运行。</Muted>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr>
                  <TH>运行</TH>
                  <TH>状态</TH>
                  <TH>项目</TH>
                  <TH>创建时间</TH>
                  <TH className="text-right">簇</TH>
                  <TH className="text-right">通过 / 被拒</TH>
                  <TH className="text-right">退役</TH>
                  <TH className="text-right">已应用 / 跳过</TH>
                  <TH>错误</TH>
                </tr>
              </thead>
              <tbody>
                {runs.data.runs.map((r) => (
                  <tr key={r.runId} className="border-t border-zinc-100">
                    <TD>
                      <Link to={`/consolidation/${r.runId}`} className="font-mono text-sky-700 hover:underline">{r.runId}</Link>
                    </TD>
                    <TD><ReviewStatusBadge status={r.status} /></TD>
                    <TD>{r.project}</TD>
                    <TD className="whitespace-nowrap">{formatTime(r.createdAt)}</TD>
                    <TD className="text-right tabular-nums">{r.processedClusters} / {r.clusterCount}</TD>
                    <TD className="text-right tabular-nums">{r.acceptedOps} / {r.rejectedOps}</TD>
                    <TD className="text-right tabular-nums whitespace-nowrap">
                      {r.retireCount}（{formatPercent(r.retireFraction)}）
                    </TD>
                    <TD className="text-right tabular-nums">
                      {r.appliedOps === undefined ? '—' : `${r.appliedOps} / ${r.skippedOps ?? 0}`}
                    </TD>
                    <TD className="max-w-xs truncate text-red-700">{r.error}</TD>
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

/** The one action on these pages that calls a model: one generation call per selected cluster. */
function Propose() {
  const stats = useQuery({ queryKey: ['stats'], queryFn: api.stats })
  const facets = useQuery({ queryKey: ['facets'], queryFn: api.facets })
  const [project, setProject] = useState('')
  const navigate = useNavigate()
  const refresh = useRefresh()
  const propose = useMutation({
    mutationFn: () => api.consolidate(project),
    onSuccess: (res) => {
      refresh()
      navigate(`/consolidation/${res.run.runId}`)
    },
  })
  const blocked = proposeBlocked(stats.data)
  const submit = (e: FormEvent) => {
    e.preventDefault()
    propose.mutate()
  }
  return (
    <Card title="发起提议">
      <p className="mb-3 text-sm text-zinc-600">
        把一个项目里相近的事实聚成簇，让生成模型对每个簇提议合并、取代、冲突或保留。提议只存成一次运行，不改动任何记录；审核后再应用。
      </p>
      {stats.error && <ErrorNotice error={stats.error} />}
      <form onSubmit={submit} className="flex flex-wrap items-end gap-3">
        <Field label="项目">
          <Select value={project} onChange={(e) => setProject(e.target.value)}>
            <option value="">选择项目</option>
            {facets.data?.projects.map((p) => (
              <option key={p.name} value={p.name}>{p.name} · {formatNumber(p.count)}</option>
            ))}
          </Select>
        </Field>
        <Button type="submit" disabled={!!blocked || !project || propose.isPending}>
          {propose.isPending ? '提议中…' : '发起提议'}
        </Button>
        <ModelCall>会调用生成模型：每个簇一次</ModelCall>
      </form>
      {blocked && stats.data && <div className="mt-3"><Note>{blocked}</Note></div>}
      {propose.error && <div className="mt-3"><ErrorNotice error={propose.error} /></div>}
    </Card>
  )
}

// ---------------------------------------------------------------- one run

export function ConsolidationRunPage() {
  const { runId } = useParams() as { runId: string }
  return <ConsolidationRunDetail key={runId} runId={runId} />
}

function ConsolidationRunDetail({ runId }: { runId: string }) {
  const query = useQuery({ queryKey: ['consolidation', 'run', runId], queryFn: () => api.consolidationRun(runId) })
  const stats = useQuery({ queryKey: ['stats'], queryFn: api.stats })

  if (query.error) return <ErrorNotice error={query.error} />
  if (!query.data) return <Muted>加载中…</Muted>
  const { run, proposals, memories } = query.data
  const enabled = stats.data?.features.consolidation

  return (
    <div className="space-y-4">
      <RunSummary
        run={run}
        actions={
          <div className="flex flex-wrap gap-2">
            <RefreshButton busy={query.isFetching} onClick={() => query.refetch()} />
            {run.status === 'proposed' && <ApplyAction run={run} proposals={proposals} enabled={enabled} />}
            {ROLLBACK_STATUSES.includes(run.status) && <RollbackAction run={run} proposals={proposals} />}
          </div>
        }
      />
      {run.status === 'proposed' && enabled === false && (
        <Note>daemon 没有开启 consolidation（consolidationEnabled），不能应用。回滚不受这个开关限制。</Note>
      )}
      {OPS.map(({ op, label }) => {
        const group = proposals.filter((p) => p.op === op)
        return (
          group.length > 0 && (
            <Card key={op} title={`${label}（${group.length}）`}>
              <ul className="space-y-3">
                {group.map((p) => (
                  <ProposalItem key={p.proposalId} p={p} memories={memories} />
                ))}
              </ul>
            </Card>
          )
        )
      })}
      {proposals.length === 0 && (
        <Card>
          <Muted>这次运行没有提议。</Muted>
        </Card>
      )}
    </div>
  )
}

function RunSummary({ run, actions }: { run: ConsolidationRun; actions: ReactNode }) {
  const l = run.limits
  return (
    <Card
      title={
        <span className="flex flex-wrap items-center gap-2">
          consolidation 运行 <span className="font-mono text-sm font-normal text-zinc-500">{run.runId}</span>
          <ReviewStatusBadge status={run.status} />
        </span>
      }
      actions={actions}
    >
      {run.error && (
        <div className="mb-3">
          <ErrorNotice error={new Error(run.error)} />
        </div>
      )}
      <div className="grid gap-4 md:grid-cols-2">
        <Fields rows={summaryRows(run)} />
        <Fields
          rows={[
            ['向量相似阈值', String(l.vectorLink)],
            ['簇大小', `${l.minCluster}–${l.maxCluster}`],
            ['每次运行最多簇', String(l.maxClustersPerRun)],
            ['合并文本上限', `${formatNumber(l.maxMergedTextBytes)} 字节`],
            ['每次最多退役', formatPercent(l.maxRetireFraction)],
          ]}
        />
      </div>
      {run.clusters.some((c) => c.error) && (
        <div className="mt-4">
          <h3 className="mb-1 text-sm font-medium">生成失败的簇</h3>
          <ul className="space-y-1 text-sm text-red-700">
            {run.clusters.filter((c) => c.error).map((c) => (
              <li key={c.index}>#{c.index}：{c.error}</li>
            ))}
          </ul>
        </div>
      )}
    </Card>
  )
}

function summaryRows(run: ConsolidationRunSummary): [string, ReactNode][] {
  const rows: [string, ReactNode][] = [
    ['项目', run.project],
    ['创建时间', formatTime(run.createdAt)],
    ['生成模型', run.llmModel || '—'],
    ['项目事实 / 可参与', `${formatNumber(run.projectFacts)} / ${formatNumber(run.eligibleFacts)}`],
    ['簇（已处理 / 共）', `${run.processedClusters} / ${run.clusterCount}`],
    ['操作（通过 / 被拒）', `${run.acceptedOps} / ${run.rejectedOps}`],
    ['退役', `${run.retireCount} 条，占项目事实 ${formatPercent(run.retireFraction)}`],
  ]
  if (run.appliedOps !== undefined) rows.push(['应用', `已应用 ${run.appliedOps}，跳过 ${run.skippedOps ?? 0}`])
  if (run.rolledBackOps !== undefined) {
    rows.push(['回滚', `已回滚 ${run.rolledBackOps}，仍保持应用 ${run.remainingAppliedOps ?? 0}`])
  }
  return rows
}

function ProposalItem({ p, memories }: { p: ConsolidationProposal; memories: MemorySummaries }) {
  return (
    <li className="rounded-md border border-zinc-200 p-3">
      <div className="mb-2 flex flex-wrap items-center gap-2 text-xs text-zinc-500">
        <ReviewStatusBadge status={p.status} />
        <span>簇 #{p.cluster}</span>
        <span className="font-mono">{p.proposalId}</span>
      </div>
      <OpBody p={p} memories={memories} />
      <div className="mt-2 space-y-1 text-xs">
        {p.rejectReason && <p className="text-red-700">被拒原因：{p.rejectReason}</p>}
        {p.skipReason && <p className="text-zinc-600">跳过原因：{p.skipReason}</p>}
        {p.rollbackSkipReason && <p className="text-amber-800">未回滚：{p.rollbackSkipReason}</p>}
        {p.applied?.mergedId && (
          <p className="text-zinc-600">
            合并记录 <MemoryLink id={p.applied.mergedId} />
          </p>
        )}
        {p.applied?.uncertaintyId && (
          <p className="text-zinc-600">
            冲突问题 <span className="font-mono">{p.applied.uncertaintyId}</span>（
            <Link to="/conflicts" className="text-sky-700 hover:underline">去冲突页</Link>）
          </p>
        )}
      </div>
    </li>
  )
}

const refs = (ids: string[], memories: MemorySummaries) => (
  <ul className="space-y-1">
    {ids.map((id) => (
      <li key={id}>
        <MemoryRef id={id} memories={memories} />
      </li>
    ))}
  </ul>
)

function OpBody({ p, memories }: { p: ConsolidationProposal; memories: MemorySummaries }) {
  switch (p.op) {
    case 'merge':
      return (
        <div className="space-y-2">
          <p className="text-xs text-zinc-500">输入</p>
          {refs(p.ids, memories)}
          <p className="text-xs text-zinc-500">合并后（与输入原文逐词比较）</p>
          <div className="rounded-md border border-zinc-200 p-2">
            <DiffView before={p.ids.map((id) => memories[id]?.text ?? '').join('\n')} after={p.text} />
          </div>
        </div>
      )
    case 'supersede':
      return (
        <div className="space-y-1">
          <p className="text-xs text-zinc-500">旧值</p>
          {refs([p.old], memories)}
          <p className="text-xs text-zinc-500">↓ 新值</p>
          {refs([p.new], memories)}
        </div>
      )
    case 'conflict':
      return (
        <div className="space-y-2">
          {refs(p.ids, memories)}
          <p className="text-sm">
            <Badge tone="red">原因</Badge> {p.reason}
          </p>
        </div>
      )
    case 'keep':
      return refs(p.ids, memories)
  }
}

// ---------------------------------------------------------------- writes

function ApplyAction({ run, proposals, enabled }: {
  run: ConsolidationRun
  proposals: ConsolidationProposal[]
  enabled: boolean | undefined
}) {
  const [open, setOpen] = useState(false)
  const refresh = useRefresh()
  const pending = proposals.filter((p) => p.status === 'proposed')
  const apply = useMutation({
    mutationFn: () => api.consolidationApply(run.runId),
    onSuccess: () => {
      refresh()
      setOpen(false)
    },
  })
  return (
    <>
      <Button
        disabled={!enabled}
        title={enabled === false ? 'daemon 没有开启 consolidation' : undefined}
        onClick={() => setOpen(true)}
      >
        应用…
      </Button>
      <Modal open={open} onOpenChange={setOpen} title="应用 consolidation" description={`应用 ${pending.length} 个通过验证的操作。`}>
        <div className="space-y-3 text-sm">
          <ul className="list-disc space-y-1 pl-5 text-zinc-700">
            {OPS.map(({ op, label, effect }) => {
              const n = pending.filter((p) => p.op === op).length
              return n > 0 && (
                <li key={op}>
                  {label} {n} 个：{effect}。
                </li>
              )
            })}
          </ul>
          <p className="text-zinc-600">每个操作单独提交；输入在提议后变了的操作会跳过并写明原因。应用后可以回滚。</p>
          {apply.error && <ErrorNotice error={apply.error} />}
          <div className="flex justify-end gap-2">
            <Button variant="secondary" onClick={() => setOpen(false)}>取消</Button>
            <Button disabled={apply.isPending} onClick={() => apply.mutate()}>
              {apply.isPending ? '应用中…' : '确认应用'}
            </Button>
          </div>
        </div>
      </Modal>
    </>
  )
}

function RollbackAction({ run, proposals }: { run: ConsolidationRun; proposals: ConsolidationProposal[] }) {
  const [open, setOpen] = useState(false)
  const refresh = useRefresh()
  const applied = proposals.filter((p) => p.status === 'applied').length
  const rollback = useMutation({
    mutationFn: () => api.consolidationRollback(run.runId),
    onSuccess: () => {
      refresh()
      setOpen(false)
    },
  })
  return (
    <>
      <Button variant="danger" onClick={() => setOpen(true)}>回滚…</Button>
      <Modal open={open} onOpenChange={setOpen} title="回滚 consolidation" description={`从新到旧撤销 ${applied} 个已应用的操作。`}>
        <div className="space-y-3 text-sm">
          <ul className="list-disc space-y-1 pl-5 text-zinc-700">
            <li>合并记录变为已取代（不删除），它的输入恢复为 current。</li>
            <li>取代关系被移除。</li>
            <li>打开的冲突问题撤回（withdrawn），成员恢复为 current。</li>
            <li>结果被人或后续运行改动过的操作（合并记录被更正、删除或再合并，问题已解决，链接已移动）保持已应用并写明原因；运行变为部分回滚，可以稍后再回滚。</li>
          </ul>
          {rollback.error && <ErrorNotice error={rollback.error} />}
          <div className="flex justify-end gap-2">
            <Button variant="secondary" onClick={() => setOpen(false)}>取消</Button>
            <Button variant="danger" disabled={rollback.isPending} onClick={() => rollback.mutate()}>
              {rollback.isPending ? '回滚中…' : '确认回滚'}
            </Button>
          </div>
        </div>
      </Modal>
    </>
  )
}
