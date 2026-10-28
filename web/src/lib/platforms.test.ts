import { expect, it } from 'vitest'
import type { PlatformHost } from '../api/types'
import { connectCommand, homePath, platformState, trustSteps, verification } from './platforms'

const codex: PlatformHost = {
  id: 'codex',
  label: 'Codex',
  platform: 'codex',
  capture: true,
  recall: true,
  files: ['/Users/me/.codex/config.toml', '/Users/me/.codex/hooks.json'],
  detected: '/Users/me/.codex',
  connected: true,
  options: { capture: true, recall: false },
  upToDate: true,
  canRollback: true,
  error: null,
}
const dsh: PlatformHost = { ...codex, id: 'deepseek-harness', label: 'DeepSeek Harness', capture: false, recall: false }

it('places a platform by its configuration, then by whether it has called', () => {
  const calls = { platform: 'codex', lastAt: '2026-10-28T10:00:00Z', other: { count: 1, lastAt: '2026-10-28T10:00:00Z' } }
  expect(platformState({ ...codex, error: 'Invalid JSON' }, calls)).toBe('error')
  expect(platformState({ ...codex, connected: false, options: null }, undefined)).toBe('available')
  expect(platformState({ ...codex, connected: false, detected: null }, undefined)).toBe('missing')
  expect(platformState({ ...codex, upToDate: false }, calls)).toBe('outdated')
  expect(platformState(codex, undefined)).toBe('silent')
  expect(platformState(codex, calls)).toBe('connected')
})

it('says what to do in the platform and how to run the same write in a terminal', () => {
  const titles = (options = { capture: true, recall: true }) => trustSteps(codex, options).map((s) => s.title)
  expect(titles()[1]).toBe('信任 hook：“shared-memory capture” 和 “shared-memory recall”')
  expect(titles({ capture: false, recall: false })).toHaveLength(2) // no hooks to trust
  expect(connectCommand(codex, { capture: false, recall: true })).toBe(
    'bash scripts/install.sh connect --host codex --apply --no-capture --recall',
  )
  expect(connectCommand(dsh, { capture: false, recall: false })).toBe(
    'bash scripts/install.sh connect --host deepseek-harness --apply',
  )
  expect(homePath('/Users/me/.codex')).toBe('~/.codex')
  expect(homePath('/home/me')).toBe('~')
  expect(homePath('/opt/homebrew/bin/codex')).toBe('/opt/homebrew/bin/codex')
})

it('counts only calls from the second of the write on, and needs MCP only from a platform without hooks', () => {
  const since = Date.parse('2026-10-28T10:00:00.700Z')
  const activity = {
    platform: 'codex',
    lastAt: '2026-10-28T10:00:05Z',
    capture: { count: 3, lastAt: '2026-10-28T10:00:00Z' },
    hookRecall: { count: 1, lastAt: '2026-10-28T09:59:59Z' },
    mcp: { count: 1, lastAt: '2026-10-28T10:00:05Z' },
  }
  const checks = verification(codex, { capture: true, recall: true }, activity, since)
  expect(checks.map((c) => [c.key, c.seenAt, c.optional])).toEqual([
    ['capture', '2026-10-28T10:00:00Z', false],
    ['hookRecall', null, false],
    ['mcp', '2026-10-28T10:00:05Z', true],
  ])
  expect(verification(codex, { capture: false, recall: false }, undefined, since)).toEqual([
    expect.objectContaining({ key: 'mcp', optional: false, seenAt: null }),
  ])
  expect(verification(dsh, { capture: true, recall: true }, activity, since).map((c) => c.key)).toEqual(['mcp'])
})
