import { useQueryClient } from '@tanstack/react-query'

/** After a write every list, count and record may have changed. */
export function useRefresh() {
  const client = useQueryClient()
  return () => {
    for (const key of ['memories', 'memory', 'facets', 'stats']) client.invalidateQueries({ queryKey: [key] })
  }
}
