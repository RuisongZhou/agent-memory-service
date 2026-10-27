import type { LineageEdge, LineageEdgeKind, LineageNode } from '../api/types'
import { RELATION_LABEL } from './format'

/** The deepest walk `memory_lineage` allows, and the one it makes when no depth is sent. */
export const MAX_DEPTH = 6
export const DEFAULT_DEPTH = 3

/** The nodes oldest first by `recordedAt`, so the timeline reads in the order things were written. */
export const timeline = (nodes: LineageNode[]): LineageNode[] =>
  [...nodes].sort(
    (a, b) => Date.parse(a.recordedAt) - Date.parse(b.recordedAt) || a.depth - b.depth || a.id.localeCompare(b.id),
  )

/** One line under a node: `before`, a link to the record at the other end, `after`. */
export interface EdgeLine {
  other: string
  before: string
  after: string
}

const KIND_ORDER: LineageEdgeKind[] = ['supersedes', 'derived', 'uses', 'related']

/** Each edge points from the record that names the link; the wording depends on which end the line sits under. */
function wording(e: LineageEdge, outgoing: boolean): Pick<EdgeLine, 'before' | 'after'> {
  switch (e.kind) {
    case 'supersedes':
      return outgoing ? { before: '取代了 ', after: '' } : { before: '被 ', after: ' 取代' }
    case 'derived':
      return outgoing
        ? { before: '由 ', after: ` 合并或派生（${e.stage}）` }
        : { before: '派生出 ', after: '' }
    case 'uses':
      return outgoing ? { before: '引用 ', after: '' } : { before: '被 ', after: ' 引用' }
    case 'related':
      // A relation reads the same from both ends: one edge serves both records.
      return { before: '与 ', after: ` ${RELATION_LABEL[e.relation] ?? e.relation}` }
  }
}

/**
 * The edges at one node, worded relative to it: supersession first, then derivation, use and relations, each with
 * the links the node names before the ones that name it.
 */
export function edgeLines(nodeId: string, edges: LineageEdge[]): EdgeLine[] {
  return edges
    .filter((e) => e.from === nodeId || e.to === nodeId)
    .map((e) => {
      const outgoing = e.from === nodeId
      return { kind: e.kind, outgoing, other: outgoing ? e.to : e.from, ...wording(e, outgoing) }
    })
    .sort(
      (a, b) =>
        KIND_ORDER.indexOf(a.kind) - KIND_ORDER.indexOf(b.kind) ||
        Number(b.outgoing) - Number(a.outgoing) ||
        a.other.localeCompare(b.other),
    )
    .map(({ other, before, after }) => ({ other, before, after }))
}
