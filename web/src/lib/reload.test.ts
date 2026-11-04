import { describe, expect, it } from 'vitest'
import { chunkFailed, mayReload, RELOAD_PAUSE_MS } from './reload'

const storage = () => {
  const items = new Map<string, string>()
  return { getItem: (k: string) => items.get(k) ?? null, setItem: (k: string, v: string) => void items.set(k, v) }
}

describe('chunkFailed', () => {
  it('recognises a page module the browser could not fetch', () => {
    for (const message of [
      'Failed to fetch dynamically imported module: http://127.0.0.1:8765/ui/assets/Recalls-Dl2HHFLq.js',
      'error loading dynamically imported module: http://127.0.0.1:8765/ui/assets/Tree-BvtYnvIK.js',
      'Importing a module script failed.',
      'Unable to preload CSS for /ui/assets/index-JcGOSjyo.css',
    ]) {
      expect(chunkFailed(new TypeError(message)), message).toBe(true)
    }
  })

  it('leaves every other error to the page', () => {
    expect(chunkFailed(new TypeError("Cannot read properties of undefined (reading 'length')"))).toBe(false)
    expect(chunkFailed(new Error('Failed to fetch'))).toBe(false)
    expect(chunkFailed('Failed to fetch dynamically imported module')).toBe(false)
  })
})

describe('mayReload', () => {
  it('reloads once, then not again until the pause has passed', () => {
    const s = storage()
    expect(mayReload(s, 1_000_000)).toBe(true)
    expect(mayReload(s, 1_000_000 + RELOAD_PAUSE_MS - 1)).toBe(false)
    expect(mayReload(s, 1_000_000 + RELOAD_PAUSE_MS)).toBe(true)
  })

  it('does not reload without storage to record it in', () => {
    expect(mayReload(null, 1)).toBe(false)
    const refusing = { getItem: () => null, setItem: () => { throw new Error('QuotaExceededError') } }
    expect(mayReload(refusing, 1)).toBe(false)
  })

  it('ignores a record from the future, as after a clock change', () => {
    const s = storage()
    expect(mayReload(s, 5_000_000)).toBe(true)
    expect(mayReload(s, 1_000_000)).toBe(true)
  })
})
