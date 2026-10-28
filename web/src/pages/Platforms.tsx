import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState, type ReactNode } from 'react'
import { api, ApiError } from '../api/client'
import type { ConnectOptions, ConnectRes, PlatformActivity, PlatformHost } from '../api/types'
import { ErrorNotice } from '../components/ErrorNotice'
import { Modal } from '../components/Modal'
import { Badge, Button, Muted, Note, Pre, RefreshButton } from '../components/ui'
import { formatAgo, formatTime } from '../lib/format'
import {
  connectCommand,
  defaultOptions,
  homePath,
  platformMark,
  platformState,
  STATE_LOOK,
  trustSteps,
  verification,
  verifyPrompt,
} from '../lib/platforms'

/** How often the verification step reads the platforms' calls, and how long before it lists likely causes. */
const POLL_MS = 3_000
const PATIENCE_MS = 120_000

const cx = (...classes: (string | false | undefined)[]) => classes.filter(Boolean).join(' ')

const Spinner = () => (
  <span
    aria-hidden
    className="inline-block h-4 w-4 animate-spin rounded-full border-2 border-zinc-300 border-t-zinc-800"
  />
)

const Tick = ({ on }: { on: boolean }) => (
  <span
    aria-hidden
    className={cx(
      'grid h-5 w-5 shrink-0 place-items-center rounded-full text-xs',
      on ? 'bg-emerald-600 text-white' : 'border border-zinc-300 bg-white text-transparent',
    )}
  >
    ✓
  </span>
)

function Mark({ host, size = 'h-11 w-11' }: { host: PlatformHost; size?: string }) {
  const mark = platformMark(host)
  return (
    <span className={cx('grid shrink-0 place-items-center rounded-xl text-sm font-semibold', size, mark.className)}>
      {mark.text}
    </span>
  )
}

const activityOf = (activity: PlatformActivity[] | undefined, host: PlatformHost) =>
  activity?.find((a) => a.platform === host.platform)

/** One feature on a card: installed, not installed, or not offered by the platform. */
function Feature({ label, supported, on }: { label: string; supported: boolean; on: boolean }) {
  return (
    <li
      title={supported ? (on ? '已安装' : '未安装') : '这个平台不支持'}
      className={cx(
        'flex items-center gap-1.5 rounded-lg border px-2.5 py-1.5 text-xs',
        !supported
          ? 'border-dashed border-zinc-200 text-zinc-400 line-through'
          : on
            ? 'border-emerald-200 bg-emerald-50 text-emerald-900'
            : 'border-zinc-200 text-zinc-600',
      )}
    >
      <span aria-hidden>{!supported ? '–' : on ? '✓' : '○'}</span>
      {label}
    </li>
  )
}

function LastCalls({ activity }: { activity: PlatformActivity | undefined }) {
  if (!activity) return <span className="text-zinc-400">近 30 天没有调用</span>
  const hook = [activity.capture, activity.hookRecall].filter(Boolean).map((c) => c!.lastAt).sort().pop()
  const parts = [
    ...(hook ? [`hook ${formatAgo(hook)}`] : []),
    ...(activity.mcp ? [`MCP ${formatAgo(activity.mcp.lastAt)}`] : []),
    ...(activity.other && !hook && !activity.mcp ? [`${formatAgo(activity.other.lastAt)}（来源未记录）`] : []),
  ]
  return <span title={formatTime(activity.lastAt)}>最近调用：{parts.join(' · ')}</span>
}

function PlatformCard({ host, activity, onConnect, onTrust, onManage }: {
  host: PlatformHost
  activity: PlatformActivity | undefined
  onConnect: () => void
  onTrust: () => void
  onManage: (action: 'uninstall' | 'rollback') => void
}) {
  const state = platformState(host, activity)
  const look = STATE_LOOK[state]
  const installed = host.options ?? { capture: false, recall: false }
  const primary: Record<typeof state, [string, () => void] | null> = {
    error: null,
    missing: null,
    available: ['接入', onConnect],
    outdated: ['更新配置', onConnect],
    silent: ['完成信任步骤', onTrust],
    connected: ['查看步骤', onTrust],
  }
  const action = primary[state]
  return (
    <section className="flex min-w-0 flex-col rounded-xl border border-zinc-200 bg-white p-5 shadow-sm">
      <header className="flex items-start gap-3">
        <Mark host={host} />
        <div className="min-w-0 flex-1">
          <h2 className="font-semibold text-zinc-900">{host.label}</h2>
          <p className="truncate text-xs text-zinc-500" title={host.detected ?? undefined}>
            {host.detected ? `本机检测到 ${homePath(host.detected)}` : '本机没有检测到这个平台'}
          </p>
        </div>
        <Badge tone={look.tone}>{look.label}</Badge>
      </header>

      <ul className="mt-4 grid grid-cols-3 gap-2">
        <Feature label="MCP 工具" supported on={host.connected} />
        <Feature label="对话抓取" supported={host.capture} on={host.connected && installed.capture} />
        <Feature label="自动召回" supported={host.recall} on={host.connected && installed.recall} />
      </ul>

      <p className="mt-3 text-sm text-zinc-600">
        {host.connected ? <LastCalls activity={activity} /> : <span className="text-zinc-400">还没有接入</span>}
      </p>

      {state === 'silent' && (
        <p className="mt-3 rounded-lg bg-amber-50 px-3 py-2 text-sm text-amber-950">
          配置已经写好，但近 30 天没有收到调用：多半还没在 {host.label} 里信任或重启。
        </p>
      )}
      {state === 'outdated' && (
        <p className="mt-3 rounded-lg bg-amber-50 px-3 py-2 text-sm text-amber-950">
          配置和现在的 daemon 不一致（比如 binary 换了位置），重新写入一次即可。
        </p>
      )}
      {state === 'error' && (
        <p className="mt-3 rounded-lg bg-red-50 px-3 py-2 text-sm break-words text-red-800">{host.error}</p>
      )}

      <footer className="mt-auto flex flex-wrap items-center gap-2 pt-4">
        {action && <Button onClick={action[1]}>{action[0]}</Button>}
        {state === 'missing' && <Button variant="secondary" onClick={onConnect}>仍然接入</Button>}
        {state === 'silent' && <Button variant="secondary" onClick={onConnect}>重新写入</Button>}
        <span className="ml-auto flex gap-3 text-sm">
          {host.canRollback && (
            <button type="button" className="text-zinc-500 hover:text-zinc-900" onClick={() => onManage('rollback')}>
              回滚上次修改
            </button>
          )}
          {host.connected && (
            <button type="button" className="text-zinc-500 hover:text-red-700" onClick={() => onManage('uninstall')}>
              卸载
            </button>
          )}
        </span>
      </footer>
    </section>
  )
}

const STEPS = ['选择功能', '确认改动', '写入', '信任', '验证'] as const
const CHOOSE = 0
const PREVIEW = 1
const WRITE = 2
const TRUST = 3
const VERIFY = 4

function Stepper({ step }: { step: number }) {
  return (
    <ol className="mb-5 flex items-center gap-1.5 text-xs">
      {STEPS.map((label, i) => (
        <li key={label} className="flex flex-1 flex-col gap-1.5">
          <span className={cx('h-1 rounded-full', i <= step ? 'bg-zinc-900' : 'bg-zinc-200')} />
          <span className={cx(i === step ? 'font-medium text-zinc-900' : 'text-zinc-500')}>
            {i + 1}. {label}
          </span>
        </li>
      ))}
    </ol>
  )
}

/** A switch row: a whole-row label with a checkbox, a title and what turning it on means. */
function Toggle({ checked, onChange, title, children }: {
  checked: boolean
  onChange: (on: boolean) => void
  title: string
  children: ReactNode
}) {
  return (
    <label
      className={cx(
        'flex cursor-pointer gap-3 rounded-lg border p-3 transition-colors',
        checked ? 'border-zinc-900 bg-zinc-50' : 'border-zinc-200 hover:border-zinc-300',
      )}
    >
      <input type="checkbox" className="mt-0.5 h-4 w-4 accent-zinc-900" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span className="text-sm">
        <span className="font-medium text-zinc-900">{title}</span>
        <span className="mt-0.5 block text-zinc-600">{children}</span>
      </span>
    </label>
  )
}

const CHANGE_LOOK = {
  create: { label: '新建', tone: 'green' },
  update: { label: '修改', tone: 'blue' },
  delete: { label: '删除', tone: 'red' },
} as const

function Changes({ result }: { result: ConnectRes }) {
  return (
    <ul className="divide-y divide-zinc-100 rounded-lg border border-zinc-200">
      {result.changes.map((change) => {
        const path = homePath(change.path)
        const cut = path.lastIndexOf('/') + 1
        return (
          <li key={change.path} className="flex items-center gap-2 px-3 py-2 text-sm" title={change.path}>
            <Badge tone={CHANGE_LOOK[change.action].tone}>{CHANGE_LOOK[change.action].label}</Badge>
            <code className="shrink-0 text-xs font-medium text-zinc-900">{path.slice(cut)}</code>
            <code className="min-w-0 truncate text-xs text-zinc-500">{path.slice(0, cut)}</code>
          </li>
        )
      })}
    </ul>
  )
}

/** An installer run that failed: its reason, and its whole output on request. */
function Failure({ result }: { result: ConnectRes }) {
  return (
    <div role="alert" className="space-y-2 rounded-lg border border-red-200 bg-red-50 p-3 text-sm text-red-800">
      <p>{result.error}</p>
      <details>
        <summary className="cursor-pointer text-xs text-red-700">安装器的完整输出</summary>
        <Pre>{result.log.join('\n')}</Pre>
      </details>
    </div>
  )
}

function Footer({ children }: { children: ReactNode }) {
  return <div className="mt-5 flex flex-wrap justify-end gap-2">{children}</div>
}

/** Connect (or update) one platform: choose, preview, write, trust, verify. Opened at `start` with `start` options. */
function ConnectWizard({ host, start, onClose }: { host: PlatformHost; start: number; onClose: () => void }) {
  const queryClient = useQueryClient()
  const [step, setStep] = useState(start)
  const [options, setOptions] = useState<ConnectOptions>(() => defaultOptions(host))
  const [since, setSince] = useState(() => Date.now())
  const preview = useMutation({ mutationFn: () => api.connect({ host: host.id, action: 'preview', ...options }) })
  const apply = useMutation({
    mutationFn: () => api.connect({ host: host.id, action: 'apply', ...options }),
    onSuccess: (result) => {
      void queryClient.invalidateQueries({ queryKey: ['platforms'] })
      if (result.ok) {
        setSince(Date.now())
        setStep(TRUST)
      }
    },
  })
  const activity = useQuery({
    queryKey: ['platform-activity'],
    queryFn: api.platformActivity,
    refetchInterval: step === VERIFY ? POLL_MS : false,
  })
  const hooksOffered = host.capture || host.recall

  const goPreview = () => {
    preview.reset()
    preview.mutate(undefined, { onSuccess: (result) => result.ok && setStep(PREVIEW) })
  }
  const write = () => {
    setStep(WRITE)
    apply.mutate()
  }

  return (
    <Modal
      open
      onOpenChange={(open) => !open && onClose()}
      title={`${host.connected ? '更新' : '接入'} ${host.label}`}
      description="写入 shared-memory 的 MCP 服务和 hook，再在平台里信任，最后确认收到了调用。"
    >
      <Stepper step={step} />

      {step === CHOOSE && (
        <div className="space-y-3">
          <div className="flex gap-3 rounded-lg border border-zinc-200 bg-zinc-50 p-3 text-sm">
            <span aria-hidden>✓</span>
            <span>
              <span className="font-medium text-zinc-900">MCP 工具</span>
              <span className="mt-0.5 block text-zinc-600">agent 可以主动查询、保存和更正记忆。总会安装。</span>
            </span>
          </div>
          {host.capture && (
            <Toggle checked={options.capture} onChange={(capture) => setOptions({ ...options, capture })} title="对话抓取">
              每轮对话结束后自动交给 daemon 形成记忆，按会话所在的文件夹归到项目。
            </Toggle>
          )}
          {host.recall && (
            <Toggle checked={options.recall} onChange={(recall) => setOptions({ ...options, recall })} title="自动召回">
              会话开始和每次提问时自动注入相关记忆。每次提问都会作为检索词发给 daemon；用托管嵌入模型时还会发到嵌入 API。
            </Toggle>
          )}
          {!hooksOffered && (
            <Note tone="gray">{host.label} 只接 MCP 工具：agent 需要时主动调用，不会自动抓取对话。</Note>
          )}
          {preview.error && <ErrorNotice error={preview.error} />}
          {preview.data && !preview.data.ok && <Failure result={preview.data} />}
          <Footer>
            <Button variant="secondary" onClick={onClose}>取消</Button>
            <Button disabled={preview.isPending} onClick={goPreview}>
              {preview.isPending ? <><Spinner /> 正在生成预览…</> : '下一步：预览改动'}
            </Button>
          </Footer>
        </div>
      )}

      {step === PREVIEW && preview.data && (
        <div className="space-y-4">
          {preview.data.changes.length === 0 ? (
            <Note tone="gray">配置已经是最新的，不需要写入。可以直接去平台里完成信任。</Note>
          ) : (
            <>
              <p className="text-sm text-zinc-700">会改动这些文件，只动 shared-memory 自己的条目：</p>
              <Changes result={preview.data} />
            </>
          )}
          <ul className="space-y-1.5 text-sm text-zinc-600">
            <li>✓ 写入前先用临时 daemon 离线跑一遍 hook 命令，失败就不写</li>
            <li>✓ 每个文件改动前都会备份，之后可以一键回滚</li>
            <li>✓ 你的其他 MCP 服务、hook 和设置保持不变</li>
          </ul>
          <details className="text-sm">
            <summary className="cursor-pointer text-zinc-500">想在终端里自己运行？</summary>
            <Pre>{connectCommand(host, options)}</Pre>
          </details>
          <Footer>
            <Button variant="secondary" onClick={() => setStep(CHOOSE)}>上一步</Button>
            {preview.data.changes.length === 0 ? (
              <Button onClick={() => setStep(TRUST)}>下一步：在平台里信任</Button>
            ) : (
              <Button onClick={write}>写入配置</Button>
            )}
          </Footer>
        </div>
      )}

      {step === WRITE && (
        <div className="space-y-4">
          {apply.isPending && (
            <div className="flex items-center gap-3 rounded-lg border border-zinc-200 p-4 text-sm text-zinc-700">
              <Spinner />
              正在离线校验并写入配置，通常要 10–30 秒…
            </div>
          )}
          {apply.error && <ErrorNotice error={apply.error} />}
          {apply.data && !apply.data.ok && (
            <>
              <Failure result={apply.data} />
              <Muted>配置没有改动。处理好原因后可以重试。</Muted>
            </>
          )}
          {!apply.isPending && (
            <Footer>
              <Button variant="secondary" onClick={() => setStep(PREVIEW)}>上一步</Button>
              <Button onClick={write}>重试</Button>
            </Footer>
          )}
        </div>
      )}

      {step === TRUST && <TrustStep host={host} options={options} written={apply.data} onClose={onClose} onNext={() => setStep(VERIFY)} />}

      {step === VERIFY && (
        <VerifyStep
          host={host}
          options={options}
          since={since}
          activity={activityOf(activity.data?.platforms, host)}
          checkedAt={activity.dataUpdatedAt}
          error={activity.error}
          onBack={() => setStep(TRUST)}
          onClose={onClose}
        />
      )}
    </Modal>
  )
}

function TrustStep({ host, options, written, onClose, onNext }: {
  host: PlatformHost
  options: ConnectOptions
  written: ConnectRes | undefined
  onClose: () => void
  onNext: () => void
}) {
  const steps = trustSteps(host, options)
  const [done, setDone] = useState<ReadonlySet<number>>(new Set())
  const toggle = (i: number) =>
    setDone((old) => {
      const next = new Set(old)
      if (!next.delete(i)) next.add(i)
      return next
    })
  return (
    <div className="space-y-4">
      {written?.ok && (
        <div className="rounded-lg bg-emerald-50 px-3 py-2 text-sm text-emerald-900">
          配置已写入{written.changes.length > 0 && `（${written.changes.length} 个文件，已备份）`}。还差最后几步，只能在 {host.label} 里完成：
        </div>
      )}
      <ol className="space-y-2">
        {steps.map((s, i) => (
          <li key={s.title}>
            <button
              type="button"
              onClick={() => toggle(i)}
              className={cx(
                'flex w-full gap-3 rounded-lg border p-3 text-left transition-colors',
                done.has(i) ? 'border-emerald-200 bg-emerald-50/60' : 'border-zinc-200 hover:border-zinc-300',
              )}
            >
              <Tick on={done.has(i)} />
              <span className="text-sm">
                <span className={cx('font-medium', done.has(i) ? 'text-zinc-500 line-through' : 'text-zinc-900')}>
                  {i + 1}. {s.title}
                </span>
                <span className="mt-0.5 block text-zinc-600">{s.detail}</span>
              </span>
            </button>
          </li>
        ))}
      </ol>
      <Muted>做完一步点一下打勾。这些步骤平台不允许别的程序代做。</Muted>
      <Footer>
        <Button variant="secondary" onClick={onClose}>稍后再做</Button>
        <Button onClick={onNext}>{done.size === steps.length ? '都做完了，开始验证' : '下一步：验证'}</Button>
      </Footer>
    </div>
  )
}

function VerifyStep({ host, options, since, activity, checkedAt, error, onBack, onClose }: {
  host: PlatformHost
  options: ConnectOptions
  since: number
  activity: PlatformActivity | undefined
  checkedAt: number
  error: Error | null
  onBack: () => void
  onClose: () => void
}) {
  const checks = verification(host, options, activity, since)
  const working = checks.filter((c) => !c.optional).every((c) => c.seenAt)
  const waitedLong = checkedAt - since > PATIENCE_MS
  return (
    <div className="space-y-4">
      <div className="flex items-start gap-3 rounded-lg border border-zinc-200 bg-zinc-50 p-3 text-sm">
        <Mark host={host} size="h-8 w-8" />
        <span>
          <span className="font-medium text-zinc-900">现在去试一下</span>
          <span className="mt-0.5 block text-zinc-600">{verifyPrompt(host, options)}</span>
        </span>
      </div>
      <ul className="space-y-2">
        {checks.map((check) => (
          <li key={check.key} className="flex items-center gap-3 rounded-lg border border-zinc-200 p-3 text-sm">
            {check.seenAt ? <Tick on /> : <Spinner />}
            <span className="flex-1">
              <span className="font-medium text-zinc-900">{check.label}</span>
              {check.optional && <span className="ml-1 text-xs text-zinc-400">可选</span>}
              <span className="block text-zinc-600">{check.detail}</span>
            </span>
            <span className={cx('text-xs whitespace-nowrap', check.seenAt ? 'text-emerald-700' : 'text-zinc-400')}>
              {check.seenAt ? `收到 · ${formatTime(check.seenAt)}` : '等待中'}
            </span>
          </li>
        ))}
      </ul>
      {error && <ErrorNotice error={error} />}
      {working ? (
        <div className="rounded-lg bg-emerald-50 px-3 py-2 text-sm font-medium text-emerald-900">
          接好了：{host.label} 的调用已经到达记忆服务。
        </div>
      ) : (
        waitedLong && (
          <Note>
            还没收到。常见原因：没有完全退出再打开 {host.label}；hook 还没信任（Codex 每次重新写入后都要再信任）；
            用的是写入前就开着的旧会话。
          </Note>
        )
      )}
      <Muted>每 3 秒自动检查一次，只在这一步。</Muted>
      <Footer>
        <Button variant="secondary" onClick={onBack}>上一步</Button>
        <Button onClick={onClose}>{working ? '完成' : '先关掉'}</Button>
      </Footer>
    </div>
  )
}

const MANAGE = {
  uninstall: {
    title: (label: string) => `卸载 ${label} 的接入`,
    what: '从这些文件里移除 shared-memory 的 MCP 服务和 hook，其他内容不动。改动前会备份，之后可以回滚。',
    button: '卸载',
  },
  rollback: {
    title: (label: string) => `回滚 ${label} 的上次修改`,
    what: '把上次写入改过的文件恢复成写入前的样子。之后又被改过的文件不会被覆盖，回滚会整个取消。',
    button: '回滚',
  },
} as const

function ManageDialog({ host, action, onClose }: { host: PlatformHost; action: 'uninstall' | 'rollback'; onClose: () => void }) {
  const queryClient = useQueryClient()
  const run = useMutation({
    mutationFn: () => api.connect({ host: host.id, action }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['platforms'] }),
  })
  const look = MANAGE[action]
  return (
    <Modal open onOpenChange={(open) => !open && onClose()} title={look.title(host.label)} description={look.what}>
      {run.data?.ok ? (
        <div className="space-y-4">
          <div className="rounded-lg bg-emerald-50 px-3 py-2 text-sm text-emerald-900">
            {run.data.changes.length ? `完成，改动了 ${run.data.changes.length} 个文件。重启 ${host.label} 后生效。` : '没有需要改动的内容。'}
          </div>
          {run.data.changes.length > 0 && <Changes result={run.data} />}
          <Footer>
            <Button onClick={onClose}>关闭</Button>
          </Footer>
        </div>
      ) : (
        <div className="space-y-4">
          {action === 'uninstall' && (
            <ul className="space-y-1 text-sm">
              {host.files.map((file) => (
                <li key={file} className="truncate" title={file}><code className="text-xs">{homePath(file)}</code></li>
              ))}
            </ul>
          )}
          {run.error && <ErrorNotice error={run.error} />}
          {run.data && !run.data.ok && <Failure result={run.data} />}
          <Footer>
            <Button variant="secondary" onClick={onClose}>取消</Button>
            <Button variant="danger" disabled={run.isPending} onClick={() => run.mutate()}>
              {run.isPending ? <><Spinner /> 正在{look.button}…</> : look.button}
            </Button>
          </Footer>
        </div>
      )}
    </Modal>
  )
}

/** Shown when the daemon has no installer copy to run: the terminal does the same. */
function NoInstaller() {
  return (
    <section className="rounded-xl border border-zinc-200 bg-white p-5">
      <h2 className="font-semibold text-zinc-900">网页接入还没启用</h2>
      <p className="mt-1 text-sm text-zinc-600">
        这个 daemon 没有安装器副本，网页没法读写平台配置。在仓库里重新安装一次服务即可启用：
      </p>
      <Pre>bash scripts/install.sh service --apply</Pre>
      <p className="mt-3 text-sm text-zinc-600">也可以直接在终端接入，比如：</p>
      <Pre>bash scripts/install.sh connect --host codex --apply</Pre>
    </section>
  )
}

export function PlatformsPage() {
  const platforms = useQuery({ queryKey: ['platforms'], queryFn: api.platforms })
  const activity = useQuery({ queryKey: ['platform-activity'], queryFn: api.platformActivity })
  const [wizard, setWizard] = useState<{ host: PlatformHost; start: number } | null>(null)
  const [manage, setManage] = useState<{ host: PlatformHost; action: 'uninstall' | 'rollback' } | null>(null)
  const missingInstaller = platforms.error instanceof ApiError && platforms.error.code === 'NOT_FOUND'
  const report = platforms.data
  const refresh = () => {
    void platforms.refetch()
    void activity.refetch()
  }

  return (
    <div className="space-y-5">
      <header className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold text-zinc-900">接入平台</h1>
          <p className="mt-1 max-w-2xl text-sm text-zinc-600">
            把 agent 平台接到共享记忆：网页写入它的 MCP 服务和 hook，你在平台里信任并重启，最后在这里确认调用到了。
          </p>
        </div>
        <RefreshButton busy={platforms.isFetching || activity.isFetching} onClick={refresh} />
      </header>

      {missingInstaller ? (
        <NoInstaller />
      ) : platforms.error ? (
        <ErrorNotice error={platforms.error} />
      ) : !report ? (
        <div className="flex items-center gap-2 text-sm text-zinc-500"><Spinner /> 正在检测本机的平台…</div>
      ) : (
        <>
          {report.service.error && <Note>{report.service.error}</Note>}
          <div className="grid gap-4 md:grid-cols-2">
            {report.hosts.map((host) => (
              <PlatformCard
                key={host.id}
                host={host}
                activity={activityOf(activity.data?.platforms, host)}
                onConnect={() => setWizard({ host, start: CHOOSE })}
                onTrust={() => setWizard({ host, start: TRUST })}
                onManage={(action) => setManage({ host, action })}
              />
            ))}
          </div>
          <Muted>
            检测只看本机有没有平台的配置目录或命令；是否已接入、是否需要更新由安装器读取平台的配置判断。最近调用来自近 30 天的召回和写入记录。
          </Muted>
        </>
      )}

      {wizard && <ConnectWizard key={wizard.host.id} host={wizard.host} start={wizard.start} onClose={() => setWizard(null)} />}
      {manage && <ManageDialog host={manage.host} action={manage.action} onClose={() => setManage(null)} />}
    </div>
  )
}
