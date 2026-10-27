import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { lazy, StrictMode, type ComponentType } from 'react'
import { createRoot } from 'react-dom/client'
import { HashRouter, Route, Routes } from 'react-router'
import { Layout } from './components/Layout'
import './index.css'
import { OverviewPage } from './pages/Overview'

/** A page module loaded when its route is first opened, so the first load carries only the overview. */
const page = <K extends string>(load: () => Promise<Record<K, ComponentType>>, name: K) =>
  lazy(() => load().then((m) => ({ default: m[name] })))

const ConflictsPage = page(() => import('./pages/Conflicts'), 'ConflictsPage')
const ConsolidationPage = page(() => import('./pages/Consolidation'), 'ConsolidationPage')
const ConsolidationRunPage = page(() => import('./pages/Consolidation'), 'ConsolidationRunPage')
const ContextPage = page(() => import('./pages/Context'), 'ContextPage')
const EntitiesPage = page(() => import('./pages/Entities'), 'EntitiesPage')
const EntityPage = page(() => import('./pages/Entities'), 'EntityPage')
const ImportRunPage = page(() => import('./pages/Imports'), 'ImportRunPage')
const ImportsPage = page(() => import('./pages/Imports'), 'ImportsPage')
const MemoriesPage = page(() => import('./pages/Memories'), 'MemoriesPage')
const MemoryPage = page(() => import('./pages/MemoryDetail'), 'MemoryPage')
const NewMemoryPage = page(() => import('./pages/NewMemory'), 'NewMemoryPage')
const RecallsPage = page(() => import('./pages/Recalls'), 'RecallsPage')
const RequestPage = page(() => import('./pages/Tasks'), 'RequestPage')
const SearchPage = page(() => import('./pages/Search'), 'SearchPage')
const TasksPage = page(() => import('./pages/Tasks'), 'TasksPage')

// Reads refresh only when asked. Calls that reach a model (search, context, writes) run only on submit.
const queryClient = new QueryClient({
  defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false, staleTime: 10_000 } },
})

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <HashRouter>
        <Routes>
          <Route element={<Layout />}>
            <Route index element={<OverviewPage />} />
            <Route path="memories" element={<MemoriesPage />} />
            <Route path="memories/:id" element={<MemoryPage />} />
            <Route path="entities" element={<EntitiesPage />} />
            <Route path="entities/:id" element={<EntityPage />} />
            <Route path="new" element={<NewMemoryPage />} />
            <Route path="search" element={<SearchPage />} />
            <Route path="context" element={<ContextPage />} />
            <Route path="imports" element={<ImportsPage />} />
            <Route path="imports/:runId" element={<ImportRunPage />} />
            <Route path="consolidation" element={<ConsolidationPage />} />
            <Route path="consolidation/:runId" element={<ConsolidationRunPage />} />
            <Route path="conflicts" element={<ConflictsPage />} />
            <Route path="recalls" element={<RecallsPage />} />
            <Route path="tasks" element={<TasksPage />} />
            <Route path="tasks/:requestId" element={<RequestPage />} />
            <Route path="*" element={<p className="text-sm text-zinc-500">没有这个页面。</p>} />
          </Route>
        </Routes>
      </HashRouter>
    </QueryClientProvider>
  </StrictMode>,
)
