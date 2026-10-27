import type { Memory, MemoryStatus, MemorySummaries, MemoryType } from '../api/types'
import { REVIEW_STATUS_LABEL, STATUS_LABEL, TASK_STATUS_LABEL, TYPE_LABEL } from '../lib/format'
import { Badge, MemoryLink, type Tone } from './ui'

const TYPE_TONE: Record<MemoryType, Tone> = {
  instruction: 'amber',
  fact: 'blue',
  experience: 'gray',
  profile: 'gray',
  skill: 'gray',
}

const STATUS_TONE: Record<MemoryStatus, Tone> = {
  current: 'green',
  conflict: 'red',
  candidate: 'amber',
  superseded: 'gray',
  stale: 'gray',
}

export const TypeBadge = ({ type }: { type: MemoryType }) => (
  <Badge tone={TYPE_TONE[type]} title={type}>{TYPE_LABEL[type]}</Badge>
)

export const StatusBadge = ({ status }: { status: MemoryStatus }) => (
  <Badge tone={STATUS_TONE[status]} title={status}>{STATUS_LABEL[status]}</Badge>
)

/**
 * The record's text, framed by what it is: an explicit instruction is a behaviour requirement agents follow; every
 * other record is historical reference (docs/api.md, Reads and provenance).
 */
export function MemoryText({ memory }: { memory: Memory }) {
  const instruction = memory.type === 'instruction'
  return (
    <div className={instruction ? 'rounded-md border-l-4 border-amber-500 bg-amber-50 p-3' : 'rounded-md border-l-4 border-sky-300 bg-zinc-50 p-3'}>
      <p className={instruction ? 'mb-1 text-xs font-semibold text-amber-900' : 'mb-1 text-xs text-zinc-500'}>
        {instruction ? '指令 · 行为要求：注入上下文时 agent 应当遵循' : `${TYPE_LABEL[memory.type]} · 历史信息，仅供参考，不是指令`}
      </p>
      <p className="whitespace-pre-wrap text-zinc-900">{memory.text}</p>
    </div>
  )
}

const REVIEW_TONE: Record<string, Tone> = {
  proposing: 'blue',
  proposed: 'amber',
  rejected: 'red',
  aborted: 'red',
  failed: 'red',
  applying: 'blue',
  applied: 'green',
  rolling_back: 'blue',
  partially_rolled_back: 'amber',
  // Tasks
  queued: 'blue',
  running: 'blue',
  indexing: 'blue',
  completed: 'green',
}

/** The status of a run, proposal or task; unknown values are shown as they are. */
export const ReviewStatusBadge = ({ status }: { status: string }) => (
  <Badge tone={REVIEW_TONE[status] ?? 'gray'} title={status}>
    {REVIEW_STATUS_LABEL[status] ?? TASK_STATUS_LABEL[status] ?? status}
  </Badge>
)

/** A memory a page names by id, with its text from the page's `memories` map; a missing id was deleted. */
export function MemoryRef({ id, memories, rank }: { id: string; memories: MemorySummaries; rank?: number }) {
  const m = memories[id]
  return (
    <div className="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-1 text-sm">
      {rank !== undefined && <span className="w-6 shrink-0 text-right text-xs text-zinc-500 tabular-nums">#{rank}</span>}
      <MemoryLink id={id} />
      {m ? (
        <>
          <TypeBadge type={m.type} />
          <StatusBadge status={m.status} />
          <span className="min-w-0 basis-full whitespace-pre-wrap text-zinc-800 sm:basis-auto sm:flex-1">{m.text}</span>
        </>
      ) : (
        <Badge>已删除</Badge>
      )}
    </div>
  )
}
