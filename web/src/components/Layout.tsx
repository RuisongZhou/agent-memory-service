import { useQuery } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import { NavLink, Outlet } from 'react-router'
import { api } from '../api/client'

// Two groups: browsing (records, entities) and editing records, then reviewing and tracing what agents and the daemon did.
const NAV = [
  [
    ['/', '概览'],
    ['/memories', '记忆列表'],
    ['/entities', '实体'],
    ['/new', '新增'],
    ['/search', '检索调试'],
    ['/context', '上下文预览'],
  ],
  [
    ['/imports', '导入审核'],
    ['/consolidation', 'consolidation'],
    ['/conflicts', '冲突'],
    ['/recalls', '召回记录'],
    ['/tasks', '任务'],
  ],
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
        {/* Wraps instead of scrolling: at phone width each group takes its own lines. */}
        <nav className="mx-auto flex max-w-6xl flex-wrap items-center gap-x-4 px-4">
          <span className="py-2.5 font-semibold sm:py-3">记忆管理</span>
          {NAV.map((group, i) => (
            <div key={i} className="flex flex-wrap items-center">
              {group.map(([to, label]) => (
                <NavLink
                  key={to}
                  to={to}
                  end={to === '/'}
                  className={({ isActive }) =>
                    `border-b-2 px-2 py-2.5 text-sm whitespace-nowrap sm:px-3 sm:py-3 ${isActive ? 'border-zinc-900 font-medium' : 'border-transparent text-zinc-600 hover:text-zinc-900'}`
                  }
                >
                  {label}
                </NavLink>
              ))}
            </div>
          ))}
        </nav>
      </header>
      <main className="mx-auto max-w-6xl px-4 py-6">
        <Outlet />
      </main>
    </div>
  )
}
