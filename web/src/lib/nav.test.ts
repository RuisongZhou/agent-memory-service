import { describe, expect, it } from 'vitest'
import { NAV, pageMeta } from './nav'

describe('pageMeta', () => {
  it('names every page in the sidebar', () => {
    for (const item of NAV.flatMap((g) => g.items)) expect(pageMeta(item.to)?.title).toBeTruthy()
  })

  it('links a detail page back to its list', () => {
    expect(pageMeta('/memories/mem_1')).toEqual({ title: '记忆详情', parent: { to: '/memories', label: '记忆列表' } })
    expect(pageMeta('/tasks/req_1')?.parent?.to).toBe('/tasks')
  })

  it('has nothing for an unknown path or a bare prefix', () => {
    expect(pageMeta('/nope')).toBeUndefined()
    expect(pageMeta('/memories/')).toBeUndefined()
  })
})
