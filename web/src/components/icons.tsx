// Line icons drawn for this UI on a 24-unit grid; they inherit the text colour.

const PATHS = {
  home: 'M3 10.5 12 3l9 7.5M5 9v11h5v-6h4v6h5V9',
  list: 'M8 6h13M8 12h13M8 18h13M3.5 6h.01M3.5 12h.01M3.5 18h.01',
  tag: 'M3 3h8l10 10-8 8L3 11V3Zm4.5 4.5h.01',
  plus: 'M12 5v14M5 12h14',
  search: 'M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14Zm9 16-4-4',
  layers: 'm12 3 9 5-9 5-9-5 9-5Zm-9 9 9 5 9-5M3 16l9 5 9-5',
  plug: 'M9 3v5M15 3v5M6 8h12v3a6 6 0 0 1-12 0V8Zm6 9v4',
  download: 'M12 3v12m0 0-4.5-4.5M12 15l4.5-4.5M4 17v3h16v-3',
  merge: 'M6 3v6a6 6 0 0 0 6 6h0a6 6 0 0 1 6 6M18 3v6a6 6 0 0 1-6 6',
  alert: 'M12 3 2 20h20L12 3Zm0 6v5m0 3h.01',
  history: 'M3 12a9 9 0 1 0 3-6.7L3 8m0-5v5h5m4-1v5l3 2',
  tasks: 'M9 6h11M9 12h11M9 18h11M4 5l1 1 2-2M4 11l1 1 2-2M4 17l1 1 2-2',
  menu: 'M4 6h16M4 12h16M4 18h16',
  x: 'M6 6l12 12M18 6 6 18',
  check: 'm5 12.5 4.5 4.5L19 7',
  refresh: 'M20 11a8 8 0 0 0-14.3-4.9L4 8m0-5v5h5M4 13a8 8 0 0 0 14.3 4.9L20 16m0 5v-5h-5',
  info: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18Zm0 8v5m0-8.5h.01',
  inbox: 'M3 13h5l1.5 3h5l1.5-3h5M5.5 5h13L21 13v6H3v-6l2.5-8Z',
  sparkles: 'M12 3v4M12 17v4M3 12h4M17 12h4M6 6l2.5 2.5M15.5 15.5 18 18M18 6l-2.5 2.5M8.5 15.5 6 18',
  arrowRight: 'M5 12h14m-6-6 6 6-6 6',
  chevronLeft: 'm15 6-6 6 6 6',
  spinner: 'M12 3a9 9 0 1 0 9 9',
  filter: 'M3 5h18l-7 8v6l-4 2v-8L3 5Z',
  folder: 'M3 6.5A1.5 1.5 0 0 1 4.5 5H9l2 2.5h8.5A1.5 1.5 0 0 1 21 9v9.5a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5v-12Z',
  file: 'M6 3h8l4 4v14H6V3Zm8 0v4h4M9 12h6M9 16h6',
  message: 'M4 5h16v11H9l-5 4V5Zm4 5h8M8 13h5',
  tree: 'M5 4v16M5 8h5M5 15h5M12 5h7v6h-7zM12 12h7v6h-7z',
} as const

export type IconName = keyof typeof PATHS

export const Icon = ({ name, className }: { name: IconName; className?: string }) => (
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth={1.75}
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
    className={className ?? 'size-4'}
  >
    <path d={PATHS[name]} />
  </svg>
)
