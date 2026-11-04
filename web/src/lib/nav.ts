import type { IconName } from '../components/icons'

export interface NavItem {
  to: string
  label: string
  icon: IconName
}

/**
 * The sidebar: records and their editing, then the read-only debugging views, then reviewing and tracing what agents
 * and the daemon did, and connecting the agents themselves.
 */
export const NAV: { title: string; items: NavItem[] }[] = [
  {
    title: '记忆',
    items: [
      { to: '/', label: '概览', icon: 'home' },
      { to: '/memories', label: '记忆列表', icon: 'list' },
      { to: '/tree', label: '记忆树', icon: 'tree' },
      { to: '/entities', label: '实体', icon: 'tag' },
      { to: '/new', label: '新增记忆', icon: 'plus' },
    ],
  },
  {
    title: '调试',
    items: [
      { to: '/search', label: '检索调试', icon: 'search' },
      { to: '/context', label: '上下文预览', icon: 'layers' },
    ],
  },
  {
    title: '审核与追溯',
    items: [
      { to: '/conflicts', label: '冲突', icon: 'alert' },
      { to: '/imports', label: '导入审核', icon: 'download' },
      { to: '/consolidation', label: 'Consolidation', icon: 'merge' },
      { to: '/recalls', label: '召回记录', icon: 'history' },
      { to: '/tasks', label: '任务', icon: 'tasks' },
    ],
  },
  {
    title: '接入',
    items: [{ to: '/platforms', label: '接入平台', icon: 'plug' }],
  },
]

export interface PageMeta {
  title: string
  description?: string
  /** A detail page's list, linked as the way back. */
  parent?: { to: string; label: string }
}

const PAGES: Record<string, PageMeta> = {
  '/': { title: '概览', description: '服务状态、待处理事项和记忆的整体分布' },
  '/memories': { title: '记忆列表', description: '按项目、平台、类型和状态筛选记忆' },
  '/tree': { title: '记忆树', description: '按项目、类型和会话浏览记忆，从摘要一路读到原始对话' },
  '/entities': { title: '实体', description: '从记忆中抽取的人、工具和概念' },
  '/new': { title: '新增记忆', description: '手动写入一条事实或指令' },
  '/search': { title: '检索调试', description: '查看一次检索的排序、分数和耗时' },
  '/context': { title: '上下文预览', description: '会话开始时 agent 会拿到的上下文' },
  '/platforms': { title: '接入平台', description: '把本机的 agent 平台接到共享记忆' },
  '/imports': { title: '导入审核', description: '逐条审核导入的记忆，应用或回滚' },
  '/consolidation': { title: 'Consolidation', description: '合并重复记忆、找出冲突，应用前可逐条查看' },
  '/conflicts': { title: '冲突', description: '两条记忆说法不一致时，选择保留哪一条' },
  '/recalls': { title: '召回记录', description: '哪个平台、哪个会话在什么时候取回了哪些记忆' },
  '/tasks': { title: '任务', description: '写入请求和模型调用的状态与失败原因' },
}

const DETAILS: [prefix: string, title: string][] = [
  ['/memories/', '记忆详情'],
  ['/entities/', '实体详情'],
  ['/imports/', '导入运行'],
  ['/consolidation/', 'Consolidation 运行'],
  ['/tasks/', '写入请求'],
]

/** The header of the page at `pathname`; a detail page links back to its list. Unknown paths have none. */
export function pageMeta(pathname: string): PageMeta | undefined {
  if (PAGES[pathname]) return PAGES[pathname]
  for (const [prefix, title] of DETAILS) {
    if (pathname.startsWith(prefix) && pathname.length > prefix.length) {
      const list = prefix.slice(0, -1)
      return { title, parent: { to: list, label: PAGES[list].title } }
    }
  }
  return undefined
}
