import { useQuery } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import { NavLink, Outlet } from 'react-router'
import { api } from '../api/client'

const NAV = [
  ['/', '概览'],
  ['/memories', '记忆列表'],
  ['/new', '新增'],
  ['/search', '检索调试'],
  ['/context', '上下文预览'],
] as const

/** The daemon and this UI are released with one version; a mismatch means the UI was built for another daemon. */
function VersionBanner() {
  const health = useQuery({ queryKey: ['health'], queryFn: api.health })
  const ui = useQuery({ queryKey: ['uiVersion'], queryFn: api.uiVersion, staleTime: Infinity })
  if (health.error) {
    return <Banner>连不上 daemon：{health.error.message}</Banner>
  }
  if (!health.data || !ui.data || health.data.version === ui.data.version) return null
  return (
    <Banner>
      界面版本 {ui.data.version} 与 daemon 版本 {health.data.version ?? '（未报告）'}{' '}
      不一致，部分功能可能不可用。请用与 daemon 相同版本的源码运行 <code>make ui</code>。
    </Banner>
  )
}

const Banner = ({ children }: { children: ReactNode }) => (
  <div role="status" className="bg-amber-100 px-4 py-2 text-sm text-amber-950">{children}</div>
)

export function Layout() {
  return (
    <div className="min-h-screen">
      <VersionBanner />
      <header className="border-b border-zinc-200 bg-white">
        <nav className="mx-auto flex max-w-6xl items-center gap-1 px-4">
          <span className="mr-4 py-3 font-semibold">记忆管理</span>
          {NAV.map(([to, label]) => (
            <NavLink
              key={to}
              to={to}
              end={to === '/'}
              className={({ isActive }) =>
                `border-b-2 px-3 py-3 text-sm ${isActive ? 'border-zinc-900 font-medium' : 'border-transparent text-zinc-600 hover:text-zinc-900'}`
              }
            >
              {label}
            </NavLink>
          ))}
        </nav>
      </header>
      <main className="mx-auto max-w-6xl px-4 py-6">
        <Outlet />
      </main>
    </div>
  )
}
