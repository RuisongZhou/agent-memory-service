// Small styled primitives in the shadcn manner: plain elements with Tailwind classes, owned as source.
import type { ComponentProps, ReactNode } from 'react'
import { Link } from 'react-router'

const cx = (...classes: (string | false | undefined)[]) => classes.filter(Boolean).join(' ')

const BUTTON = {
  primary: 'bg-zinc-900 text-white hover:bg-zinc-700',
  secondary: 'border border-zinc-300 bg-white text-zinc-800 hover:bg-zinc-50',
  danger: 'bg-red-600 text-white hover:bg-red-500',
}

export function Button({
  variant = 'primary',
  className,
  ...props
}: ComponentProps<'button'> & { variant?: keyof typeof BUTTON }) {
  return (
    <button
      type="button"
      className={cx(
        'inline-flex items-center justify-center gap-1 rounded-md px-3 py-1.5 text-sm font-medium transition-colors disabled:pointer-events-none disabled:opacity-50',
        BUTTON[variant],
        className,
      )}
      {...props}
    />
  )
}

const FIELD = 'rounded-md border border-zinc-300 bg-white px-2.5 py-1.5 text-sm focus:border-zinc-500 focus:outline-none'

export const Input = ({ className, ...props }: ComponentProps<'input'>) => (
  <input className={cx(FIELD, className)} {...props} />
)

export const Textarea = ({ className, ...props }: ComponentProps<'textarea'>) => (
  <textarea className={cx(FIELD, 'min-h-24 w-full', className)} {...props} />
)

export const Select = ({ className, ...props }: ComponentProps<'select'>) => (
  <select className={cx(FIELD, 'pr-7', className)} {...props} />
)

export function Field({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <label className="flex flex-col gap-1 text-sm">
      <span className="font-medium text-zinc-700">{label}</span>
      {children}
      {hint && <span className="text-xs text-zinc-500">{hint}</span>}
    </label>
  )
}

export function Check({ label, className, ...props }: ComponentProps<'input'> & { label: ReactNode }) {
  return (
    <label className={cx('inline-flex items-center gap-1.5 text-sm', className)}>
      <input type="checkbox" className="size-4 accent-zinc-800" {...props} />
      {label}
    </label>
  )
}

export function Card({ title, actions, children, className }: {
  title?: ReactNode
  actions?: ReactNode
  children: ReactNode
  className?: string
}) {
  return (
    <section className={cx('rounded-lg border border-zinc-200 bg-white p-4', className)}>
      {(title || actions) && (
        <header className="mb-3 flex items-center justify-between gap-2">
          <h2 className="font-semibold text-zinc-900">{title}</h2>
          {actions}
        </header>
      )}
      {children}
    </section>
  )
}

const TONE = {
  gray: 'bg-zinc-100 text-zinc-700',
  blue: 'bg-sky-100 text-sky-800',
  amber: 'bg-amber-100 text-amber-900',
  green: 'bg-emerald-100 text-emerald-800',
  red: 'bg-red-100 text-red-800',
}
export type Tone = keyof typeof TONE

export const Badge = ({ tone = 'gray', title, children }: { tone?: Tone; title?: string; children: ReactNode }) => (
  <span title={title} className={cx('inline-block rounded px-1.5 py-0.5 text-xs font-medium whitespace-nowrap', TONE[tone])}>
    {children}
  </span>
)

/** Marks an action that calls a model (and may cost money) before it is submitted (design §4.4). */
export const ModelCall = ({ children }: { children: ReactNode }) => (
  <span className="rounded border border-amber-300 bg-amber-50 px-1.5 py-0.5 text-xs text-amber-900">{children}</span>
)

export const MemoryLink = ({ id }: { id: string }) => (
  <Link to={`/memories/${id}`} className="font-mono text-sm text-sky-700 hover:underline">
    {id}
  </Link>
)

/** A two-column list of labelled values. */
export function Fields({ rows }: { rows: [string, ReactNode][] }) {
  return (
    <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1.5 text-sm">
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

export const Pre = ({ children }: { children: ReactNode }) => (
  <pre className="overflow-x-auto rounded-md bg-zinc-50 p-3 text-xs whitespace-pre-wrap break-words text-zinc-800">
    {children}
  </pre>
)
