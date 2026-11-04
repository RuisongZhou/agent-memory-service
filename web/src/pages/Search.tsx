import { useMutation, useQuery } from '@tanstack/react-query'
import { useState, type FormEvent } from 'react'
import { Link } from 'react-router'
import { api } from '../api/client'
import type { MemoryType, SearchReq } from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { StatusBadge, TypeBadge } from '../components/memory'
import { Button, Card, Check, Chip, Field, Fields, Input, ModelCall, Muted, Select, TD, TH } from '../components/ui'
import { ALL_PLATFORMS_LABEL, labelled, projectOption, TYPE_LABEL } from '../lib/format'

// Instructions are not searchable: get_context selects them, and search rejects the type.
const SEARCH_TYPES: MemoryType[] = ['fact', 'experience', 'profile', 'skill']

const ms = (n: number) => `${n.toFixed(1)} ms`
const score = (n: number | undefined) => (n === undefined ? '—' : n.toFixed(4))

/** A score with a bar for its size, so a ranking can be read at a glance; the bar spans 0–1. */
const Score = ({ value }: { value: number | undefined }) => (
  <span className="inline-flex items-center justify-end gap-2 tabular-nums">
    {value !== undefined && (
      <span className="hidden h-1.5 w-16 overflow-hidden rounded-full bg-zinc-100 sm:block">
        <span className="block h-full rounded-full bg-brand-500" style={{ width: `${Math.max(0, Math.min(1, value)) * 100}%` }} />
      </span>
    )}
    {score(value)}
  </span>
)

export function SearchPage() {
  const facets = useQuery({ queryKey: ['facets'], queryFn: api.facets })
  const health = useQuery({ queryKey: ['health'], queryFn: api.health })
  const [query, setQuery] = useState('')
  const [project, setProject] = useState('')
  const [allPlatforms, setAllPlatforms] = useState(true)
  const [topK, setTopK] = useState('')
  const [tokenBudget, setTokenBudget] = useState('')
  const [types, setTypes] = useState<MemoryType[]>(['fact'])
  const [rerank, setRerank] = useState(true)
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
      // Sent either way: on, the reranker selects what is returned, as for the hooks; off keeps Mem0's top-k.
      rerank,
    })
  }
  const toggle = (type: MemoryType, on: boolean) =>
    setTypes(SEARCH_TYPES.filter((t) => (t === type ? on : types.includes(t))))

  const res = search.data
  return (
    <div className="space-y-4">
      <Card>
        <form onSubmit={submit} className="space-y-3">
          <Field label="查询">
            <Input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="输入一句 agent 可能会问的话，回车检索"
              autoFocus
            />
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
          <div className="flex flex-wrap items-center gap-1.5">
            <span className="mr-1 text-sm font-medium text-zinc-700">类型</span>
            {SEARCH_TYPES.map((t) => {
              const on = types.includes(t)
              return (
                <Chip
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
                  选择性召回（rerank: true）：重排后低于阈值的不返回 <ModelCall>调用重排模型</ModelCall>
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
                '选择性召回',
                res.selection
                  ? `返回 ${res.selection.admitted}/${res.selection.reranked} · 最高分 ${score(res.selection.top ?? undefined)} · 阈值 ${res.selection.floor.toFixed(2)}`
                  : '未启用（Mem0 top-k）',
              ],
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
                  <tr>
                    <TH>名次</TH>
                    <TH>内容</TH>
                    <TH>类型</TH>
                    <TH>状态</TH>
                    <TH className="text-right">score</TH>
                    <TH className="text-right">rerankScore</TH>
                  </tr>
                </thead>
                <tbody className="divide-y divide-zinc-100">
                  {res.results.map((hit, i) => (
                    <tr key={hit.memory.id} className="hover:bg-zinc-50">
                      <TD className="text-zinc-500 tabular-nums">{i + 1}</TD>
                      <TD>
                        <Link to={`/memories/${hit.memory.id}`} className="hover:text-brand-700 hover:underline">{hit.memory.text}</Link>
                        {hit.uri && (
                          <Link
                            to={`/tree?uri=${encodeURIComponent(hit.uri)}`}
                            title={hit.uri}
                            className="ml-2 text-xs whitespace-nowrap text-zinc-500 hover:text-brand-700 hover:underline"
                          >
                            出处与原始对话
                          </Link>
                        )}
                      </TD>
                      <TD><TypeBadge type={hit.memory.type} /></TD>
                      <TD><StatusBadge status={hit.memory.status} /></TD>
                      <TD className="text-right"><Score value={hit.score} /></TD>
                      <TD className="text-right"><Score value={hit.rerankScore} /></TD>
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
