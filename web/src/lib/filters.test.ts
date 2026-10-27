import { describe, expect, it } from 'vitest'
import { filtersFromParams, filtersToParams, listRequest, NO_FILTERS } from './filters'

describe('memory list filters', () => {
  it('default to the active statuses (NO_FILTERS), drop values the daemon would reject, and leave both out of URL and body', () => {
    for (const query of ['', 'type=note&scope=team&status=archived']) {
      const f = filtersFromParams(new URLSearchParams(query))
      expect(f).toEqual(NO_FILTERS)
      expect(filtersToParams(f).toString()).toBe('')
      expect(listRequest(f)).toEqual({})
    }
  })

  it('round-trip through the hash query and map to a list_memories body', () => {
    const query =
      'project=demo&scope=project&platform=codex&type=instruction&origin=user&status=current%2Cstale&q=%E7%AB%AF%E5%8F%A3'
    const f = filtersFromParams(new URLSearchParams(query))
    expect(f.statuses).toEqual(['current', 'stale'])
    expect(filtersToParams(f).toString()).toBe(query)
    expect(listRequest(f, 'c1')).toEqual({
      project: 'demo',
      scopeKind: 'project',
      sourcePlatform: 'codex',
      types: ['instruction'],
      origins: ['user'],
      statuses: ['current', 'stale'],
      text: '端口',
      cursor: 'c1',
    })
  })
})
