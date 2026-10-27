import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { api } from '../api/client'
import type { LineageNode } from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { StatusBadge, TypeBadge } from '../components/memory'
import { Badge, Card, MemoryLink, Muted, Note, RefreshButton, Select } from '../components/ui'
import { formatNumber, formatTime, labelled, ORIGIN_LABEL } from '../lib/format'
import { DEFAULT_DEPTH, edgeLines, MAX_DEPTH, timeline } from '../lib/lineage'

const DEPTHS = Array.from({ length: MAX_DEPTH }, (_, i) => i + 1)

/**
 * The record's links in every direction, as a timeline: supersession, merging and derivation, profile and skill
 * sources, and `related`. The correction chain above it stays the focused view of one record's own values.
 */
export function Lineage({ id }: { id: string }) {
  const [depth, setDepth] = useState(DEFAULT_DEPTH)
  const lineage = useQuery({
    queryKey: ['lineage', id, depth],
    queryFn: () => api.memoryLineage(id, depth),
    placeholderData: keepPreviousData,
  })
  const data = lineage.data
  const byId = Object.fromEntries(data?.nodes.map((n) => [n.id, n]) ?? []) as Record<string, LineageNode>

  return (
    <Card
      title="谱系"
      actions={
        <div className="flex items-center gap-2">
          <label className="inline-flex items-center gap-1.5 text-sm">
            <span className="text-zinc-600">深度</span>
            <Select value={depth} onChange={(e) => setDepth(Number(e.target.value))}>
              {DEPTHS.map((d) => (
                <option key={d} value={d}>{d}</option>
              ))}
            </Select>
          </label>
          <RefreshButton busy={lineage.isFetching} onClick={() => lineage.refetch()} />
        </div>
      }
    >
      <p className="mb-3 text-sm text-zinc-500">
        沿取代、合并和派生、画像和技能的来源、关联四种关系，向两个方向最多走 {depth} 步，按记录时间从旧到新。
      </p>
      {lineage.error && <ErrorNotice error={lineage.error} />}
      {!data ? (
        !lineage.error && <Muted>加载中…</Muted>
      ) : (
        <div className="space-y-3">
          {data.truncated && (
            <Note>
              <Badge tone="amber">已截断</Badge> 节点数达到上限，只显示了 {formatNumber(data.nodes.length)} 个；可以减小深度，或打开其中一条记忆再看它的谱系。
            </Note>
          )}
          {data.nodes.length === 1 ? (
            <Muted>没有关联记录。</Muted>
          ) : (
            <ol className="space-y-3 border-l-2 border-zinc-200 pl-4">
              {timeline(data.nodes).map((n) => (
                <li key={n.id} className={n.id === data.root ? 'rounded-md bg-sky-50 p-2' : 'p-2'}>
                  <div className="mb-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-zinc-500">
                    {n.id === data.root && <Badge tone="blue">本条</Badge>}
                    <MemoryLink id={n.id} />
                    <TypeBadge type={n.type} />
                    <StatusBadge status={n.status} />
                    <span>{labelled(ORIGIN_LABEL, n.origin)} · {n.platform}</span>
                    <span>记录 {formatTime(n.recordedAt)}</span>
                  </div>
                  <p className="text-sm whitespace-pre-wrap">{n.text}</p>
                  <ul className="mt-1.5 space-y-0.5 text-xs text-zinc-600">
                    {edgeLines(n.id, data.edges).map((l) => (
                      <li key={l.before + l.other + l.after} className="flex flex-wrap items-baseline gap-x-1">
                        <span>
                          {l.before}
                          <MemoryLink id={l.other} />
                          {l.after}
                        </span>
                        <span className="max-w-full truncate text-zinc-400" title={byId[l.other].text}>
                          {byId[l.other].text}
                        </span>
                      </li>
                    ))}
                  </ul>
                </li>
              ))}
            </ol>
          )}
        </div>
      )}
    </Card>
  )
}
