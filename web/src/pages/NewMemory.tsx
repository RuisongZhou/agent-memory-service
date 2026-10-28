import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { api, PLATFORM } from '../api/client'
import { useRefresh } from '../api/refresh'
import type { AddMemoriesReq, MemScope, ScopeKind, Task } from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { Button, Card, Check, Field, Fields, Input, MemoryLink, ModelCall, Muted, Textarea } from '../components/ui'
import { SCOPE_KINDS } from '../lib/filters'
import { byteLength, charLength, labelled, projectName, SCOPE_LABEL } from '../lib/format'

// docs/api.md, Writes: explicit text is at most 4 KB; an instruction also at most 1024 characters.
const MAX_TEXT_BYTES = 4096
const MAX_INSTRUCTION_CHARS = 1024

type Kind = 'fact' | 'instruction'

const finished = (task?: Task) => task?.status === 'completed' || task?.status === 'failed'

/** An instruction's scope: everywhere, the project it is written in, or this client's platform. */
function instructionScope(kind: ScopeKind, project: string): MemScope {
  if (kind === 'project') return { kind, project }
  if (kind === 'platform') return { kind, platform: PLATFORM }
  return { kind }
}

export function NewMemoryPage() {
  const facets = useQuery({ queryKey: ['facets'], queryFn: api.facets })
  const [kind, setKind] = useState<Kind>('fact')
  const [text, setText] = useState('')
  const [project, setProject] = useState('')
  const [session, setSession] = useState(PLATFORM)
  const [infer, setInfer] = useState(false)
  const [scopeKind, setScopeKind] = useState<ScopeKind>('project')
  const refresh = useRefresh()

  const body = text.trim()
  const bytes = byteLength(body)
  const chars = charLength(body)
  const problems = [
    !body && '内容不能为空',
    bytes > MAX_TEXT_BYTES && `超过 ${MAX_TEXT_BYTES} 字节`,
    kind === 'instruction' && chars > MAX_INSTRUCTION_CHARS && `指令超过 ${MAX_INSTRUCTION_CHARS} 个字符`,
    (!project.trim() || project.trim() === '*') && '需要一个具体的项目',
    !session.trim() && '需要会话',
  ].filter(Boolean)

  const request = (): AddMemoriesReq => {
    const scope = { project: project.trim(), session: session.trim() }
    if (kind === 'instruction') {
      return { scope, text: body, memoryType: 'instruction', memoryScope: instructionScope(scopeKind, scope.project) }
    }
    // Verbatim text is stored before the answer; generation can take minutes, so it is polled instead.
    return { scope, text: body, infer, ...(infer && { sync: false }) }
  }
  const add = useMutation({ mutationFn: () => api.addMemories(request()), onSuccess: refresh })

  const accepted = add.data
  const poll = useQuery({
    queryKey: ['task', accepted?.requestId],
    queryFn: async () => {
      const task = await api.getTask(accepted!.requestId)
      if (task.status === 'completed') refresh()
      return task
    },
    enabled: !!accepted && !finished(accepted),
    refetchInterval: (q) => (finished(q.state.data) ? false : 1000),
  })
  const task = poll.data ?? accepted
  const completed = task?.status === 'completed'

  const reset = () => {
    add.reset()
    setText('')
  }

  return (
    <div className="grid gap-4 lg:grid-cols-[1fr_20rem]">
      <Card title="新增记忆">
        <div className="space-y-4">
          <div className="flex gap-4">
            <Check
              type="radio"
              name="kind"
              label="事实：历史信息，检索时作为参考"
              checked={kind === 'fact'}
              onChange={() => setKind('fact')}
            />
            <Check
              type="radio"
              name="kind"
              label="指令：行为要求，注入上下文时 agent 应当遵循"
              checked={kind === 'instruction'}
              onChange={() => setKind('instruction')}
            />
          </div>
          <Field
            label="内容"
            hint={
              <>
                {bytes} / {MAX_TEXT_BYTES} 字节
                {kind === 'instruction' && `，${chars} / ${MAX_INSTRUCTION_CHARS} 字符`}
                。密钥、密码等凭据在保存前会替换为 [redacted:…]。
              </>
            }
          >
            <Textarea value={text} onChange={(e) => setText(e.target.value)} rows={5} />
          </Field>
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="项目">
              <Input list="projects" value={project} onChange={(e) => setProject(e.target.value)} />
              <datalist id="projects">
                {facets.data?.projects.map((p) => <option key={p.name} value={p.name} label={projectName(p.name)} />)}
              </datalist>
            </Field>
            <Field label="会话">
              <Input value={session} onChange={(e) => setSession(e.target.value)} />
            </Field>
          </div>
          {kind === 'fact' ? (
            <Check
              checked={infer}
              onChange={(e) => setInfer(e.target.checked)}
              label={
                <>
                  交给生成模型抽取事实（infer: true） <ModelCall>调用生成模型，产生费用</ModelCall>
                </>
              }
            />
          ) : (
            <div className="flex flex-col gap-1 text-sm">
              <span className="font-medium text-zinc-700">适用范围</span>
              <div className="flex gap-4">
                {SCOPE_KINDS.map((k) => (
                  <Check
                    key={k}
                    type="radio"
                    name="scope"
                    label={labelled(SCOPE_LABEL, k)}
                    checked={scopeKind === k}
                    onChange={() => setScopeKind(k)}
                  />
                ))}
              </div>
              <span className="text-xs text-zinc-500">平台级指令只对 {PLATFORM} 平台可见：写入平台取自连接。</span>
            </div>
          )}
          {kind === 'fact' && !infer && <Muted>不抽取时原样保存为一条事实，不调用生成模型。</Muted>}
          {add.error && <ErrorNotice error={add.error} />}
          {problems.length > 0 && body && <Muted>{problems.join('；')}</Muted>}
          <Button disabled={problems.length > 0 || add.isPending || !!accepted} onClick={() => add.mutate()}>
            {add.isPending ? '提交中…' : '保存'}
          </Button>
        </div>
      </Card>
      {task && (
        <Card title="结果" actions={<Button variant="secondary" onClick={reset}>继续新增</Button>}>
          <Fields
            rows={[
              ['请求', <span className="font-mono">{task.requestId}</span>],
              ['状态', finished(task) ? task.status : `${task.status}（每秒刷新）`],
              ['记忆', task.memoryIds.length ? task.memoryIds.map((id) => <div key={id}><MemoryLink id={id} /></div>) : '—'],
              ...(task.duplicate ? [['重复', '这个请求之前已经提交过'] as [string, string]] : []),
              ...(task.error ? [['错误', task.error] as [string, string]] : []),
            ]}
          />
          {poll.error && <ErrorNotice error={poll.error} />}
          {completed && task.memoryIds.length === 0 && <Muted>没有形成新记忆（可能与已有记忆重复）。</Muted>}
        </Card>
      )}
    </div>
  )
}
