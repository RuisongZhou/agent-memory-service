import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState, type ReactNode } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { api } from '../api/client'
import { useRefresh } from '../api/refresh'
import type { DeleteMemoryRes, Evidence, SourceBatch, StatedMemory } from '../api/types'
import { DiffView } from '../components/DiffView'
import { ErrorNotice } from '../components/ErrorNotice'
import { MemoryText, StatusBadge, TypeBadge } from '../components/memory'
import { Modal } from '../components/Modal'
import { Badge, Button, Card, Field, Fields, Input, MemoryLink, Muted, Pre, Textarea } from '../components/ui'
import {
  byteLength,
  EVIDENCE_ROLE_LABEL,
  formatTime,
  labelled,
  ORIGIN_LABEL,
  RELATION_LABEL,
  scopeText,
} from '../lib/format'
import { MemoryEntities } from './Entities'
import { Lineage } from './Lineage'
import { RecentRecalls } from './Recalls'

const MAX_TEXT_BYTES = 4096

const dash = (value: ReactNode) => value || '—'

const ids = (list: string[]) =>
  list.length ? (
    <span className="flex flex-wrap gap-x-3">
      {list.map((id) => <MemoryLink key={id} id={id} />)}
    </span>
  ) : (
    '—'
  )

export function MemoryPage() {
  const { id } = useParams() as { id: string }
  // A new id is a new page: dialogs and the deletion result belong to one record.
  return <MemoryDetail key={id} id={id} />
}

function MemoryDetail({ id }: { id: string }) {
  const [deleted, setDeleted] = useState<DeleteMemoryRes>()
  const query = useQuery({ queryKey: ['memory', id], queryFn: () => api.getMemory(id), enabled: !deleted })

  if (deleted) return <DeleteResult result={deleted} />
  if (query.error) return <ErrorNotice error={query.error} />
  if (!query.data) return <Muted>加载中…</Muted>
  const { memory: m, history, evidence, sourceBatch } = query.data

  return (
    <div className="space-y-4">
      <Card
        title={
          <span className="flex flex-wrap items-center gap-2">
            <TypeBadge type={m.type} />
            <StatusBadge status={m.status} />
            <span className="font-mono text-sm font-normal text-zinc-500">{m.id}</span>
          </span>
        }
        actions={<Actions memory={m} onDeleted={setDeleted} />}
      >
        <MemoryText memory={m} />
        <div className="mt-4">
          <Fields rows={recordFields(m)} />
        </div>
      </Card>
      <MemoryEntities id={m.id} />
      {history.length > 1 && <History history={history} viewing={m.id} />}
      <Lineage id={m.id} />
      <Provenance evidence={evidence} sourceBatch={sourceBatch} />
      {m.source.turnIds.length > 0 && <Turns turnIds={m.source.turnIds} />}
      <RecentRecalls id={m.id} />
    </div>
  )
}

function recordFields(m: StatedMemory): [string, ReactNode][] {
  const rows: [string, ReactNode][] = [
    ['范围', scopeText(m.scope)],
    ['写入平台', m.source.platform],
    ['来源', labelled(ORIGIN_LABEL, m.source.origin)],
    ['对话轮次', dash(m.source.turnIds.join('、'))],
    ['生效时间', formatTime(m.effectiveAt)],
    ['说出时间', formatTime(m.statedAt)],
    ['记录时间', formatTime(m.recordedAt)],
    ['失效时间', formatTime(m.validUntil)],
    ['取代了', m.supersedes ? <MemoryLink id={m.supersedes} /> : '—'],
    ['被取代于', m.supersededBy ? <MemoryLink id={m.supersededBy} /> : '—'],
    [
      '关联',
      m.related.length ? (
        <span className="flex flex-wrap gap-x-3">
          {m.related.map((r) => (
            <span key={r.id + r.relation}>
              {RELATION_LABEL[r.relation] ?? r.relation} <MemoryLink id={r.id} />
            </span>
          ))}
        </span>
      ) : (
        '—'
      ),
    ],
  ]
  if (m.derivation) {
    rows.push(
      ['派生阶段', `${m.derivation.stage}${m.derivation.currentView ? '（当前视图，输入变化时失效）' : ''}`],
      ['派生自记忆', ids(m.derivation.inputMemoryIds)],
      ['派生自轮次', dash(m.derivation.inputTurnIds.join('、'))],
    )
  }
  if (m.staleReason) rows.push(['失效原因', m.staleReason])
  if (m.detail != null) rows.push(['结构化内容', <Pre>{JSON.stringify(m.detail, null, 2)}</Pre>])
  return rows
}

function History({ history, viewing }: { history: StatedMemory[]; viewing: string }) {
  return (
    <Card title="更正链（从旧到新）">
      <ol className="space-y-4 border-l-2 border-zinc-200 pl-4">
        {history.map((h, i) => (
          <li key={h.id} className={h.id === viewing ? 'rounded-md bg-sky-50 p-2' : 'p-2'}>
            <div className="mb-1 flex flex-wrap items-center gap-2 text-xs text-zinc-500">
              {h.id === viewing ? <Badge tone="blue">本条</Badge> : <MemoryLink id={h.id} />}
              <StatusBadge status={h.status} />
              <span>
                生效 {formatTime(h.effectiveAt)}
                {h.validUntil && ` 至 ${formatTime(h.validUntil)}`}
              </span>
              <span>记录 {formatTime(h.recordedAt)}</span>
              <span>{labelled(ORIGIN_LABEL, h.source.origin)} · {h.source.platform}</span>
            </div>
            {i === 0 ? (
              <p className="text-sm whitespace-pre-wrap">{h.text}</p>
            ) : (
              <DiffView before={history[i - 1].text} after={h.text} />
            )}
          </li>
        ))}
      </ol>
    </Card>
  )
}

function Provenance({ evidence, sourceBatch }: { evidence: Evidence[]; sourceBatch?: SourceBatch }) {
  return (
    <Card title="出处">
      {sourceBatch && (
        <div className="mb-4">
          <h3 className="mb-1 text-sm font-medium">来源批次</h3>
          <Muted>整批输入是这条记忆的来源，不是逐条核实过的原话引用。</Muted>
          <div className="mt-2">
            <Fields
              rows={[
                ['级别', sourceBatch.level],
                ['输入请求', dash(sourceBatch.inputRequestIds.join('、'))],
                ['观察时间', formatTime(sourceBatch.observedAt)],
                ['归属', sourceBatch.attributedTo == null ? '—' : JSON.stringify(sourceBatch.attributedTo)],
              ]}
            />
          </div>
        </div>
      )}
      <h3 className="mb-1 text-sm font-medium">证据（{evidence.length}）</h3>
      {evidence.length === 0 ? (
        <Muted>没有证据记录。</Muted>
      ) : (
        <ul className="space-y-3">
          {evidence.map((e) => (
            <li key={e.id} className="rounded-md border border-zinc-100 p-2 text-sm">
              <div className="mb-1 flex flex-wrap items-center gap-2 text-xs text-zinc-500">
                <Badge>{EVIDENCE_ROLE_LABEL[e.role] ?? e.role}</Badge>
                <span>{formatTime(e.statedAt)}</span>
                <span>{e.platform}</span>
                {e.turnId && <span className="font-mono">{e.turnId}</span>}
              </div>
              {e.context && <p className="mb-1 text-zinc-500">问：{e.context.span}</p>}
              <p className="whitespace-pre-wrap">{e.span}</p>
            </li>
          ))}
        </ul>
      )}
    </Card>
  )
}

function Turns({ turnIds }: { turnIds: string[] }) {
  const turns = useQuery({ queryKey: ['turns', turnIds], queryFn: () => api.listTurns(turnIds) })
  return (
    <Card title="原始对话">
      {turns.error && <ErrorNotice error={turns.error} />}
      {turns.data?.turns.length === 0 && <Muted>没有找到这些轮次的原始输入。</Muted>}
      <div className="space-y-4">
        {turns.data?.turns.map((t) => (
          <div key={t.requestId}>
            <p className="mb-1 text-xs text-zinc-500">
              <span className="font-mono">{t.turnId}</span> · {t.platform} · {t.project}/{t.session} · 抽取状态 {t.extractState}
            </p>
            <ul className="space-y-1">
              {t.messages.map((msg, i) => (
                <li key={i} className={msg.role === 'user' ? 'rounded-md bg-zinc-50 p-2 text-sm' : 'p-2 text-sm'}>
                  <span className="mr-2 text-xs text-zinc-500">
                    {EVIDENCE_ROLE_LABEL[msg.role] ?? msg.role} · {formatTime(msg.timestamp)}
                  </span>
                  <span className="whitespace-pre-wrap">{msg.content}</span>
                </li>
              ))}
            </ul>
          </div>
        ))}
      </div>
    </Card>
  )
}

// ---------------------------------------------------------------- actions

type Dialog = 'correct' | 'retract' | 'delete' | null

function Actions({ memory, onDeleted }: { memory: StatedMemory; onDeleted: (result: DeleteMemoryRes) => void }) {
  const [open, setOpen] = useState<Dialog>(null)
  const dialog = (name: Exclude<Dialog, null>) => ({
    open: open === name,
    onOpenChange: (on: boolean) => setOpen(on ? name : null),
  })
  return (
    <div className="flex gap-2">
      <Button variant="secondary" onClick={() => setOpen('correct')}>更正</Button>
      <Button variant="secondary" onClick={() => setOpen('retract')}>撤回</Button>
      <Button variant="danger" onClick={() => setOpen('delete')}>删除</Button>
      <CorrectDialog memory={memory} {...dialog('correct')} />
      <RetractDialog memory={memory} {...dialog('retract')} />
      <DeleteDialog memory={memory} onDeleted={onDeleted} {...dialog('delete')} />
    </div>
  )
}

interface DialogProps {
  memory: StatedMemory
  open: boolean
  onOpenChange: (open: boolean) => void
}

function CorrectDialog({ memory, open, onOpenChange }: DialogProps) {
  const [text, setText] = useState(memory.text)
  const [reason, setReason] = useState('')
  const [effectiveAt, setEffectiveAt] = useState('')
  const navigate = useNavigate()
  const refresh = useRefresh()
  const next = text.trim()
  const bytes = byteLength(next)
  const correct = useMutation({
    mutationFn: () =>
      api.correctMemory({
        id: memory.id,
        text: next,
        ...(reason.trim() && { reason: reason.trim() }),
        ...(effectiveAt && { effectiveAt }),
      }),
    onSuccess: (res) => {
      refresh()
      navigate(`/memories/${res.memory.id}`)
    },
  })

  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      title="更正记忆"
      description="新值作为一条新记录（来源：显式）取代这条记录；旧值保留在更正链里。只有 current 记录能更正。"
    >
      <div className="space-y-3">
        <Field label="新内容" hint={`${bytes} / ${MAX_TEXT_BYTES} 字节`}>
          <Textarea value={text} onChange={(e) => setText(e.target.value)} />
        </Field>
        <Field label="理由" hint="记为这次更正的证据">
          <Input value={reason} onChange={(e) => setReason(e.target.value)} />
        </Field>
        <Field label="生效日期" hint="新值从哪天起成立；留空为现在">
          <Input type="date" value={effectiveAt} onChange={(e) => setEffectiveAt(e.target.value)} />
        </Field>
        <div>
          <p className="mb-1 text-sm font-medium text-zinc-700">变化</p>
          <div className="rounded-md border border-zinc-200 p-2">
            <DiffView before={memory.text} after={next} />
          </div>
        </div>
        {correct.error && <ErrorNotice error={correct.error} />}
        <div className="flex justify-end gap-2">
          <Button variant="secondary" onClick={() => onOpenChange(false)}>取消</Button>
          <Button
            disabled={!next || next === memory.text || bytes > MAX_TEXT_BYTES || correct.isPending}
            onClick={() => correct.mutate()}
          >
            {correct.isPending ? '提交中…' : '提交更正'}
          </Button>
        </div>
      </div>
    </Modal>
  )
}

function RetractDialog({ memory, open, onOpenChange }: DialogProps) {
  const [reason, setReason] = useState('')
  const refresh = useRefresh()
  const retract = useMutation({
    mutationFn: () => api.correctMemory({ id: memory.id, ...(reason.trim() && { reason: reason.trim() }) }),
    onSuccess: () => {
      refresh()
      onOpenChange(false)
    },
  })
  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      title="撤回记忆"
      description="撤回后这条记忆不再成立：状态变为已取代，没有新值。撤回不能恢复，只能重新新增。"
    >
      <div className="space-y-3">
        <Field label="理由" hint="记为这次撤回的证据">
          <Input value={reason} onChange={(e) => setReason(e.target.value)} />
        </Field>
        {retract.error && <ErrorNotice error={retract.error} />}
        <div className="flex justify-end gap-2">
          <Button variant="secondary" onClick={() => onOpenChange(false)}>取消</Button>
          <Button variant="danger" disabled={retract.isPending} onClick={() => retract.mutate()}>
            {retract.isPending ? '提交中…' : '确认撤回'}
          </Button>
        </div>
      </div>
    </Modal>
  )
}

function DeleteDialog({ memory, open, onOpenChange, onDeleted }: DialogProps & {
  onDeleted: (result: DeleteMemoryRes) => void
}) {
  const client = useQueryClient()
  const refresh = useRefresh()
  const remove = useMutation({
    mutationFn: () => api.deleteMemory(memory.id),
    onSuccess: (res) => {
      client.removeQueries({ queryKey: ['memory', memory.id] })
      refresh()
      onDeleted(res)
    },
  })
  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      title="删除记忆"
      description="物理删除这条记录和只属于它的证据，不能撤销。"
    >
      <div className="space-y-3 text-sm">
        <ul className="list-disc space-y-1 pl-5 text-zinc-700">
          <li>由它生成的画像和技能会一起删除。</li>
          <li>它是某条合并记录的输入时，合并记录也会删除，合并的其他输入恢复为 current。</li>
          <li>它在更正链中间时，前后两条会直接相连。</li>
        </ul>
        <p className="rounded-md bg-zinc-50 p-2 whitespace-pre-wrap">{memory.text}</p>
        {remove.error && <ErrorNotice error={remove.error} />}
        <div className="flex justify-end gap-2">
          <Button variant="secondary" onClick={() => onOpenChange(false)}>取消</Button>
          <Button variant="danger" disabled={remove.isPending} onClick={() => remove.mutate()}>
            {remove.isPending ? '删除中…' : '确认删除'}
          </Button>
        </div>
      </div>
    </Modal>
  )
}

function DeleteResult({ result }: { result: DeleteMemoryRes }) {
  return (
    <Card title="已删除" actions={<Link to="/memories" className="text-sm text-sky-700 hover:underline">返回列表</Link>}>
      <Fields
        rows={[
          ['删除的记录', <span className="font-mono">{result.deleted}</span>],
          [
            '连带删除',
            result.derivedDeleted.length
              ? result.derivedDeleted.map((id) => <span key={id} className="mr-3 font-mono">{id}</span>)
              : '—',
          ],
          ['恢复为 current', ids(result.restoredInputs)],
          [
            '更正链重新相连',
            result.relinked ? (
              <span>
                <MemoryLink id={result.relinked.prev} /> → <MemoryLink id={result.relinked.next} />
              </span>
            ) : (
              '—'
            ),
          ],
          ['移除的关联', result.edgesRemoved],
          ['删除 / 更新的证据', `${result.evidenceDeleted} / ${result.evidenceUpdated}`],
        ]}
      />
    </Card>
  )
}
