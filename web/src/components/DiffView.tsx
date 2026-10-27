import { wordDiff } from '../lib/diff'

/** Word changes between two values: removed words struck through in red, added ones in green. */
export function DiffView({ before, after }: { before: string; after: string }) {
  return (
    <p className="text-sm leading-relaxed whitespace-pre-wrap">
      {wordDiff(before, after).map((s, i) =>
        s.kind === 'added' ? (
          <ins key={i} className="bg-emerald-100 text-emerald-900 no-underline">{s.text}</ins>
        ) : s.kind === 'removed' ? (
          <del key={i} className="bg-red-100 text-red-800">{s.text}</del>
        ) : (
          <span key={i}>{s.text}</span>
        ),
      )}
    </p>
  )
}
