import { useMutation, useQuery } from '@tanstack/react-query'
import { useState, type FormEvent, type ReactNode } from 'react'
import { Link, useParams, useSearchParams } from 'react-router'
import { api } from '../api/client'
import { useRefresh } from '../api/refresh'
import type { ImportProposal, ImportRun, ImportUnitOutcome } from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { ReviewStatusBadge } from '../components/memory'
import { Modal } from '../components/Modal'
import { Badge, Button, Card, Field, Fields, Input, MemoryLink, Muted, Note, RefreshButton, TD, TH } from '../components/ui'
import {
  EVIDENCE_ROLE_LABEL,
  formatNumber,
  formatTime,
  ITEM_KIND_LABEL,
  labelled,
  UNIT_OUTCOME_LABEL,
} from '../lib/format'
import { importDecisions, pendingSessionTurns, SESSION_TURN } from '../lib/review'

const RUN_LIMIT = 50
const ROLLBACK_STATUSES = ['applied', 'partially_rolled_back', 'rolling_back']

const runLink = (runId: string) => (
  <Link to={`/imports/${runId}`} className="font-mono text-sm text-sky-700 hover:underline">{runId}</Link>
)

// ---------------------------------------------------------------- run list

export function ImportsPage() {
  const [params, setParams] = useSearchParams()
  const project = params.get('project') ?? ''
  const runs = useQuery({
    queryKey: ['imports', 'runs', project],
    queryFn: () => api.importRuns({ ...(project && { project }), limit: RUN_LIMIT }),
  })
  const filter = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    const value = String(new FormData(e.currentTarget).get('project')).trim()
    setParams(value ? { project: value } : {}, { replace: true })
  }

  return (
    <div className="space-y-4">
      <Card>
        <form key={project} onSubmit={filter} className="flex flex-wrap items-end gap-2">
          <Field label="项目">
            <Input name="project" defaultValue={project} placeholder="全部项目，回车筛选" />
          </Field>
          <Button type="submit" variant="secondary">筛选</Button>
        </form>
        <p className="mt-3 text-sm text-zinc-500">
          导入由 <code>agent-memoryd import</code> 或 MCP 的 <code>propose_import</code> 发起；这里只审核、应用和回滚，不发起导入。
        </p>
      </Card>
      {runs.error && <ErrorNotice error={runs.error} />}
      <Card
        title={`导入运行（最近 ${RUN_LIMIT} 次）`}
        actions={<RefreshButton busy={runs.isFetching} onClick={() => runs.refetch()} />}
      >
        {runs.isPending ? (
          <Muted>加载中…</Muted>
        ) : !runs.data?.runs.length ? (
          <Muted>没有导入运行。</Muted>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr>
                  <TH>运行</TH>
                  <TH>状态</TH>
                  <TH>项目</TH>
                  <TH>来源</TH>
                  <TH>创建时间</TH>
                  <TH className="text-right">单元</TH>
                  <TH className="text-right">提议</TH>
                  <TH className="text-right">已应用</TH>
                  <TH className="text-right">会话轮次</TH>
                  <TH>错误</TH>
                </tr>
              </thead>
              <tbody>
                {runs.data.runs.map((r) => (
                  <tr key={r.runId} className="border-t border-zinc-100">
                    <TD>{runLink(r.runId)}</TD>
                    <TD><ReviewStatusBadge status={r.status} /></TD>
                    <TD>{r.project}</TD>
                    <TD>{r.host}</TD>
                    <TD className="whitespace-nowrap">{formatTime(r.createdAt)}</TD>
                    <TD className="text-right tabular-nums">{formatNumber(r.counts.units)}</TD>
                    <TD className="text-right tabular-nums">{formatNumber(r.counts.proposals)}</TD>
                    <TD className="text-right tabular-nums">{formatNumber(r.counts.applied)}</TD>
                    <TD className="text-right tabular-nums">{formatNumber(r.counts.sessionTurns)}</TD>
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

// ---------------------------------------------------------------- one run

export function ImportRunPage() {
  const { runId } = useParams() as { runId: string }
  // A new run is a new page: decisions and dialogs belong to one run.
  return <ImportRunDetail key={runId} runId={runId} />
}

function ImportRunDetail({ runId }: { runId: string }) {
  const query = useQuery({ queryKey: ['imports', 'run', runId], queryFn: () => api.importRun(runId) })
  const [rejected, setRejected] = useState<ReadonlySet<string>>(new Set())

  if (query.error) return <ErrorNotice error={query.error} />
  if (!query.data) return <Muted>加载中…</Muted>
  const { run, proposals } = query.data
  const reviewing = run.status === 'proposed'
  const items = proposals.filter((p) => p.itemKind !== SESSION_TURN)
  const turns = proposals.filter((p) => p.itemKind === SESSION_TURN)
  const decide = (id: string, reject: boolean) =>
    setRejected((old) => {
      const next = new Set(old)
      if (reject) next.add(id)
      else next.delete(id)
      return next
    })

  return (
    <div className="space-y-4">
      <RunSummary
        run={run}
        actions={
          <div className="flex flex-wrap gap-2">
            <RefreshButton busy={query.isFetching} onClick={() => query.refetch()} />
            {reviewing && <ApplyAction run={run} proposals={proposals} rejected={rejected} />}
            {ROLLBACK_STATUSES.includes(run.status) && <RollbackAction run={run} proposals={proposals} />}
          </div>
        }
      />
      {reviewing && pendingSessionTurns(proposals) > 0 && (
        <Note>
          这次运行有 {pendingSessionTurns(proposals)} 个会话轮次还没应用，网页不能应用它。<code>import_apply</code>{' '}
          只把这些轮次标为已排队，随后由发起导入的客户端从它的对话发件箱（outbox）提交；网页没有发件箱，在这里应用会让轮次永远不会提交。请用发起导入的{' '}
          <code>agent-memoryd import</code> 或 MCP 的 <code>apply_import</code> 应用。回滚不受影响。
        </Note>
      )}
      <Proposals
        items={items}
        units={run.units}
        reviewing={reviewing}
        rejected={rejected}
        decide={decide}
        setRejected={setRejected}
      />
      {turns.length > 0 && <SessionTurns turns={turns} />}
    </div>
  )
}

function RunSummary({ run, actions }: { run: ImportRun; actions: ReactNode }) {
  const c = run.counts
  const failedUnits = run.units.filter((u) => u.outcome === 'failed')
  return (
    <Card
      title={
        <span className="flex flex-wrap items-center gap-2">
          导入运行 <span className="font-mono text-sm font-normal text-zinc-500">{run.runId}</span>
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
        <Fields
          rows={[
            ['项目', run.project],
            ['来源', run.host],
            ['创建时间', formatTime(run.createdAt)],
            ['更新时间', formatTime(run.updatedAt)],
            ['提议完成', formatTime(run.finishedAt)],
            ['生成模型', run.llmModel || '—'],
            ['嵌入模型', run.embeddingModel || '—'],
            ['确认方式', run.confirmedVia === 'chat' ? '在对话中确认（chat）' : (run.confirmedVia ?? '—')],
          ]}
        />
        <Fields
          rows={[
            ['单元', `${formatNumber(c.units)}（未变化 ${formatNumber(c.unchanged)}，失败 ${formatNumber(c.failed)}）`],
            ['提议', `${formatNumber(c.proposals)}（被拒 ${formatNumber(c.rejected)}，重复 ${formatNumber(c.duplicates)}）`],
            ['生成调用', `${formatNumber(c.generationCalls)}（缓存命中 ${formatNumber(c.cacheHits)}）`],
            ['应用', `已应用 ${formatNumber(c.applied)}，未采纳 ${formatNumber(c.dropped)}，跳过 ${formatNumber(c.skipped)}`],
            ['已回滚', formatNumber(c.rolledBack)],
            [
              '会话轮次',
              `${formatNumber(c.sessionTurns)}（已排队 ${formatNumber(c.turnsQueued)}，已回滚 ${formatNumber(c.turnsRolledBack)}）`,
            ],
          ]}
        />
      </div>
      {failedUnits.length > 0 && (
        <div className="mt-4">
          <h3 className="mb-1 text-sm font-medium">抽取失败的单元（下次导入会重新提议）</h3>
          <ul className="space-y-1 text-sm">
            {failedUnits.map((u) => (
              <li key={u.unit}>
                <span className="font-mono text-xs">{u.unit}</span> <span className="text-red-700">{u.error}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </Card>
  )
}

/** Proposals in run order, grouped by the unit they came from. */
function groupByUnit(items: ImportProposal[]): [string, ImportProposal[]][] {
  const groups = new Map<string, ImportProposal[]>()
  for (const p of items) groups.set(p.unit, [...(groups.get(p.unit) ?? []), p])
  return [...groups]
}

function Proposals({ items, units, reviewing, rejected, decide, setRejected }: {
  items: ImportProposal[]
  units: ImportUnitOutcome[]
  reviewing: boolean
  rejected: ReadonlySet<string>
  decide: (id: string, reject: boolean) => void
  setRejected: (set: ReadonlySet<string>) => void
}) {
  const open = items.filter((p) => p.status === 'proposed')
  const outcome = new Map(units.map((u) => [u.unit, u]))
  return (
    <Card
      title={`提议（${formatNumber(items.length)}）`}
      actions={
        reviewing &&
        open.length > 0 && (
          <div className="flex flex-wrap gap-2">
            <Button variant="secondary" onClick={() => setRejected(new Set())}>全部接受</Button>
            <Button variant="secondary" onClick={() => setRejected(new Set(open.map((p) => p.proposalId)))}>全部拒绝</Button>
          </div>
        )
      }
    >
      {items.length === 0 ? (
        <Muted>没有提议。</Muted>
      ) : (
        <div className="space-y-5">
          {groupByUnit(items).map(([unit, group]) => (
            <UnitGroup key={unit} unit={unit} outcome={outcome.get(unit)}>
              {group.map((p) => (
                <ProposalItem
                  key={p.proposalId}
                  p={p}
                  decision={
                    reviewing && p.status === 'proposed'
                      ? { reject: rejected.has(p.proposalId), set: (reject) => decide(p.proposalId, reject) }
                      : undefined
                  }
                />
              ))}
            </UnitGroup>
          ))}
        </div>
      )}
    </Card>
  )
}

function UnitGroup({ unit, outcome, children }: { unit: string; outcome?: ImportUnitOutcome; children: ReactNode }) {
  return (
    <section>
      <header className="mb-2 flex flex-wrap items-center gap-2 text-xs text-zinc-500">
        <span className="font-mono break-all">{unit}</span>
        {outcome && (
          <>
            <Badge>{outcome.mode}</Badge>
            <Badge>{labelled(UNIT_OUTCOME_LABEL, outcome.outcome)}</Badge>
            {outcome.coverage != null && <span>证据覆盖 {(outcome.coverage * 100).toFixed(0)}%</span>}
            {outcome.cached && <span>抽取来自缓存</span>}
          </>
        )}
      </header>
      {outcome?.previousRecords && outcome.previousRecords.length > 0 && (
        <div className="mb-2 rounded-md bg-zinc-50 p-2 text-sm">
          <p className="mb-1 text-xs text-zinc-500">
            来源已变化。上次导入写入的这些记录没有被本次提议取代，会保持 current，除非手动撤回：
          </p>
          <ul className="space-y-1">
            {outcome.previousRecords.map((r) => (
              <li key={r.memoryId}>
                <MemoryLink id={r.memoryId} /> <span className="text-zinc-700">{r.text}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
      <ul className="space-y-2">{children}</ul>
    </section>
  )
}

function ProposalItem({ p, decision }: {
  p: ImportProposal
  decision?: { reject: boolean; set: (reject: boolean) => void }
}) {
  const reason = p.rejectReason ?? p.skipReason ?? null
  return (
    <li className={`rounded-md border p-3 ${decision?.reject ? 'border-red-200 bg-red-50/40' : 'border-zinc-200'}`}>
      <div className="mb-2 flex flex-wrap items-center gap-2 text-xs text-zinc-500">
        <ReviewStatusBadge status={p.status} />
        <Badge tone="blue">{labelled(ITEM_KIND_LABEL, p.itemKind)}</Badge>
        <span>范围提示 {p.scopeHint}</span>
        {p.statedAt && <span>说出于 {formatTime(p.statedAt)}</span>}
        {p.headingPath.length > 0 && <span>{p.headingPath.join(' › ')}</span>}
        {decision && (
          <span className="ml-auto inline-flex overflow-hidden rounded-md border border-zinc-300">
            <DecisionButton active={!decision.reject} onClick={() => decision.set(false)}>接受</DecisionButton>
            <DecisionButton active={decision.reject} danger onClick={() => decision.set(true)}>拒绝</DecisionButton>
          </span>
        )}
      </div>
      <p className="text-sm whitespace-pre-wrap text-zinc-900">{p.text}</p>
      {p.evidence && p.evidence !== p.text && (
        <p className="mt-1 text-xs whitespace-pre-wrap text-zinc-500">证据：{p.evidence}</p>
      )}
      <div className="mt-2 space-y-1 text-xs text-zinc-600">
        {reason && <p className="text-red-700">{p.rejectReason ? '被拒原因' : '跳过原因'}：{reason}</p>}
        {p.rollbackSkipReason && <p className="text-amber-800">未回滚：{p.rollbackSkipReason}</p>}
        {p.duplicateOf && (
          <p>
            与已有导入记录相同 <MemoryLink id={p.duplicateOf} />：接受时只把单元链接到它，不写新记录。
          </p>
        )}
        {p.supersedes && (
          <p>
            取代上次导入的记录 <MemoryLink id={p.supersedes} />
          </p>
        )}
        {p.memoryId && (
          <p>
            写入 / 链接的记录 <MemoryLink id={p.memoryId} />
          </p>
        )}
        {p.similar.length > 0 && (
          <div>
            <p>相似的已有记忆：</p>
            <ul className="mt-0.5 space-y-0.5 pl-3">
              {p.similar.map((s) => (
                <li key={s.memoryId}>
                  <span className="tabular-nums">{s.score.toFixed(3)}</span> <MemoryLink id={s.memoryId} />{' '}
                  <span className="text-zinc-700">{s.text}</span>
                </li>
              ))}
            </ul>
          </div>
        )}
      </div>
    </li>
  )
}

function DecisionButton({ active, danger, onClick, children }: {
  active: boolean
  danger?: boolean
  onClick: () => void
  children: ReactNode
}) {
  const on = danger ? 'bg-red-600 text-white' : 'bg-zinc-900 text-white'
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onClick}
      className={`px-2.5 py-1 text-xs font-medium ${active ? on : 'bg-white text-zinc-700 hover:bg-zinc-50'}`}
    >
      {children}
    </button>
  )
}

function SessionTurns({ turns }: { turns: ImportProposal[] }) {
  return (
    <Card title={`会话轮次（${formatNumber(turns.length)}）`}>
      <Muted>过去的会话轮次不是待审核的条目：应用后由发起导入的客户端提交，daemon 像捕获的对话一样从中形成事实。</Muted>
      <ul className="mt-3 space-y-3">
        {turns.map((p) => {
          const applied = (p.applied ?? {}) as { requestId?: string; duplicate?: boolean }
          return (
            <li key={p.proposalId} className="rounded-md border border-zinc-200 p-3">
              <div className="mb-2 flex flex-wrap items-center gap-2 text-xs text-zinc-500">
                <ReviewStatusBadge status={p.status} />
                {p.turn && (
                  <span>
                    会话 {p.turn.session} · <span className="font-mono">{p.turn.nativeId}</span>
                  </span>
                )}
                {applied.requestId && (
                  <Link to={`/tasks/${applied.requestId}`} className="font-mono text-sky-700 hover:underline">
                    {applied.requestId}
                  </Link>
                )}
                {applied.duplicate && <Badge>daemon 已有这轮</Badge>}
              </div>
              {p.turn?.messages.map((m, i) => (
                <p key={i} className={m.role === 'user' ? 'rounded-md bg-zinc-50 p-2 text-sm' : 'p-2 text-sm'}>
                  <span className="mr-2 text-xs text-zinc-500">
                    {EVIDENCE_ROLE_LABEL[m.role] ?? m.role} · {formatTime(m.timestamp)}
                  </span>
                  <span className="whitespace-pre-wrap">{m.content}</span>
                </p>
              ))}
              {p.rejectReason && <p className="mt-1 text-xs text-red-700">被拒原因：{p.rejectReason}</p>}
              {p.rollbackSkipReason && <p className="mt-1 text-xs text-amber-800">未回滚：{p.rollbackSkipReason}</p>}
            </li>
          )
        })}
      </ul>
    </Card>
  )
}

// ---------------------------------------------------------------- writes

function ApplyAction({ run, proposals, rejected }: {
  run: ImportRun
  proposals: ImportProposal[]
  rejected: ReadonlySet<string>
}) {
  const [open, setOpen] = useState(false)
  const refresh = useRefresh()
  const decisions = importDecisions(proposals, rejected)
  const values = Object.values(decisions)
  const accepted = values.filter((d) => d === 'accept').length
  const apply = useMutation({
    mutationFn: () => api.importApply({ runId: run.runId, decisions }),
    onSuccess: () => {
      refresh()
      setOpen(false)
    },
  })
  const blocked = pendingSessionTurns(proposals) > 0
  return (
    <>
      <Button disabled={blocked} title={blocked ? '有未应用的会话轮次，见下方说明' : undefined} onClick={() => setOpen(true)}>
        应用…
      </Button>
      <Modal
        open={open}
        onOpenChange={setOpen}
        title="应用导入"
        description={`接受 ${accepted} 条，拒绝 ${values.length - accepted} 条。`}
      >
        <div className="space-y-3 text-sm">
          <ul className="list-disc space-y-1 pl-5 text-zinc-700">
            <li>接受的提议写成项目级的导入事实（来源：导入）；与已有记录相同的只把单元链接到那条记录。</li>
            <li>取代上次导入记录的提议会把旧记录标为已取代。</li>
            <li>拒绝的提议标为未采纳，不写任何记录。</li>
            <li>应用时逐条重新检查，来源已变化的提议会跳过并写明原因。</li>
            <li>应用后可以回滚。</li>
          </ul>
          {apply.error && <ErrorNotice error={apply.error} />}
          <div className="flex justify-end gap-2">
            <Button variant="secondary" onClick={() => setOpen(false)}>取消</Button>
            <Button disabled={apply.isPending} onClick={() => apply.mutate()}>
              {apply.isPending ? '应用中…' : `确认应用（接受 ${accepted} 条）`}
            </Button>
          </div>
        </div>
      </Modal>
    </>
  )
}

function RollbackAction({ run, proposals }: { run: ImportRun; proposals: ImportProposal[] }) {
  const [open, setOpen] = useState(false)
  const refresh = useRefresh()
  const applied = proposals.filter((p) => p.status === 'applied')
  const turns = applied.filter((p) => p.itemKind === SESSION_TURN).length
  const rollback = useMutation({
    mutationFn: () => api.importRollback(run.runId),
    onSuccess: () => {
      refresh()
      setOpen(false)
    },
  })
  return (
    <>
      <Button variant="danger" onClick={() => setOpen(true)}>回滚…</Button>
      <Modal
        open={open}
        onOpenChange={setOpen}
        title="回滚导入"
        description={`撤销 ${applied.length - turns} 条已应用的提议和 ${turns} 个会话轮次。`}
      >
        <div className="space-y-3 text-sm">
          <ul className="list-disc space-y-1 pl-5 text-zinc-700">
            <li>本次写入的记录变为已取代（不删除），被它取代的旧记录恢复为 current；链接到已有记录的只解除链接。</li>
            <li>会话轮次：这次运行不再接收新的轮次；只由本次运行的请求形成的事实变为已取代。</li>
            <li>之后被人或其他运行改动过的提议保持已应用，并写明原因；运行变为部分回滚，可以稍后再回滚。</li>
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
