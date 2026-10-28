import { useMutation, useQuery } from '@tanstack/react-query'
import { useState, type FormEvent } from 'react'
import { Link } from 'react-router'
import { api } from '../api/client'
import type { MemoryType, SearchReq } from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { StatusBadge, TypeBadge } from '../components/memory'
import { Button, Card, Check, Field, Fields, Input, ModelCall, Muted, Select } from '../components/ui'
import { ALL_PLATFORMS_LABEL, labelled, projectOption, TYPE_LABEL } from '../lib/format'

// Instructions are not searchable: get_context selects them, and search rejects the type.
const SEARCH_TYPES: MemoryType[] = ['fact', 'experience', 'profile', 'skill']

const ms = (n: number) => `${n.toFixed(1)} ms`
const score = (n: number | undefined) => (n === undefined ? '—' : n.toFixed(4))

export function SearchPage() {
  const facets = useQuery({ queryKey: ['facets'], queryFn: api.facets })
  const health = useQuery({ queryKey: ['health'], queryFn: api.health })
  const [query, setQuery] = useState('')
  const [project, setProject] = useState('')
  const [allPlatforms, setAllPlatforms] = useState(true)
  const [topK, setTopK] = useState('')
  const [tokenBudget, setTokenBudget] = useState('')
  const [types, setTypes] = useState<MemoryType[]>(['fact'])
  const [rerank, setRerank] = useState(false)
  // A search embeds the query at the model backend, so it runs only on submit and is never refetched.
  const search = useMutation({ mutationFn: (req: SearchReq) => api.search(req) })

  const submit = (e: FormEvent) => {
    e.preventDefault()
    search.mutate({
      query: query.trim(),
      scope: { project: project || '*', ...(allPlatforms && { platform: '*' }) },
      ...(topK && { topK: Number(topK) }),
      ...(tokenBudget && { tokenBudget: Number(tokenBudget) }),
      types,
      // Sent either way: left out, the daemon's `mem0Rerank` setting decides, which may call the reranker.
      rerank,
    })
  }
  const toggle = (type: MemoryType, on: boolean) =>
    setTypes(SEARCH_TYPES.filter((t) => (t === type ? on : types.includes(t))))

  const res = search.data
  return (
    <div className="space-y-4">
      <Card title="检索调试">
        <form onSubmit={submit} className="space-y-3">
          <Field label="查询">
            <Input value={query} onChange={(e) => setQuery(e.target.value)} />
          </Field>
          <div className="grid gap-3 sm:grid-cols-3">
            <Field label="项目">
              <Select value={project} onChange={(e) => setProject(e.target.value)}>
                <option value="">全部项目（*）</option>
                {facets.data?.projects.map((p) => <option key={p.name} value={p.name}>{projectOption(p.name)}</option>)}
              </Select>
            </Field>
            <Field label="topK" hint="留空用 daemon 默认值">
              <Input type="number" min={1} step={1} value={topK} onChange={(e) => setTopK(e.target.value)} />
            </Field>
            <Field label="token 预算" hint="留空用 daemon 默认值">
              <Input type="number" min={1} step={1} value={tokenBudget} onChange={(e) => setTokenBudget(e.target.value)} />
            </Field>
          </div>
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
            <span className="text-sm font-medium text-zinc-700">类型</span>
            {SEARCH_TYPES.map((t) => {
              const on = types.includes(t)
              return (
                <Check
                  key={t}
                  label={labelled(TYPE_LABEL, t)}
                  checked={on}
                  disabled={on && types.length === 1}
                  onChange={(e) => toggle(t, e.target.checked)}
                />
              )
            })}
          </div>
          <div className="flex flex-col gap-2">
            <Check label={ALL_PLATFORMS_LABEL} checked={allPlatforms} onChange={(e) => setAllPlatforms(e.target.checked)} />
            <Check
              checked={rerank}
              onChange={(e) => setRerank(e.target.checked)}
              label={
                <>
                  重排（rerank: true） <ModelCall>调用重排模型</ModelCall>
                </>
              }
            />
          </div>
          <div className="flex flex-wrap items-center gap-3">
            <Button type="submit" disabled={!query.trim() || search.isPending}>
              {search.isPending ? '检索中…' : '检索'}
            </Button>
            <ModelCall>查询会发送到嵌入模型（后端 {health.data?.modelBackend ?? '…'}）计算向量</ModelCall>
            <Muted>不更新记忆的访问时间（touch: false）；daemon 会记一条召回记录。</Muted>
          </div>
        </form>
      </Card>

      {search.error && <ErrorNotice error={search.error} />}
      {res && (
        <Card title={`结果（${res.results.length}）`}>
          <Fields
            rows={[
              ['用掉的 token', res.tokensUsed],
              ['无结果（gap）', res.gap ? '是' : '否'],
              [
                '耗时',
                `嵌入 ${ms(res.latencyMs.embed)} · 召回 ${ms(res.latencyMs.recall)} · 重排 ${ms(res.latencyMs.rerank)} · 组装 ${ms(res.latencyMs.assemble)} · 合计 ${ms(res.latencyMs.total)}`,
              ],
              ['召回记录', <span className="font-mono">{res.retrievalId}</span>],
            ]}
          />
          {res.results.length > 0 && (
            <div className="mt-4 overflow-x-auto">
              <table className="w-full text-sm">
                <thead>
                  <tr className="text-left text-zinc-500">
                    <th className="px-2 py-1.5 font-normal">名次</th>
                    <th className="px-2 py-1.5 font-normal">内容</th>
                    <th className="px-2 py-1.5 font-normal">类型</th>
                    <th className="px-2 py-1.5 font-normal">状态</th>
                    <th className="px-2 py-1.5 text-right font-normal">score</th>
                    <th className="px-2 py-1.5 text-right font-normal">rerankScore</th>
                  </tr>
                </thead>
                <tbody>
                  {res.results.map((hit, i) => (
                    <tr key={hit.memory.id} className="border-t border-zinc-100 align-top">
                      <td className="px-2 py-1.5 tabular-nums">{i + 1}</td>
                      <td className="px-2 py-1.5">
                        <Link to={`/memories/${hit.memory.id}`} className="hover:underline">{hit.memory.text}</Link>
                      </td>
                      <td className="px-2 py-1.5"><TypeBadge type={hit.memory.type} /></td>
                      <td className="px-2 py-1.5"><StatusBadge status={hit.memory.status} /></td>
                      <td className="px-2 py-1.5 text-right tabular-nums">{score(hit.score)}</td>
                      <td className="px-2 py-1.5 text-right tabular-nums">{score(hit.rerankScore)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </Card>
      )}
    </div>
  )
}
