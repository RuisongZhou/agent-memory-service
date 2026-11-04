import { describe, expect, it } from 'vitest'
import { runActions } from './review'

describe('runActions', () => {
  it('offers what the daemon accepts, including resuming or undoing an interrupted apply', () => {
    const at = (status: string, error: string | null = null) => runActions({ status, error })
    expect(at('proposed')).toEqual({ apply: true, rollback: false })
    // No error: the apply may still be running, since the daemon clears the error when one starts.
    expect(at('applying')).toEqual({ apply: false, rollback: true })
    expect(at('applying', 'store error')).toEqual({ apply: true, rollback: true })
    for (const status of ['applied', 'rolling_back', 'partially_rolled_back']) {
      expect(at(status)).toEqual({ apply: false, rollback: true })
    }
    for (const status of ['proposing', 'aborted', 'failed', 'rolled_back']) {
      expect(at(status, 'boom')).toEqual({ apply: false, rollback: false })
    }
  })
})
