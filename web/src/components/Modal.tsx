import * as Dialog from '@radix-ui/react-dialog'
import type { ReactNode } from 'react'
import { Icon } from './icons'
import { cx, FOCUS } from './ui'

export function Modal({ open, onOpenChange, title, description, children }: {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  description?: ReactNode
  children: ReactNode
}) {
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 animate-fade-in bg-zinc-900/40 backdrop-blur-[2px]" />
        <Dialog.Content className="fixed top-1/2 left-1/2 z-50 max-h-[90vh] w-[min(40rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 animate-dialog-in overflow-y-auto rounded-xl bg-white p-6 shadow-2xl ring-1 ring-zinc-900/5">
          <Dialog.Title className="pr-8 text-lg font-semibold text-zinc-900">{title}</Dialog.Title>
          <Dialog.Description className="mt-1 mb-5 text-sm text-zinc-600">{description}</Dialog.Description>
          <Dialog.Close
            aria-label="关闭"
            className={cx('absolute top-4 right-4 grid size-8 place-items-center rounded-md text-zinc-400 hover:bg-zinc-100 hover:text-zinc-700', FOCUS)}
          >
            <Icon name="x" className="size-4" />
          </Dialog.Close>
          {children}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  )
}
