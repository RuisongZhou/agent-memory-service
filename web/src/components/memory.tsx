import type { Memory, MemoryStatus, MemoryType } from '../api/types'
import { STATUS_LABEL, TYPE_LABEL } from '../lib/format'
import { Badge, type Tone } from './ui'

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
