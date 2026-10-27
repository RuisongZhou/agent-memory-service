import { useMutation, useQuery } from '@tanstack/react-query'
import { useState, type FormEvent } from 'react'
import { Link, useSearchParams } from 'react-router'
import { api } from '../api/client'
import { useRefresh } from '../api/refresh'
import type { StatedMemory, Uncertainty, UncertaintyStatus } from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { MemoryText, StatusBadge, TypeBadge } from '../components/memory'
import { Modal } from '../components/Modal'
import { Badge, Button, Card, Field, Fields, Input, MemoryLink, Muted, RefreshButton, Textarea } from '../components/ui'
import { formatNumber, formatTime, labelled, ORIGIN_LABEL, scopeText } from '../lib/format'

const LIMIT = 200

const TABS: [UncertaintyStatus, string][] = [
  ['open', '未解决'],
  ['resolved', '已解决'],
  ['withdrawn', '已撤回'],
]

const statusOf = (value: string | null): UncertaintyStatus =>
  TABS.some(([s]) => s === value) ? (value as UncertaintyStatus) : 'open'

export function ConflictsPage() {
  const [params, setParams] = useSearchParams()
  const status = statusOf(params.get('status'))
  const project = params.get('project') ?? ''
  const go = (set: { status?: UncertaintyStatus; project?: string }) => {
    const next = { status, project, ...set }
    setParams(
      { ...(next.status !== 'open' && { status: next.status }), ...(next.project && { project: next.project }) },
      { replace: true },
    )
  }
  const list = useQuery({
    queryKey: ['uncertainties', { status, project }],
    queryFn: () => api.listUncertainties({ status, ...(project && { project }), limit: LIMIT }),
  })
  const filter = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    go({ project: String(new FormData(e.currentTarget).get('project')).trim() })
  }
  const rows = list.data?.uncertainties ?? []

  return (
    <div className="space-y-4">
      <Card>
        <div className="flex flex-wrap items-end justify-between gap-3">
          <div role="tablist" className="inline-flex overflow-hidden rounded-md border border-zinc-300">
            {TABS.map(([s, label]) => (
              <button
                key={s}
                type="button"
                role="tab"
                aria-selected={s === status}
                onClick={() => go({ status: s })}
                className={`px-3 py-1.5 text-sm ${s === status ? 'bg-zinc-900 text-white' : 'bg-white text-zinc-700 hover:bg-zinc-50'}`}
              >
                {label}
              </button>
            ))}
          </div>
          <form key={project} onSubmit={filter} className="flex flex-wrap items-end gap-2">
            <Field label="项目">
              <Input name="project" defaultValue={project} placeholder="全部项目，回车筛选" />
            </Field>
            <Button type="submit" variant="secondary">筛选</Button>
            <RefreshButton busy={list.isFetching} onClick={() => list.refetch()} />
          </form>
        </div>
        <p className="mt-3 text-sm text-zinc-500">
          冲突是互相矛盾、需要人来决定的记忆。成员处于“冲突”状态，检索时仍会返回；解决时选一条保留，其余变为已取代。
        </p>
      </Card>
      {list.error && <ErrorNotice error={list.error} />}
      {list.isPending ? (
        <Muted>加载中…</Muted>
      ) : rows.length === 0 ? (
        <Card>
          <Muted>没有{TABS.find(([s]) => s === status)?.[1]}的冲突。</Muted>
        </Card>
      ) : (
        <>
          <Muted>
            {formatNumber(rows.length)} 个问题，从新到旧{rows.length === LIMIT && `（只显示最新 ${LIMIT} 个）`}
          </Muted>
          {rows.map((u) => <Question key={u.id} u={u} />)}
        </>
      )}
    </div>
  )
}

function Question({ u }: { u: Uncertainty }) {
  const [keep, setKeep] = useState('')
  const [reason, setReason] = useState('')
  const [confirming, setConfirming] = useState(false)
  const byId = new Map(u.memories.map((m) => [m.id, m]))
  const open = u.status === 'open'

  return (
    <Card
      title={
        <span className="flex flex-wrap items-center gap-2">
          <Badge tone={open ? 'red' : 'gray'}>{TABS.find(([s]) => s === u.status)?.[1] ?? u.status}</Badge>
          <span className="text-sm font-normal text-zinc-600">{scopeText(u.scope)}</span>
          <span className="font-mono text-xs font-normal text-zinc-400">{u.id}</span>
        </span>
      }
      actions={<span className="text-xs text-zinc-500">{formatTime(u.createdAt)}</span>}
    >
      <p className="mb-3 text-sm">
        <span className="text-zinc-500">原因：</span>
        {u.reason || '—'}
      </p>
      <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
        {u.memoryIds.map((id) => (
          <Member
            key={id}
            id={id}
            memory={byId.get(id)}
            kept={u.resolution?.keepMemoryId === id}
            choose={open && byId.has(id) ? { chosen: keep === id, set: () => setKeep(id), name: u.id } : undefined}
          />
        ))}
      </div>
      {u.resolution && (
        <div className="mt-3">
          <Fields
            rows={[
              ['解决时间', formatTime(u.resolvedAt)],
              ['保留', <MemoryLink id={u.resolution.keepMemoryId} />],
              ['平台', u.resolution.platform],
              ['理由', u.resolution.reason || '—'],
            ]}
          />
        </div>
      )}
      {u.status === 'withdrawn' && (
        <div className="mt-3">
          <Fields
            rows={[
              ['撤回时间', formatTime(u.withdrawnAt)],
              [
                '撤回者',
                u.withdrawnBy ? (
                  <Link to={`/consolidation/${u.withdrawnBy}`} className="font-mono text-sky-700 hover:underline">
                    {u.withdrawnBy}
                  </Link>
                ) : (
                  '—'
                ),
              ],
            ]}
          />
        </div>
      )}
      {open && (
        <div className="mt-4 space-y-3 border-t border-zinc-100 pt-3">
          <Field label="理由" hint="记在问题上，说明为什么保留这条">
            <Textarea className="min-h-16" value={reason} onChange={(e) => setReason(e.target.value)} />
          </Field>
          <div className="flex justify-end">
            <Button disabled={!keep || !reason.trim()} onClick={() => setConfirming(true)}>
              {keep ? '解决…' : '先选择保留哪一条'}
            </Button>
          </div>
          {keep && (
            <ResolveDialog
              u={u}
              keep={keep}
              reason={reason.trim()}
              open={confirming}
              onOpenChange={setConfirming}
            />
          )}
        </div>
      )}
    </Card>
  )
}

function Member({ id, memory, kept, choose }: {
  id: string
  memory?: StatedMemory
  kept: boolean
  choose?: { chosen: boolean; set: () => void; name: string }
}) {
  if (!memory) {
    return (
      <div className="rounded-md border border-dashed border-zinc-300 p-3 text-sm">
        <span className="font-mono text-zinc-500">{id}</span> <Badge>已删除</Badge>
      </div>
    )
  }
  return (
    <div
      className={`rounded-md border p-3 ${choose?.chosen || kept ? 'border-emerald-400 ring-1 ring-emerald-400' : 'border-zinc-200'}`}
    >
      <div className="mb-2 flex flex-wrap items-center gap-2">
        <MemoryLink id={id} />
        <TypeBadge type={memory.type} />
        <StatusBadge status={memory.status} />
        {kept && <Badge tone="green">保留了这条</Badge>}
      </div>
      <MemoryText memory={memory} />
      <div className="mt-2 text-xs">
        <Fields
          rows={[
            ['来源', labelled(ORIGIN_LABEL, memory.source.origin)],
            ['写入平台', memory.source.platform],
            ['生效时间', formatTime(memory.effectiveAt)],
            ['说出时间', formatTime(memory.statedAt)],
            ['记录时间', formatTime(memory.recordedAt)],
          ]}
        />
      </div>
      {choose && (
        <label className="mt-3 inline-flex items-center gap-1.5 text-sm font-medium">
          <input type="radio" name={choose.name} checked={choose.chosen} onChange={choose.set} className="size-4 accent-zinc-800" />
          保留这条
        </label>
      )}
    </div>
  )
}

function ResolveDialog({ u, keep, reason, open, onOpenChange }: {
  u: Uncertainty
  keep: string
  reason: string
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const refresh = useRefresh()
  const others = u.memoryIds.filter((id) => id !== keep)
  const resolve = useMutation({
    mutationFn: () => api.resolveUncertainty({ id: u.id, keepMemoryId: keep, reason }),
    onSuccess: () => {
      refresh()
      onOpenChange(false)
    },
  })
  return (
    <Modal open={open} onOpenChange={onOpenChange} title="解决冲突" description="原文和证据都会保留，但这一步不能撤销。">
      <div className="space-y-3 text-sm">
        <ul className="list-disc space-y-1 pl-5 text-zinc-700">
          <li>
            保留 <span className="font-mono">{keep}</span>：恢复为 current；它若还在其他未解决的冲突里，则继续保持冲突状态。
          </li>
          <li>
            其余 {others.length} 条（{others.map((id) => <span key={id} className="mr-1 font-mono">{id}</span>)}）变为已取代，失效时间为现在。
          </li>
          <li>问题记为已解决，理由：{reason}</li>
        </ul>
        {resolve.error && <ErrorNotice error={resolve.error} />}
        <div className="flex justify-end gap-2">
          <Button variant="secondary" onClick={() => onOpenChange(false)}>取消</Button>
          <Button disabled={resolve.isPending} onClick={() => resolve.mutate()}>
            {resolve.isPending ? '提交中…' : '确认解决'}
          </Button>
        </div>
      </div>
    </Modal>
  )
}
