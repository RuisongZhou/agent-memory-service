import { diffWords } from 'diff'

export interface Segment {
  kind: 'same' | 'added' | 'removed'
  text: string
}

// Chinese has no spaces between words; a word segmenter keeps a changed term from being split into characters.
const words = new Intl.Segmenter('zh', { granularity: 'word' })

/** Word-level changes from `before` to `after`, for a correction preview and the history timeline. */
export function wordDiff(before: string, after: string): Segment[] {
  return diffWords(before, after, { intlSegmenter: words }).map((c) => ({
    kind: c.added ? 'added' : c.removed ? 'removed' : 'same',
    text: c.value,
  }))
}
