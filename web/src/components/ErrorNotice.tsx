import { ApiError } from '../api/client'
import { MemoryLink } from './ui'

/** A failed request. A 409 NOT_CURRENT links to the current value of the record's chain. */
export function ErrorNotice({ error }: { error: Error }) {
  const api = error instanceof ApiError ? error : undefined
  return (
    <div role="alert" className="rounded-md border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-800">
      {api && <span className="mr-1 font-mono text-xs">{api.code}</span>}
      {error.message}
      {api?.code === 'NOT_CURRENT' && api.currentId && (
        <div className="mt-1">
          只有 current 记录能更正。当前值：<MemoryLink id={api.currentId} />
        </div>
      )}
    </div>
  )
}
