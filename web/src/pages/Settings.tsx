import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState, type ReactNode } from 'react'
import { api } from '../api/client'
import { ApiError } from '../api/client'
import type {
  KeySettingName,
  LlamaJob,
  LlamaSource,
  LlamaStatus,
  ModelSettings,
  ModelSettingsTest,
  ProbeResult,
  TextSettingName,
} from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { Icon } from '../components/icons'
import { Badge, Button, Card, Field, Fields, Input, Loading, Note, RadioCards, Select, Spinner } from '../components/ui'
import {
  changed,
  DEFAULT_URLS,
  formOf,
  keysNeeded,
  lockedBy,
  mayReembed,
  proposal,
  SOURCE_LABEL,
  type Form,
  type Provider,
} from '../lib/settings'

/** How often, and how long, the page asks whether the daemon is back after a save. */
const POLL_MS = 1_500
const RESTART_WAIT_MS = 120_000

const KEY_LABEL: Record<KeySettingName, string> = { llmKey: '生成模型的 API key', modelKey: '检索模型的 API key' }

export function SettingsPage() {
  const queryClient = useQueryClient()
  const settings = useQuery({ queryKey: ['model-settings'], queryFn: api.modelSettings })
  // Kept here, not in the form: the form starts over from the settings the restarted daemon reports.
  const [restart, setRestart] = useState<Restart | null>(null)

  // After a save, wait for a daemon that started later than the one that took the save.
  useEffect(() => {
    if (restart?.phase !== 'waiting') return
    const timer = setInterval(async () => {
      const h = await api.health().catch(() => null)
      if (h?.ok && h.startedAt && h.startedAt !== restart.before) {
        clearInterval(timer)
        setRestart({ phase: 'back' })
        void queryClient.invalidateQueries()
      } else if (Date.now() - restart.since > RESTART_WAIT_MS) {
        clearInterval(timer)
        setRestart({ phase: 'timeout' })
      }
    }, POLL_MS)
    return () => clearInterval(timer)
  }, [restart, queryClient])

  if (settings.isPending) return <Loading />
  if (settings.isError) return <ErrorNotice error={settings.error} />
  return (
    <div className="space-y-6">
      {restart && <RestartState restart={restart} />}
      <SettingsForm
        key={settings.dataUpdatedAt}
        settings={settings.data}
        restarting={restart?.phase === 'waiting'}
        onSaved={(before) => setRestart({ phase: 'waiting', before, since: Date.now() })}
      />
    </div>
  )
}

type Restart = { phase: 'waiting'; before?: string; since: number } | { phase: 'back' } | { phase: 'timeout' }

function SettingsForm({ settings: s, restarting, onSaved }: {
  settings: ModelSettings
  restarting: boolean
  onSaved: (before?: string) => void
}) {
  const [form, setForm] = useState<Form>(() => formOf(s))
  const [tested, setTested] = useState<{ form: Form; result: ModelSettingsTest } | null>(null)
  const set = (name: keyof Form) => (value: string) => setForm((f) => ({ ...f, [name]: value }))

  // The form travels with the call: the fields stay editable while it runs, and a result belongs to what was sent.
  const test = useMutation({
    mutationFn: (sent: Form) => api.testModelSettings(proposal(sent)),
    onSuccess: (result, sent) => setTested({ form: sent, result }),
  })
  const apply = useMutation({
    mutationFn: async (sent: Form) => {
      const before = (await api.health().catch(() => null))?.startedAt
      const res = await api.applyModelSettings(proposal(sent))
      return { res, before }
    },
    onSuccess: ({ res, before }, sent) => {
      setTested({ form: sent, result: res.test })
      if (res.saved) onSaved(before)
    },
  })


  const f = s.fields
  const provider = form.modelProvider as Provider
  const edits = changed(form, s)
  const needed = keysNeeded(form, s)
  const current = tested && JSON.stringify(tested.form) === JSON.stringify(form) ? tested.result : null
  const busy = test.isPending || apply.isPending || restarting
  const blocked = !s.editable || edits.length === 0 || needed.length > 0 || busy

  const text = (name: TextSettingName, label: string, hint?: ReactNode, placeholder?: string) => {
    const locked = lockedBy(f[name].source)
    return (
      <Field label={label} hint={locked ?? hint}>
        <Input
          value={form[name]}
          onChange={(e) => set(name)(e.target.value)}
          disabled={!!locked || !s.editable}
          placeholder={placeholder}
          spellCheck={false}
          autoComplete="off"
        />
      </Field>
    )
  }
  const key = (name: KeySettingName, hint?: ReactNode) => {
    const locked = lockedBy(f[name].source)
    const missing = needed.includes(name)
    return (
      <Field
        label={KEY_LABEL[name]}
        hint={
          locked ??
          (missing ? (
            <span className="text-amber-700">地址换到了另一台主机：请重新填写 key，已保存的 key 不会发到新地址</span>
          ) : (
            hint
          ))
        }
      >
        <Input
          type="password"
          value={form[name]}
          onChange={(e) => set(name)(e.target.value)}
          disabled={!!locked || !s.editable}
          placeholder={f[name].set ? '已设置（不显示）；留空保持不变' : '未设置'}
          autoComplete="new-password"
          spellCheck={false}
        />
      </Field>
    )
  }

  return (
    <div className="space-y-6">
      {s.rolledBack && (
        <Note>
          上次保存的设置没能启动，已恢复为之前的设置（{s.rolledBack.at}）：
          <span className="break-words">{s.rolledBack.error}</span>
        </Note>
      )}
      {!s.editable && (
        <Note>
          这个 daemon 启动时没有指定 <code>--env-file</code>，网页里只能查看设置。用{' '}
          <code>bash scripts/install.sh service --apply</code> 安装为服务后即可在这里保存。
        </Note>
      )}
      {s.mock && <Note tone="gray">daemon 以 MEMORY_MOCK_MODELS=1 运行：检索模型是测试用的假模型，与这里的设置无关。</Note>}

      <Card title="正在使用" description="daemon 当前连接的检索模型">
        <Fields
          rows={[
            ['方式', s.models.backend === 'llama-server' ? '本机 llama-server' : s.models.backend === 'api' ? '托管 API' : s.models.backend],
            ['embedding', `${s.models.embedModel}（${s.models.dim} 维）`],
            ['rerank', s.models.rerankModel ?? (s.rerank ? '—' : '未启用')],
            ['配置文件', s.envFile ?? '—'],
          ]}
        />
      </Card>

      <Card title="生成模型（抽取）" description="从对话里抽取记忆时调用；OpenAI 兼容接口">
        <div className="grid gap-4 md:grid-cols-2">
          {text('llmBaseUrl', '接口地址', <Source name="llmBaseUrl" s={s} />)}
          {text('llmModel', '模型', <Source name="llmModel" s={s} />)}
          {key('llmKey', <Source name="llmKey" s={s} />)}
        </div>
      </Card>

      <Card title="检索模型（embedding 与 rerank）" description="写入和检索记忆时计算向量、对候选重新排序">
        <div className="space-y-4">
          <RadioCards
            name="modelProvider"
            value={provider}
            onChange={(v) => (lockedBy(f.modelProvider.source) || !s.editable ? undefined : set('modelProvider')(v))}
            options={[
              { value: 'api', title: '托管 API', description: '用 API key 调用 OpenAI 兼容的 embedding 和 rerank 接口' },
              { value: 'self-hosted', title: '本机 llama-server', description: '自己运行的两个 llama-server，不需要 key' },
            ]}
          />
          <div className="grid gap-4 md:grid-cols-2">
            {text('embedUrl', 'embedding 地址', '留空使用默认地址', DEFAULT_URLS[provider]?.embedUrl)}
            {text('rerankUrl', 'rerank 地址', s.rerank ? '留空使用默认地址' : 'rerank 未启用（mem0Rerank: false）', DEFAULT_URLS[provider]?.rerankUrl)}
            {provider === 'api' && text('embedModel', 'embedding 模型')}
            {provider === 'api' && text('rerankModel', 'rerank 模型')}
            {provider === 'api' && key('modelKey', '留空时使用生成模型的 key')}
          </div>
          {provider === 'self-hosted' && (
            <Note tone="gray">每个 llama-server 只加载一个模型，模型名由它自己报告。还没有运行 llama-server 的话，可以用下面的一键安装。</Note>
          )}
        </div>
      </Card>

      <LlamaCard
        onUse={
          s.editable && !lockedBy(f.modelProvider.source)
            ? () => setForm((x) => ({ ...x, modelProvider: 'self-hosted', embedUrl: '', rerankUrl: '' }))
            : undefined
        }
        using={provider === 'self-hosted' && !form.embedUrl.trim() && !form.rerankUrl.trim()}
        saved={edits.length === 0}
      />

      {mayReembed(form, s) && (
        <Note>
          更换 embedding 模型后，daemon 重启时会用新模型重新计算所有记忆的向量；记忆多时需要一段时间，期间检索结果可能不完整。
        </Note>
      )}

      <div className="flex flex-wrap items-center gap-2">
        <Button variant="secondary" disabled={busy || needed.length > 0} onClick={() => test.mutate(form)}>
          {test.isPending ? <Spinner /> : <Icon name="check" className="size-4" />}
          测试连接
        </Button>
        <Button disabled={blocked} onClick={() => apply.mutate(form)}>
          {apply.isPending ? <Spinner className="text-white" /> : <Icon name="refresh" className="size-4" />}
          保存并重启 daemon
        </Button>
        {edits.length === 0 && s.editable && <span className="text-sm text-zinc-500">没有改动</span>}
        <span className="text-xs text-zinc-500">保存前会先测试；测试会请求一次生成接口的模型列表，不调用模型生成</span>
      </div>

      {(test.error || apply.error) && <ErrorNotice error={(test.error ?? apply.error) as Error} />}
      {current && <TestResult result={current} saved={apply.data?.res.saved} />}
    </div>
  )
}

function Source({ name, s }: { name: TextSettingName | KeySettingName; s: ModelSettings }) {
  const field = s.fields[name]
  return <span>来源：{SOURCE_LABEL[field.source]}</span>
}

function probeBadge(p: ProbeResult) {
  if (p.ok === true) return <Badge tone="green">正常</Badge>
  if (p.ok === false) return <Badge tone="red">失败</Badge>
  return <Badge tone="amber">无法确认</Badge>
}

function TestResult({ result, saved }: { result: ModelSettingsTest; saved?: boolean }) {
  const r = result.retrieval
  return (
    <Card
      title="测试结果"
      description={result.ok ? '设置可用' : saved === false ? '测试没有通过，设置没有保存' : '测试没有通过'}
    >
      <div className="space-y-3 text-sm">
        <div className="flex flex-wrap items-start gap-2">
          {probeBadge(r)}
          <span className="font-medium text-zinc-900">检索模型</span>
          <span className="min-w-0 break-words text-zinc-600">
            {r.mock
              ? '假模型（MEMORY_MOCK_MODELS=1），未连接'
              : r.ok
                ? `${r.embedModel}（${r.dim} 维）${r.rerankModel ? ` · ${r.rerankModel}` : ''}`
                : r.error}
          </span>
        </div>
        {r.reembed && <Note>新的 embedding 模型与当前不同：重启后会重新计算所有记忆的向量。</Note>}
        <div className="flex flex-wrap items-start gap-2">
          {probeBadge(result.generation)}
          <span className="font-medium text-zinc-900">生成模型</span>
          <span className="min-w-0 break-words text-zinc-600">
            {result.generation.error ?? result.generation.warning ?? '接口和 key 可用'}
          </span>
        </div>
      </div>
    </Card>
  )
}

function RestartState({ restart }: { restart: Restart }) {
  if (restart.phase === 'waiting') return <Loading>已保存，daemon 正在用新设置重启…</Loading>
  if (restart.phase === 'back') return <Note tone="gray">daemon 已用新设置重启。若新设置没能启动，页面顶部会说明并已恢复为原来的设置。</Note>
  return <Note>两分钟内没等到 daemon 重启完成。请查看 daemon 日志，或刷新页面再看。</Note>
}

const STEP_LABEL: Record<LlamaJob['steps'][number]['id'], string> = {
  llama: '安装 llama.cpp（llama-server）',
  embed: '下载 BGE-M3（embedding）',
  rerank: '下载 bge-reranker-v2-m3（rerank）',
  services: '注册并启动两个 llama-server 服务',
  ready: '等待模型加载完成',
}

const STEP_TONE = { pending: 'gray', running: 'blue', done: 'green', failed: 'red' } as const
const STEP_TEXT = { pending: '等待', running: '进行中', done: '完成', failed: '失败' } as const

const megabytes = (n: number) => `${(n / 1024 / 1024).toFixed(0)} MB`

/** The one-click self-hosted models: llama.cpp, the BGE pair and a service for each (`install.sh models`). */
function LlamaCard({ onUse, using, saved }: { onUse?: () => void; using: boolean; saved: boolean }) {
  const queryClient = useQueryClient()
  const [source, setSource] = useState<LlamaSource>('huggingface')
  const status = useQuery({
    queryKey: ['llama'],
    queryFn: api.llama,
    retry: false,
    refetchInterval: (q) => (q.state.data?.job?.state === 'running' ? 2_000 : false),
    // A download takes minutes: keep the steps current while the page is in another tab.
    refetchIntervalInBackground: true,
  })
  const install = useMutation({
    mutationFn: () => api.llamaInstall(source),
    // A fetch, not only the data: polling starts after a fetch that finds the run going.
    onSuccess: (data) => {
      queryClient.setQueryData(['llama'], data)
      void queryClient.invalidateQueries({ queryKey: ['llama'] })
    },
  })
  const description = '下载约 1.27 GB 的模型文件，在本机 127.0.0.1:8081 和 8082 各运行一个 llama-server，不需要 API key'
  if (status.isPending) return <Card title="一键安装本机检索模型" description={description}><Loading /></Card>
  if (status.isError) {
    const missing = status.error instanceof ApiError && status.error.status === 404
    return (
      <Card title="一键安装本机检索模型" description={description}>
        {missing ? (
          <Note>需要服务安装的安装器副本：运行 <code>bash scripts/install.sh service --apply</code> 后再来。也可以在终端里运行 <code>bash scripts/install.sh models --apply</code>。</Note>
        ) : (
          <ErrorNotice error={status.error} />
        )}
      </Card>
    )
  }
  const s: LlamaStatus = status.data
  const job = s.job
  const running = job?.state === 'running'
  const ready = s.models.every((m) => m.healthy)
  return (
    <Card title="一键安装本机检索模型" description={description}>
      <div className="space-y-4">
        <Fields
          rows={[
            ['llama-server', s.llamaServer ?? (s.brew ? '未找到：将用 Homebrew 安装（brew install llama.cpp）' : '未找到')],
            ...s.models.map(
              (m): [string, ReactNode] => [
                m.name,
                <span className="flex flex-wrap items-center gap-1.5">
                  <Badge tone={m.downloaded ? 'green' : 'gray'}>{m.downloaded ? '已下载' : '未下载'}</Badge>
                  <Badge tone={m.installed ? 'green' : 'gray'}>{m.installed ? '服务已注册' : '未注册服务'}</Badge>
                  <Badge tone={m.healthy ? 'green' : 'gray'}>{m.healthy ? `运行中 ${m.url}` : '未运行'}</Badge>
                </span>,
              ],
            ),
          ]}
        />
        {!s.llamaServer && !s.brew && (
          <Note>
            这台机器上没有 llama-server，也没有 Homebrew。请先安装 llama.cpp（Linux 可按 docs/self-hosted-models.md 编译，或下载
            官方 release），让 llama-server 在 PATH 上，再回来点安装。
          </Note>
        )}
        <div className="flex flex-wrap items-end gap-3">
          <Field label="下载来源">
            <Select value={source} onChange={(e) => setSource(e.target.value as LlamaSource)} disabled={running}>
              <option value="huggingface">huggingface.co</option>
              <option value="hf-mirror">hf-mirror.com（国内镜像）</option>
            </Select>
          </Field>
          <Button variant="secondary" disabled={running || install.isPending} onClick={() => install.mutate()}>
            {running || install.isPending ? <Spinner /> : <Icon name="download" className="size-4" />}
            {ready ? '重新检查并安装' : '一键安装并启动'}
          </Button>
          {ready && onUse && (
            <Button disabled={using} onClick={onUse}>
              <Icon name="check" className="size-4" />
              {!using ? '改用本机模型' : saved ? '正在使用本机模型' : '已填入，点下方保存'}
            </Button>
          )}
        </div>
        {install.error && <ErrorNotice error={install.error as Error} />}
        {job && <LlamaSteps job={job} />}
      </div>
    </Card>
  )
}

function LlamaSteps({ job }: { job: LlamaJob }) {
  return (
    <div className="space-y-2 rounded-lg border border-zinc-200 p-3 text-sm">
      <p className="text-xs text-zinc-500">
        {job.state === 'running' ? '正在安装' : job.state === 'done' ? '上次安装完成' : '上次安装失败'} · 开始于 {job.startedAt}
        {job.finishedAt ? ` · 结束于 ${job.finishedAt}` : ''} · 来源 {job.source}
      </p>
      <ol className="space-y-2">
        {job.steps.map((step) => (
          <li key={step.id} className="space-y-1">
            <div className="flex flex-wrap items-center gap-2">
              <Badge tone={STEP_TONE[step.state]}>{STEP_TEXT[step.state]}</Badge>
              <span className="text-zinc-900">{STEP_LABEL[step.id] ?? step.label}</span>
              {step.total != null && step.done != null && step.state !== 'pending' && (
                <span className="text-xs text-zinc-500">
                  {megabytes(step.done)} / {megabytes(step.total)}
                </span>
              )}
            </div>
            {step.total != null && step.done != null && step.state === 'running' && (
              <div className="h-1.5 overflow-hidden rounded-full bg-zinc-100">
                <div className="h-full bg-brand-500" style={{ width: `${Math.min(100, (100 * step.done) / step.total)}%` }} />
              </div>
            )}
            {step.detail && <p className="text-xs break-words text-zinc-500">{step.detail}</p>}
          </li>
        ))}
      </ol>
      {job.error && <p className="break-words text-red-700">{job.error}</p>}
    </div>
  )
}
