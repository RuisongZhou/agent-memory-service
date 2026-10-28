import { expect, it } from 'vitest'
import { formatAgo } from './format'

it('says how long ago a time was in its largest whole unit, and a time ahead of the clock as just now', () => {
  const now = Date.parse('2026-10-28T12:00:00Z')
  const cases: [string, string][] = [
    ['2026-10-28T12:00:30Z', '刚刚'],
    ['2026-10-28T11:59:31Z', '刚刚'],
    ['2026-10-28T11:59:00Z', '1 分钟前'],
    ['2026-10-28T11:00:01Z', '59 分钟前'],
    ['2026-10-28T11:00:00Z', '1 小时前'],
    ['2026-10-27T12:00:01Z', '23 小时前'],
    ['2026-10-27T12:00:00Z', '1 天前'],
    ['2026-09-28T12:00:00Z', '30 天前'],
  ]
  for (const [at, want] of cases) expect(formatAgo(at, now), at).toBe(want)
})
