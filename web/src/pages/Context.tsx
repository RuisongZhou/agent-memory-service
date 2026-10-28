import { useMutation, useQuery } from '@tanstack/react-query'
import { useState, type FormEvent, type ReactNode } from 'react'
import { api, PLATFORM } from '../api/client'
import type { GetContextReq, Memory } from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { Badge, Button, Card, Check, Field, Fields, Input, MemoryLink, Muted, Pre } from '../components/ui'
import { ALL_PLATFORMS_LABEL, projectName } from '../lib/format'

function Section({ title, empty, children, instruction }: {
  title: string
  empty: boolean
  children: ReactNode
  instruction?: boolean
}) {
  return (
    <div className={instruction ? 'rounded-md border-l-4 border-amber-500 bg-amber-50 p-3' : 'rounded-md border-l-4 border-zinc-200 p-3'}>
      <h3 className="mb-1 text-sm font-semibold">{title}</h3>
      {empty ? <Muted>无</Muted> : children}
    </div>
  )
}

const MemoryItems = ({ memories }: { memories: (Memory & { pendingConfirmation?: boolean })[] }) => (
  <ul className="space-y-1 text-sm">
    {memories.map((m) => (
      <li key={m.id} className="flex flex-wrap items-baseline gap-2">
        <span className="whitespace-pre-wrap">{m.text}</span>
        {m.pendingConfirmation && <Badge tone="red">矛盾，待确认</Badge>}
        <MemoryLink id={m.id} />
      </li>
    ))}
  </ul>
)

export function ContextPage() {
  const facets = useQuery({ queryKey: ['facets'], queryFn: api.facets })
  const [project, setProject] = useState('')
  const [session, setSession] = useState(PLATFORM)
  const [tokenBudget, setTokenBudget] = useState('')
  const [allPlatforms, setAllPlatforms] = useState(true)
  // Each preview writes a retrieval log row, so it runs only on submit.
  const preview = useMutation({ mutationFn: (req: GetContextReq) => api.getContext(req) })

  const submit = (e: FormEvent) => {
    e.preventDefault()
    preview.mutate({
      scope: { project: project.trim(), session: session.trim(), ...(allPlatforms && { platform: '*' }) },
      ...(tokenBudget && { tokenBudget: Number(tokenBudget) }),
    })
  }

  const res = preview.data
  return (
    <div className="space-y-4">
      <Card title="上下文预览">
        <form onSubmit={submit} className="space-y-3">
          <div className="grid gap-3 sm:grid-cols-3">
            <Field label="项目">
              <Input list="context-projects" value={project} onChange={(e) => setProject(e.target.value)} />
              <datalist id="context-projects">
                {facets.data?.projects.map((p) => <option key={p.name} value={p.name} label={projectName(p.name)} />)}
              </datalist>
            </Field>
            <Field label="会话">
              <Input value={session} onChange={(e) => setSession(e.target.value)} />
            </Field>
            <Field label="token 预算" hint="留空用 daemon 默认值">
              <Input type="number" min={1} step={1} value={tokenBudget} onChange={(e) => setTokenBudget(e.target.value)} />
            </Field>
          </div>
          <Check label={ALL_PLATFORMS_LABEL} checked={allPlatforms} onChange={(e) => setAllPlatforms(e.target.checked)} />
          <div className="flex flex-wrap items-center gap-3">
            <Button type="submit" disabled={!project.trim() || preview.isPending}>
              {preview.isPending ? '生成中…' : '预览'}
            </Button>
            <Muted>不调用模型。每次预览会写一条召回记录（retrieval_log），平台记为 {PLATFORM}。</Muted>
          </div>
        </form>
      </Card>

      {preview.error && <ErrorNotice error={preview.error} />}
      {res && (
        <>
          <Card title="注入文本">
            <div className="mb-3">
              <Fields
                rows={[
                  ['用掉的 token', res.tokensUsed],
                  ['召回记录', <span className="font-mono">{res.retrievalId}</span>],
                ]}
              />
            </div>
            {res.text ? <Pre>{res.text}</Pre> : <Muted>没有可注入的内容。</Muted>}
          </Card>
          <Card title="各部分">
            <div className="space-y-3">
              <Section title="显式指令（行为要求）" empty={!res.instructions.length} instruction>
                <MemoryItems memories={res.instructions} />
              </Section>
              <Section title="用户画像" empty={!res.profile}>
                {res.profile && <MemoryItems memories={[res.profile]} />}
              </Section>
              <Section title="当前项目要点" empty={!res.projectNotes && !res.projectFacts.length}>
                <MemoryItems memories={res.projectNotes ? [res.projectNotes] : res.projectFacts} />
              </Section>
              <Section title="近期变化" empty={!res.facts.length}>
                <MemoryItems memories={res.facts} />
              </Section>
              <Section title="可用技能" empty={!res.skills.length}>
                <ul className="space-y-1 text-sm">
                  {res.skills.map((s) => (
                    <li key={s.id}>
                      {s.name}
                      {s.preconditions && `：${s.preconditions}`} <MemoryLink id={s.id} />
                    </li>
                  ))}
                </ul>
              </Section>
            </div>
          </Card>
        </>
      )}
    </div>
  )
}
