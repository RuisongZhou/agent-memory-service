import { expect, it } from 'vitest'
import type { LineageEdge, LineageNode } from '../api/types'
import { wordDiff } from './diff'
import { edgeLines, timeline } from './lineage'

it('marks only the changed words of a correction, in Chinese and English alike, and words a lineage from each record', () => {
  expect(wordDiff('服务端口是 8765，部署在测试机上。', '服务端口是 9000，部署在生产机上。')).toEqual([
    { kind: 'same', text: '服务端口是 ' },
    { kind: 'removed', text: '8765' },
    { kind: 'added', text: '9000' },
    { kind: 'same', text: '，部署在' },
    { kind: 'removed', text: '测试' },
    { kind: 'added', text: '生产' },
    { kind: 'same', text: '机上。' },
  ])
  expect(wordDiff('The port is 8765 now.', 'The port is 8765 now.')).toEqual([
    { kind: 'same', text: 'The port is 8765 now.' },
  ])

  // The timeline runs oldest first, a tie going to the node nearer the root.
  const node = (id: string, recordedAt: string, depth: number) => ({ id, recordedAt, depth }) as LineageNode
  const sorted = timeline([
    node('m3', '2026-03-01T00:00:00Z', 1),
    node('m1', '2026-01-01T00:00:00Z', 2),
    node('m2', '2026-01-01T00:00:00Z', 0),
  ])
  expect(sorted.map((n) => n.id)).toEqual(['m2', 'm1', 'm3'])

  // Every edge reads from the record it is seen at: the one that names a link, the one it names, and a relation from
  // both ends. Lines come in the order supersession, derivation, use, relation.
  const edges: LineageEdge[] = [
    { from: 'mNew', to: 'mOld', kind: 'supersedes' },
    { from: 'mMerged', to: 'mNew', kind: 'derived', stage: 'consolidation' },
    { from: 'mProfile', to: 'mNew', kind: 'uses' },
    { from: 'mNew', to: 'mOther', kind: 'related', relation: 'conflicts' },
  ]
  const line = (other: string, before: string, after = '') => ({ other, before, after })
  expect(edgeLines('mNew', edges)).toEqual([
    line('mOld', '取代了 '),
    line('mMerged', '派生出 '),
    line('mProfile', '被 ', ' 引用'),
    line('mOther', '与 ', ' 冲突'),
  ])
  expect(edgeLines('mOld', edges)).toEqual([line('mNew', '被 ', ' 取代')])
  expect(edgeLines('mMerged', edges)).toEqual([line('mNew', '由 ', ' 合并或派生（consolidation）')])
  expect(edgeLines('mProfile', edges)).toEqual([line('mNew', '引用 ')])
  expect(edgeLines('mOther', edges)).toEqual([line('mNew', '与 ', ' 冲突')])
  expect(edgeLines('mAlone', edges)).toEqual([])
})
