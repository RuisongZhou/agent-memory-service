import { useQueryClient } from '@tanstack/react-query'

/** Every query a write can make stale. */
const KEYS = ['memories', 'memory', 'facets', 'stats', 'imports', 'consolidation', 'uncertainties', 'retrievals', 'requests', 'lineage', 'entities']

/** After a write every list, count and record may have changed. */
export function useRefresh() {
  const client = useQueryClient()
  return () => {
    for (const key of KEYS) client.invalidateQueries({ queryKey: [key] })
  }
}
