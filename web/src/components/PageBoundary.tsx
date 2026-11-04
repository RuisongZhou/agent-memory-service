import { Component, useEffect, useState, type ReactNode } from 'react'
import { useLocation } from 'react-router'
import { chunkFailed, mayReload } from '../lib/reload'
import { Icon } from './icons'
import { Button, cx, Loading } from './ui'

/**
 * The open page, or what stopped it rendering. Without a boundary React unmounts the whole app on such an error: a
 * blank window until a manual refresh. Here the sidebar stays, and every navigation gives the page another try.
 */
export function PageBoundary({ children }: { children: ReactNode }) {
  const { key } = useLocation()
  return <Boundary navigation={key}>{children}</Boundary>
}

type BoundaryState = { navigation: string; error: Error | null }

class Boundary extends Component<{ navigation: string; children: ReactNode }, BoundaryState> {
  state: BoundaryState = { navigation: this.props.navigation, error: null }

  static getDerivedStateFromError(error: unknown): Partial<BoundaryState> {
    return { error: error instanceof Error ? error : new Error(String(error)) }
  }

  static getDerivedStateFromProps(props: { navigation: string }, state: BoundaryState): Partial<BoundaryState> | null {
    return props.navigation === state.navigation ? null : { navigation: props.navigation, error: null }
  }

  render() {
    return this.state.error ? <PageError error={this.state.error} /> : this.props.children
  }
}

const sessionStore = () => {
  try {
    return window.sessionStorage
  } catch {
    return null
  }
}

/** Seconds between checks whether the daemon serves the UI again. */
const RETRY_MS = 3_000

/**
 * A page module that could not be fetched reloads the tab as soon as the daemon serves the UI again (lib/reload.ts);
 * any other error is shown with a reload button.
 */
function PageError({ error }: { error: Error }) {
  const chunk = chunkFailed(error)
  const [state, setState] = useState<'checking' | 'offline' | 'failed'>('checking')

  useEffect(() => {
    if (!chunk) return
    let stopped = false
    let timer: ReturnType<typeof setTimeout> | undefined
    const check = async () => {
      // The page's own address is `index.html`; `no-store`, so the daemon answers and not the cache.
      const served = await fetch(window.location.pathname, { cache: 'no-store' }).then((r) => r.ok, () => false)
      if (stopped) return
      if (!served) {
        setState('offline')
        timer = setTimeout(check, RETRY_MS)
      } else if (mayReload(sessionStore(), Date.now())) {
        window.location.reload()
      } else {
        setState('failed')
      }
    }
    void check()
    return () => {
      stopped = true
      clearTimeout(timer)
    }
  }, [chunk])

  if (chunk && state === 'checking') return <Loading>正在重新加载页面…</Loading>
  const [tone, title, detail] = !chunk
    ? (['red', '这个页面出错了', `${error.message}。其他页面不受影响，可以重新加载后再试。`] as const)
    : state === 'offline'
      ? (['amber', '连不上 daemon，这个页面加载不了', 'daemon 可能正在重启；它恢复后页面会自动重新加载。'] as const)
      : ([
          'red',
          '这个页面的文件加载失败，重新加载后仍然如此',
          `${error.message}。界面文件可能不完整：用与 daemon 相同版本的源码运行 make ui，或重新运行 bash scripts/install.sh service --apply。`,
        ] as const)
  return (
    <div
      role="alert"
      className={cx(
        'space-y-3 rounded-lg border px-4 py-3 text-sm',
        tone === 'red' ? 'border-red-200 bg-red-50 text-red-800' : 'border-amber-200 bg-amber-50 text-amber-950',
      )}
    >
      <p className="flex items-center gap-2 font-medium">
        <Icon name="alert" className={cx('size-4 shrink-0', tone === 'red' ? 'text-red-500' : 'text-amber-600')} />
        {title}
      </p>
      <p className="break-words">{detail}</p>
      <Button variant="secondary" onClick={() => window.location.reload()}>
        <Icon name="refresh" className="size-4" />
        {state === 'offline' ? '立即重新加载' : '重新加载'}
      </Button>
    </div>
  )
}
