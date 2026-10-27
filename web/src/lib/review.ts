import type { Decision, ImportProposal, ModelResponse } from '../api/types'

/** The `itemKind` of a past session turn's row in an import run (`ITEM_SESSION_TURN`). */
export const SESSION_TURN = 'session-turn'

/**
 * `import_apply` decisions: one for every proposal still `proposed`, `reject` for those the reviewer rejected and
 * `accept` otherwise, as the daemon does for a proposal it is not told about.
 */
export function importDecisions(proposals: ImportProposal[], rejected: ReadonlySet<string>): Record<string, Decision> {
  return Object.fromEntries(
    proposals
      .filter((p) => p.status === 'proposed')
      .map((p) => [p.proposalId, rejected.has(p.proposalId) ? 'reject' : 'accept']),
  )
}

/**
 * Session turns the run would queue on apply. `import_apply` only marks them queued; the client that ran the import
 * submits them from its dialogue outbox, and the web UI has none, so it must not apply such a run.
 */
export const pendingSessionTurns = (proposals: ImportProposal[]) =>
  proposals.filter((p) => p.itemKind === SESSION_TURN && p.status === 'proposed').length

/** What `list_model_responses {failedOnly: true}` keeps: no answer, a non-2xx answer, a body that is not JSON, or
 * output of the wrong shape. Only a decoded 2xx reply records `validOutputShape`. */
export const responseFailed = (r: ModelResponse) => r.validOutputShape !== true
