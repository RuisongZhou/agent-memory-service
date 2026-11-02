// Small styled primitives in the shadcn manner: plain elements with Tailwind classes, owned as source.
import { useEffect, useRef, useState, type ComponentProps, type ReactNode } from 'react'
import { Link } from 'react-router'
import type { Facet, MemScope } from '../api/types'
import { formatAgo, formatNumber, formatTime, labelled, projectName, scopeText } from '../lib/format'
import { Icon } from './icons'

export const cx = (...classes: (string | false | null | undefined)[]) => classes.filter(Boolean).join(' ')

/** The ring every focusable control shows when reached by keyboard. */
export const FOCUS = 'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-500/40 focus-visible:ring-offset-1'

const BUTTON = {
  primary: 'bg-brand-600 text-white shadow-sm hover:bg-brand-700',
  secondary: 'border border-zinc-300 bg-white text-zinc-800 shadow-xs hover:border-zinc-400 hover:bg-zinc-50',
  ghost: 'text-zinc-700 hover:bg-zinc-100 hover:text-zinc-900',
  danger: 'bg-red-600 text-white shadow-sm hover:bg-red-700',
  /** Opens a destructive action's confirmation; the solid `danger` is for the confirming button itself. */
  dangerOutline: 'border border-red-200 bg-white text-red-600 shadow-xs hover:border-red-300 hover:bg-red-50',
}

const BUTTON_SIZE = { sm: 'h-7 px-2.5 text-xs', md: 'h-8 px-3 text-sm' }

export function Button({
  variant = 'primary',
  size = 'md',
  className,
  ...props
}: ComponentProps<'button'> & { variant?: keyof typeof BUTTON; size?: keyof typeof BUTTON_SIZE }) {
  return (
    <button
      type="button"
      className={cx(
        'inline-flex shrink-0 items-center justify-center gap-1.5 rounded-md font-medium whitespace-nowrap transition-colors disabled:pointer-events-none disabled:opacity-50',
        FOCUS,
        BUTTON_SIZE[size],
        BUTTON[variant],
        className,
      )}
      {...props}
    />
  )
}

const FIELD = cx(
  'h-8 rounded-md border border-zinc-300 bg-white px-2.5 text-sm shadow-xs transition-colors placeholder:text-zinc-400',
  'hover:border-zinc-400 focus:border-brand-500 focus:ring-2 focus:ring-brand-500/20 focus:outline-none',
  'disabled:cursor-not-allowed disabled:bg-zinc-50 disabled:text-zinc-500',
)

export const Input = ({ className, ...props }: ComponentProps<'input'>) => (
  <input className={cx(FIELD, className)} {...props} />
)

export const Textarea = ({ className, ...props }: ComponentProps<'textarea'>) => (
  <textarea className={cx(FIELD, 'h-auto min-h-24 w-full py-1.5 leading-relaxed', className)} {...props} />
)

export const Select = ({ className, ...props }: ComponentProps<'select'>) => (
  <select className={cx(FIELD, 'cursor-pointer pr-8', className)} {...props} />
)

/** A text box with a search icon and a button that clears it. */
export function SearchInput({ value, onValueChange, className, ...props }: Omit<ComponentProps<'input'>, 'value'> & {
  value: string
  onValueChange: (value: string) => void
}) {
  return (
    <div className={cx('relative', className)}>
      <Icon name="search" className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-zinc-400" />
      <Input
        type="search"
        value={value}
        onChange={(e) => onValueChange(e.target.value)}
        onKeyDown={(e) => {
          // Esc clears the box before it would close anything else.
          if (e.key === 'Escape' && value) {
            e.preventDefault()
            onValueChange('')
          }
        }}
        className="w-full pr-8 pl-8 [&::-webkit-search-cancel-button]:hidden"
        {...props}
      />
      {value && (
        <button
          type="button"
          aria-label="清除"
          onClick={() => onValueChange('')}
          className={cx('absolute top-1/2 right-1.5 grid size-5 -translate-y-1/2 place-items-center rounded text-zinc-400 hover:bg-zinc-100 hover:text-zinc-700', FOCUS)}
        >
          <Icon name="x" className="size-3.5" />
        </button>
      )}
    </div>
  )
}

/**
 * A search box that applies itself while typing, after a short pause, or at once on Enter. `value` is the applied
 * text (from the URL); a change from outside, such as clearing every filter, replaces what is typed.
 */
export function LiveSearch({ value, onCommit, delay = 300, ...props }: Omit<ComponentProps<'input'>, 'value'> & {
  value: string
  onCommit: (value: string) => void
  delay?: number
}) {
  const [text, setText] = useState(value)
  const applied = useRef(value)
  const commit = useRef(onCommit)
  commit.current = onCommit

  useEffect(() => {
    if (value.trim() === applied.current.trim()) return
    applied.current = value
    setText(value)
  }, [value])

  const apply = (next: string) => {
    if (next.trim() === applied.current.trim()) return
    applied.current = next
    commit.current(next)
  }
  useEffect(() => {
    const timer = setTimeout(() => apply(text), delay)
    return () => clearTimeout(timer)
  }, [text, delay])

  return (
    <SearchInput
      value={text}
      onValueChange={(next) => {
        setText(next)
        // Clearing the box shows everything again without waiting.
        if (!next) apply('')
      }}
      onKeyDownCapture={(e) => {
        if (e.key === 'Enter') apply(text)
      }}
      {...props}
    />
  )
}

/** Facet values for a dropdown, keeping a value the URL names even when nothing has it any more. */
export function FacetOptions({ facets, selected, labels, show, counts = true }: {
  facets: Facet[]
  selected: string
  labels?: Record<string, string>
  /** How a value reads in the list, when its id is too long or plain for people (a project's path). */
  show?: (name: string) => string
  /** Off when the facet's counts are of something other than what the list shows. */
  counts?: boolean
}) {
  const listed = facets.some((f) => f.name === selected)
  const text = (name: string, count: number) =>
    `${labels ? labelled(labels, name) : show ? show(name) : name}${counts ? ` · ${formatNumber(count)}` : ''}`
  return (
    <>
      {facets.map((f) => (
        <option key={f.name} value={f.name}>{text(f.name, f.count)}</option>
      ))}
      {selected && !listed && <option value={selected}>{text(selected, 0)}</option>}
    </>
  )
}

/** A project id as its folder's name; the full path is the tooltip. A global session has no project. */
export const ProjectName = ({ id }: { id: string }) =>
  id ? <span title={id}>{projectName(id)}</span> : <span className="text-zinc-400">全局</span>

/** A memory's scope; a project's full path is the tooltip. */
export const ScopeText = ({ scope }: { scope: MemScope }) => (
  <span title={scope.kind === 'project' ? scope.project : undefined}>{scopeText(scope)}</span>
)

/** A time as how long ago it was, with the exact time as the tooltip. */
export const TimeAgo = ({ value }: { value: string | null | undefined }) =>
  value ? (
    <time dateTime={value} title={formatTime(value)} className="whitespace-nowrap">{formatAgo(value)}</time>
  ) : (
    <span className="text-zinc-400">—</span>
  )

export function Field({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <label className="flex min-w-0 flex-col gap-1.5 text-sm">
      <span className="font-medium text-zinc-700">{label}</span>
      {children}
      {hint && <span className="text-xs text-zinc-500">{hint}</span>}
    </label>
  )
}

export function Check({ label, className, ...props }: ComponentProps<'input'> & { label: ReactNode }) {
  return (
    <label className={cx('inline-flex cursor-pointer items-center gap-2 text-sm has-disabled:cursor-not-allowed', className)}>
      <input type="checkbox" className={cx('size-4 rounded accent-brand-600', FOCUS)} {...props} />
      {label}
    </label>
  )
}

/** A toggle in a row of filter choices: reads as a pill, works as a checkbox. */
export function Chip({ label, checked, className, ...props }: ComponentProps<'input'> & { label: ReactNode }) {
  return (
    <label
      className={cx(
        'inline-flex cursor-pointer items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs font-medium transition-colors select-none',
        'has-focus-visible:ring-2 has-focus-visible:ring-brand-500/40 has-disabled:cursor-not-allowed',
        checked
          ? 'border-brand-200 bg-brand-50 text-brand-700'
          : 'border-zinc-200 bg-white text-zinc-600 hover:border-zinc-300 hover:text-zinc-900',
        className,
      )}
    >
      <input type="checkbox" className="sr-only" checked={checked} {...props} />
      {checked && <Icon name="check" className="size-3.5" />}
      {label}
    </label>
  )
}

/** A choice between a few options that each need a line of explanation, drawn as cards side by side. */
export function RadioCards<T extends string>({ name, value, onChange, options }: {
  name: string
  value: T
  onChange: (value: T) => void
  options: readonly { value: T; title: ReactNode; description: ReactNode }[]
}) {
  return (
    <div role="radiogroup" className="grid gap-2 sm:grid-cols-2">
      {options.map((o) => {
        const on = o.value === value
        return (
          <label
            key={o.value}
            className={cx(
              'flex cursor-pointer gap-3 rounded-lg border p-3 transition-colors has-focus-visible:ring-2 has-focus-visible:ring-brand-500/40',
              on ? 'border-brand-500 bg-brand-50/50 ring-1 ring-brand-500' : 'border-zinc-200 hover:border-zinc-300',
            )}
          >
            <input
              type="radio"
              name={name}
              checked={on}
              onChange={() => onChange(o.value)}
              className="mt-0.5 size-4 shrink-0 accent-brand-600"
            />
            <span className="text-sm">
              <span className="block font-medium text-zinc-900">{o.title}</span>
              <span className="mt-0.5 block text-zinc-500">{o.description}</span>
            </span>
          </label>
        )
      })}
    </div>
  )
}

export function Card({ title, description, actions, children, className }: {
  title?: ReactNode
  /** A line under the title on what the card shows. */
  description?: ReactNode
  actions?: ReactNode
  children: ReactNode
  className?: string
}) {
  return (
    <section className={cx('rounded-xl border border-zinc-200 bg-white p-4 shadow-xs sm:p-5', className)}>
      {(title || actions) && (
        <header className="mb-4 flex flex-wrap items-start justify-between gap-x-4 gap-y-2">
          <div className="min-w-0">
            <h2 className="font-semibold text-zinc-900">{title}</h2>
            {description && <p className="mt-0.5 text-sm text-zinc-500">{description}</p>}
          </div>
          {actions && <div className="flex shrink-0 items-center gap-2 text-sm">{actions}</div>}
        </header>
      )}
      {children}
    </section>
  )
}

const TONE = {
  gray: 'bg-zinc-100 text-zinc-700 ring-zinc-500/15',
  blue: 'bg-sky-50 text-sky-700 ring-sky-600/20',
  amber: 'bg-amber-50 text-amber-800 ring-amber-600/25',
  green: 'bg-emerald-50 text-emerald-700 ring-emerald-600/20',
  red: 'bg-red-50 text-red-700 ring-red-600/20',
}
export type Tone = keyof typeof TONE

export const Badge = ({ tone = 'gray', title, children }: { tone?: Tone; title?: string; children: ReactNode }) => (
  <span
    title={title}
    className={cx('inline-flex items-center rounded-md px-1.5 py-0.5 text-xs font-medium whitespace-nowrap ring-1 ring-inset', TONE[tone])}
  >
    {children}
  </span>
)

/** Marks an action that calls a model (and may cost money) before it is submitted (design §4.4). */
export const ModelCall = ({ children }: { children: ReactNode }) => (
  <span className="inline-flex items-center gap-1 rounded-md bg-amber-50 px-1.5 py-0.5 text-xs text-amber-800 ring-1 ring-amber-600/25 ring-inset">
    <Icon name="sparkles" className="size-3.5 shrink-0" />
    {children}
  </span>
)

export const MemoryLink = ({ id }: { id: string }) => (
  <Link to={`/memories/${id}`} className="font-mono text-sm text-brand-700 hover:underline">
    {id}
  </Link>
)

/** A two-column list of labelled values. */
export function Fields({ rows }: { rows: [string, ReactNode][] }) {
  return (
    <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">
      {rows.map(([label, value]) => (
        <div key={label} className="contents">
          <dt className="text-zinc-500">{label}</dt>
          <dd className="min-w-0 break-words text-zinc-900">{value}</dd>
        </div>
      ))}
    </dl>
  )
}

export const Muted = ({ children }: { children: ReactNode }) => <p className="text-sm text-zinc-500">{children}</p>

export const Spinner = ({ className }: { className?: string }) => (
  <Icon name="spinner" className={cx('size-4 animate-spin text-zinc-400', className)} />
)

/** What a card or page shows while its first read is in flight. */
export const Loading = ({ children = '加载中…' }: { children?: ReactNode }) => (
  <div role="status" className="flex items-center gap-2 py-6 text-sm text-zinc-500">
    <Spinner />
    {children}
  </div>
)

/** A list or result with nothing in it. */
export const Empty = ({ children, action }: { children: ReactNode; action?: ReactNode }) => (
  <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-zinc-200 px-4 py-10 text-center text-sm text-zinc-500">
    <Icon name="inbox" className="size-8 text-zinc-300" />
    <div>{children}</div>
    {action}
  </div>
)

export const Pre = ({ children }: { children: ReactNode }) => (
  <pre className="overflow-x-auto rounded-lg border border-zinc-200 bg-zinc-50 p-3 font-mono text-xs leading-relaxed whitespace-pre-wrap break-words text-zinc-800">
    {children}
  </pre>
)

/** Lists refresh only when asked (design §4.4: nothing auto-refreshes). */
export const RefreshButton = ({ onClick, busy }: { onClick: () => void; busy: boolean }) => (
  <Button variant="secondary" disabled={busy} onClick={onClick}>
    <Icon name="refresh" className={cx('size-4', busy && 'animate-spin')} />
    {busy ? '刷新中…' : '刷新'}
  </Button>
)

/** A notice that explains why an action is unavailable or what it will do. */
export const Note = ({ tone = 'amber', children }: { tone?: 'amber' | 'gray'; children: ReactNode }) => (
  <div
    className={cx(
      'flex gap-2 rounded-lg border px-3 py-2.5 text-sm',
      tone === 'amber' ? 'border-amber-200 bg-amber-50 text-amber-950' : 'border-zinc-200 bg-zinc-50 text-zinc-700',
    )}
  >
    <Icon name="info" className={cx('mt-0.5 size-4 shrink-0', tone === 'amber' ? 'text-amber-600' : 'text-zinc-400')} />
    <div className="min-w-0">{children}</div>
  </div>
)

/** A row of mutually exclusive views, such as the two lists of the tasks page. */
export function Tabs<T extends string>({ value, onChange, tabs }: {
  value: T
  onChange: (value: T) => void
  tabs: readonly (readonly [T, ReactNode])[]
}) {
  return (
    <div role="tablist" className="inline-flex gap-1 rounded-lg bg-zinc-200/60 p-1">
      {tabs.map(([key, label]) => (
        <button
          key={key}
          type="button"
          role="tab"
          aria-selected={value === key}
          onClick={() => onChange(key)}
          className={cx(
            'rounded-md px-3 py-1 text-sm font-medium transition-colors',
            FOCUS,
            value === key ? 'bg-white text-zinc-900 shadow-sm' : 'text-zinc-600 hover:text-zinc-900',
          )}
        >
          {label}
        </button>
      ))}
    </div>
  )
}

/** A table's header row and cell padding, shared by the list pages. */
export const TH = ({ children, className }: { children?: ReactNode; className?: string }) => (
  <th
    className={cx(
      'border-b border-zinc-200 bg-zinc-50/80 px-3 py-2 text-left text-xs font-medium whitespace-nowrap text-zinc-500 first:rounded-tl-lg last:rounded-tr-lg',
      className,
    )}
  >
    {children}
  </th>
)

export const TD = ({ children, className }: { children?: ReactNode; className?: string }) => (
  <td className={cx('px-3 py-2 align-top', className)}>{children}</td>
)
