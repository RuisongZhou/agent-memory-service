// A page whose module cannot be fetched: the build this tab was loaded from was replaced while it stayed open (`make ui`,
// `install.sh service --apply` delete the old hashed files), or the daemon is not answering. Only a reload recovers it:
// `React.lazy` and the browser keep the failed import, and the new `index.html` names the files now installed.

/** Whether `error` is the browser failing to fetch a page's module (Chromium, Firefox, Safari) or Vite failing to
 * preload its stylesheet. */
export function chunkFailed(error: unknown): boolean {
  return (
    error instanceof Error &&
    /dynamically imported module|Importing a module script failed|Unable to preload CSS/i.test(error.message)
  )
}

const RELOADED_AT = 'agent-memory-ui:reloaded-at'
/** A reload this soon after the last one would fail the same way: the page shows the error instead. */
export const RELOAD_PAUSE_MS = 30_000

/**
 * Whether the tab may reload now to recover, recorded in `storage` (its sessionStorage, which outlives the reload) so
 * a module still missing afterwards cannot reload it in a loop. Without storage nothing is recorded: no reload.
 */
export function mayReload(storage: Pick<Storage, 'getItem' | 'setItem'> | null, now: number): boolean {
  if (!storage) return false
  try {
    const last = Number(storage.getItem(RELOADED_AT))
    if (last && now >= last && now - last < RELOAD_PAUSE_MS) return false
    storage.setItem(RELOADED_AT, String(now))
    return true
  } catch {
    return false
  }
}
