import * as Dialog from '@radix-ui/react-dialog'
import { useQuery } from '@tanstack/react-query'
import { Suspense, useEffect, useState, type ReactNode } from 'react'
import { Link, NavLink, Outlet, useLocation } from 'react-router'
import { api } from '../api/client'
import { formatNumber } from '../lib/format'
import { NAV, pageMeta } from '../lib/nav'
import { Icon } from './icons'
import { cx, FOCUS, Loading } from './ui'

/** The overview reads the same open conflicts, so the badge and its card share one request. */
export const OPEN_CONFLICTS_LIMIT = 200
export const openConflictsQuery = {
  queryKey: ['uncertainties', { status: 'open', limit: OPEN_CONFLICTS_LIMIT }],
  queryFn: () => api.listUncertainties({ status: 'open', limit: OPEN_CONFLICTS_LIMIT }),
} as const

/** The daemon and this UI are released with one version; a mismatch means the UI was built for another daemon. */
function VersionBanner() {
  const health = useQuery({ queryKey: ['health'], queryFn: api.health })
  const ui = useQuery({ queryKey: ['uiVersion'], queryFn: api.uiVersion, staleTime: Infinity })
  if (health.error) {
    return <Banner tone="red">连不上 daemon：{health.error.message}</Banner>
  }
  if (!health.data || !ui.data || health.data.version === ui.data.version) return null
  return (
    <Banner>
      界面版本 {ui.data.version} 与 daemon 版本 {health.data.version ?? '（未报告）'}{' '}
      不一致，部分功能可能不可用。请用与 daemon 相同版本的源码运行 <code>make ui</code>。
    </Banner>
  )
}

const Banner = ({ tone = 'amber', children }: { tone?: 'amber' | 'red'; children: ReactNode }) => (
  <div
    role="status"
    className={cx(
      'flex items-center gap-2 border-b px-4 py-2 text-sm',
      tone === 'red' ? 'border-red-200 bg-red-50 text-red-800' : 'border-amber-200 bg-amber-50 text-amber-950',
    )}
  >
    <Icon name="alert" className="size-4 shrink-0" />
    <span>{children}</span>
  </div>
)

function NavCount({ count }: { count: number }) {
  return (
    <span className="ml-auto rounded-full bg-red-500 px-1.5 text-[11px] leading-[18px] font-semibold text-white tabular-nums">
      {count >= OPEN_CONFLICTS_LIMIT ? `${OPEN_CONFLICTS_LIMIT}+` : formatNumber(count)}
    </span>
  )
}

function Nav({ onNavigate }: { onNavigate?: () => void }) {
  const conflicts = useQuery(openConflictsQuery)
  const open = conflicts.data?.uncertainties.length ?? 0
  return (
    <nav className="flex-1 space-y-5 overflow-y-auto px-3 py-4" aria-label="主导航">
      {NAV.map((group) => (
        <div key={group.title}>
          <p className="mb-1 px-2 text-xs font-medium text-zinc-400">{group.title}</p>
          <ul className="space-y-0.5">
            {group.items.map((item) => (
              <li key={item.to}>
                <NavLink
                  to={item.to}
                  end={item.to === '/'}
                  onClick={onNavigate}
                  className={({ isActive }) =>
                    cx(
                      'flex items-center gap-2.5 rounded-md px-2 py-1.5 text-sm transition-colors',
                      FOCUS,
                      isActive
                        ? 'bg-brand-50 font-medium text-brand-700'
                        : 'text-zinc-600 hover:bg-zinc-100 hover:text-zinc-900',
                    )
                  }
                >
                  <Icon name={item.icon} className="size-4 shrink-0" />
                  {item.label}
                  {item.to === '/conflicts' && open > 0 && <NavCount count={open} />}
                </NavLink>
              </li>
            ))}
          </ul>
        </div>
      ))}
    </nav>
  )
}

const Brand = () => (
  <Link to="/" className={cx('flex items-center gap-2 rounded-md font-semibold text-zinc-900', FOCUS)}>
    <span className="grid size-7 place-items-center rounded-lg bg-brand-600 text-sm text-white">忆</span>
    记忆管理
  </Link>
)

/** Whether the daemon answers, and its version. */
function ServiceStatus() {
  const health = useQuery({ queryKey: ['health'], queryFn: api.health })
  const ok = health.data?.ok
  const text = health.error ? '连不上 daemon' : health.data ? (ok ? 'daemon 运行中' : 'daemon 异常') : '正在连接…'
  return (
    <div className="flex items-center gap-2 border-t border-zinc-200 px-5 py-3 text-xs text-zinc-500">
      <span
        className={cx(
          'size-2 rounded-full',
          health.error || ok === false ? 'bg-red-500' : ok ? 'bg-emerald-500' : 'bg-zinc-300',
        )}
      />
      <span>{text}</span>
      {health.data?.version && <span className="ml-auto font-mono">v{health.data.version}</span>}
    </div>
  )
}

/** Phones and narrow windows get the sidebar as a drawer behind a menu button. */
function MobileBar() {
  const [open, setOpen] = useState(false)
  return (
    <Dialog.Root open={open} onOpenChange={setOpen}>
      <div className="sticky top-0 z-30 flex h-14 items-center gap-3 border-b border-zinc-200 bg-white/90 px-4 backdrop-blur lg:hidden">
        <Dialog.Trigger asChild>
          <button
            type="button"
            aria-label="打开菜单"
            className={cx('-ml-1.5 grid size-9 place-items-center rounded-md text-zinc-600 hover:bg-zinc-100', FOCUS)}
          >
            <Icon name="menu" className="size-5" />
          </button>
        </Dialog.Trigger>
        <Brand />
      </div>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 animate-fade-in bg-zinc-900/30 lg:hidden" />
        <Dialog.Content className="fixed inset-y-0 left-0 z-50 flex w-64 animate-drawer-in flex-col bg-white shadow-xl lg:hidden">
          <Dialog.Title className="sr-only">导航</Dialog.Title>
          <Dialog.Description className="sr-only">选择要打开的页面</Dialog.Description>
          <div className="flex h-14 items-center justify-between border-b border-zinc-200 px-4">
            <Brand />
            <Dialog.Close
              aria-label="关闭菜单"
              className={cx('grid size-8 place-items-center rounded-md text-zinc-500 hover:bg-zinc-100', FOCUS)}
            >
              <Icon name="x" className="size-4" />
            </Dialog.Close>
          </div>
          <Nav onNavigate={() => setOpen(false)} />
          <ServiceStatus />
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  )
}

function PageHeader() {
  const { pathname } = useLocation()
  const meta = pageMeta(pathname)

  useEffect(() => {
    document.title = meta ? `${meta.title} · 记忆管理` : '记忆管理'
  }, [meta])
  // A new page starts at its top, as it would on a site with real page loads.
  useEffect(() => window.scrollTo(0, 0), [pathname])

  if (!meta) return null
  return (
    <header className="mb-6">
      {meta.parent && (
        <Link
          to={meta.parent.to}
          className={cx('mb-1 inline-flex items-center gap-1 rounded text-sm text-zinc-500 hover:text-zinc-900', FOCUS)}
        >
          <Icon name="chevronLeft" className="size-4" />
          {meta.parent.label}
        </Link>
      )}
      <h1 className="text-xl font-semibold tracking-tight text-zinc-900">{meta.title}</h1>
      {meta.description && <p className="mt-1 text-sm text-zinc-500">{meta.description}</p>}
    </header>
  )
}

export function Layout() {
  return (
    <div className="min-h-screen lg:pl-60">
      <aside className="fixed inset-y-0 left-0 z-20 hidden w-60 flex-col border-r border-zinc-200 bg-white lg:flex">
        <div className="flex h-14 items-center px-5">
          <Brand />
        </div>
        <Nav />
        <ServiceStatus />
      </aside>
      <MobileBar />
      <VersionBanner />
      <main className="mx-auto max-w-6xl px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
        <PageHeader />
        {/* Pages load when first opened (main.tsx); the nav stays while one does. */}
        <Suspense fallback={<Loading />}>
          <Outlet />
        </Suspense>
      </main>
    </div>
  )
}
