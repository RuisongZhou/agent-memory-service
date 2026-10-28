import { describe, expect, it } from 'vitest'
import {
  entityFiltersFromParams,
  entityFiltersToParams,
  entityRequest,
  filtersFromParams,
  filtersToParams,
  listRequest,
  NO_ENTITY_FILTERS,
  NO_FILTERS,
  NO_RECALL_FILTERS,
  NO_TASK_FILTERS,
  recallFiltersFromParams,
  recallFiltersToParams,
  recallRequest,
  responsesRequest,
  taskFiltersFromParams,
  taskFiltersToParams,
  taskRequest,
} from './filters'
import { projectName, projectOption } from './format'

describe('list filters', () => {
  it('default to the active statuses (NO_FILTERS), drop values the daemon would reject, and leave both out of URL and body', () => {
    for (const query of ['', 'type=note&scope=team&status=archived&recall=yearly']) {
      const f = filtersFromParams(new URLSearchParams(query))
      expect(f).toEqual(NO_FILTERS)
      expect(filtersToParams(f).toString()).toBe('')
      expect(listRequest(f)).toEqual({})
    }

    // Entity filters are free text, so only blanks fall away.
    for (const query of ['', 'q=%20%20']) {
      const f = entityFiltersFromParams(new URLSearchParams(query))
      expect(entityFiltersToParams(f).toString()).toBe('')
      expect(entityRequest(f)).toEqual({})
    }
    expect(entityFiltersFromParams(new URLSearchParams(''))).toEqual(NO_ENTITY_FILTERS)

    // The recall log hides the UI's own calls by default; the task list shows every status and the model-call tab
    // only failures, which the daemon would not do unasked.
    for (const query of ['', 'kind=feedback&web=yes']) {
      const f = recallFiltersFromParams(new URLSearchParams(query))
      expect(f).toEqual(NO_RECALL_FILTERS)
      expect(recallFiltersToParams(f).toString()).toBe('')
      expect(recallRequest(f)).toEqual({ excludePlatforms: ['web'] })
    }
    for (const query of ['', 'tab=other&status=archived&kind=dialogue']) {
      const f = taskFiltersFromParams(new URLSearchParams(query))
      expect(f).toEqual(NO_TASK_FILTERS)
      expect(taskFiltersToParams(f).toString()).toBe('')
      expect(taskRequest(f)).toEqual({})
      expect(responsesRequest(f)).toEqual({ failedOnly: true })
    }
  })

  it('round-trip through the hash query, with a folder path as the project, and map to a request body', () => {
    const query =
      'project=%2FUsers%2Fme%2Fmy+repo&scope=project&platform=codex&type=instruction&origin=user&status=current%2Cstale&q=%E7%AB%AF%E5%8F%A3&recall=30'
    const f = filtersFromParams(new URLSearchParams(query))
    expect(f.statuses).toEqual(['current', 'stale'])
    expect(filtersToParams(f).toString()).toBe(query)
    expect(listRequest(f, 'c1')).toEqual({
      project: '/Users/me/my repo',
      scopeKind: 'project',
      sourcePlatform: 'codex',
      types: ['instruction'],
      origins: ['user'],
      statuses: ['current', 'stale'],
      text: '端口',
      notRecalledDays: 30,
      cursor: 'c1',
    })

    // The recall filter is one choice, so the daemon never gets both `neverRecalled` and `notRecalledDays`.
    const never = listRequest({ ...f, recall: 'never' })
    expect(never).toMatchObject({ project: '/Users/me/my repo', neverRecalled: true })
    expect(never).not.toHaveProperty('notRecalledDays')
    expect(listRequest({ ...NO_FILTERS, recall: '90' })).toEqual({ notRecalledDays: 90 })

    // A project id is its folder's path: tables show the last segment (the path is their tooltip) and other ids as they are.
    expect(['/Users/me/my repo', '/Users/me/repo/', '/', 'demo', ''].map(projectName)).toEqual([
      'my repo',
      'repo',
      '/',
      'demo',
      '',
    ])
    expect([f.project, 'demo'].map(projectOption)).toEqual(['my repo — /Users/me/my repo', 'demo'])

    const entityQuery = 'project=demo&type=PERSON&q=%E7%AB%AF%E5%8F%A3'
    const entity = entityFiltersFromParams(new URLSearchParams(entityQuery))
    expect(entityFiltersToParams(entity).toString()).toBe(entityQuery)
    expect(entityRequest(entity, 'c5')).toEqual({ project: 'demo', entityType: 'PERSON', text: '端口', cursor: 'c5' })

    // Showing the UI's own calls, or asking for the web platform by name, stops excluding them.
    const recallQuery = 'kind=search&platform=codex&project=demo&session=s1&web=1'
    const recall = recallFiltersFromParams(new URLSearchParams(recallQuery))
    expect(recallFiltersToParams(recall).toString()).toBe(recallQuery)
    expect(recallRequest(recall, 'c2')).toEqual({
      kind: 'search',
      platform: 'codex',
      project: 'demo',
      session: 's1',
      cursor: 'c2',
    })
    expect(recallRequest({ ...recall, showWeb: false })).toMatchObject({ excludePlatforms: ['web'] })
    expect(recallRequest({ ...recall, platform: 'web', showWeb: false })).not.toHaveProperty('excludePlatforms')

    const taskQuery = 'tab=responses&status=queued%2Cfailed&kind=mem0&platform=codex&project=demo&stage=Mem0-add&all=1'
    const task = taskFiltersFromParams(new URLSearchParams(taskQuery))
    expect(task.statuses).toEqual(['queued', 'failed'])
    expect(taskFiltersToParams(task).toString()).toBe(taskQuery)
    expect(taskRequest(task, 'c3')).toEqual({
      statuses: ['queued', 'failed'],
      kind: 'mem0',
      platform: 'codex',
      project: 'demo',
      cursor: 'c3',
    })
    expect(responsesRequest(task, 'c4')).toEqual({ stage: 'Mem0-add', failedOnly: false, cursor: 'c4' })
  })
})
