import { afterEach, describe, expect, it, vi } from 'vitest'
import { importDecisions } from '../lib/review'
import { api, ApiError } from './client'
import type { ImportProposal } from './types'

function respond(...answers: Response[]) {
  const fetch = vi.fn<typeof globalThis.fetch>()
  for (const answer of answers) fetch.mockResolvedValueOnce(answer)
  vi.stubGlobal('fetch', fetch)
  return fetch
}

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } })

afterEach(() => vi.unstubAllGlobals())

describe('api client', () => {
  it('posts JSON as the web platform, never touches on search, only proposes consolidation, decides every proposed import proposal, and reads health with a plain GET', async () => {
    const fetch = respond(
      json({ results: [] }),
      json({ ok: true, version: '0.1.0' }),
      json({ run: {}, proposals: [] }),
      json({ run: {}, proposals: [] }),
    )

    await api.search({ query: '端口', scope: { project: 'p', platform: '*' }, topK: 5 })
    const [path, init] = fetch.mock.calls[0]
    expect(path).toBe('/v1/search_memories')
    expect(init?.method).toBe('POST')
    expect(init?.headers).toEqual({ 'Content-Type': 'application/json', 'X-Memory-Platform': 'web' })
    expect(JSON.parse(init?.body as string)).toEqual({
      query: '端口',
      scope: { project: 'p', platform: '*' },
      topK: 5,
      touch: false,
    })

    expect(await api.health()).toEqual({ ok: true, version: '0.1.0' })
    expect(fetch.mock.calls[1]).toEqual(['/health'])

    await api.consolidate('p')
    expect(fetch.mock.calls[2][0]).toBe('/admin/consolidate')
    expect(JSON.parse(fetch.mock.calls[2][1]?.body as string)).toEqual({ project: 'p', dryRun: true })

    // Accept is the default for a proposal the reviewer left alone; settled proposals get no decision.
    const proposal = (proposalId: string, status: ImportProposal['status']) => ({ proposalId, status }) as ImportProposal
    const proposals = [proposal('a', 'proposed'), proposal('b', 'proposed'), proposal('c', 'rejected')]
    await api.importApply({ runId: 'ir1', decisions: importDecisions(proposals, new Set(['b', 'c'])) })
    expect(fetch.mock.calls[3][0]).toBe('/admin/import_apply')
    expect(JSON.parse(fetch.mock.calls[3][1]?.body as string)).toEqual({
      runId: 'ir1',
      decisions: { a: 'accept', b: 'reject' },
    })
  })

  it('turns error answers into ApiError, with the current id of a 409 and the bare status without a body', async () => {
    respond(
      json({ code: 'NOT_CURRENT', message: 'memory is not current; current id = m2', currentId: 'm2' }, 409),
      new Response('Bad Gateway', { status: 502 }),
    )

    const notCurrent = await api.correctMemory({ id: 'm1', text: 'x' }).catch((e: unknown) => e)
    expect(notCurrent).toBeInstanceOf(ApiError)
    expect(notCurrent).toMatchObject({ status: 409, code: 'NOT_CURRENT', currentId: 'm2' })
    expect((notCurrent as ApiError).message).toBe('memory is not current; current id = m2')

    const gateway = await api.stats().catch((e: unknown) => e)
    expect(gateway).toMatchObject({ status: 502, code: 'HTTP_502', message: 'Bad Gateway', currentId: undefined })
  })
})
