import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { HashRouter, Route, Routes } from 'react-router'
import { Layout } from './components/Layout'
import './index.css'
import { ConflictsPage } from './pages/Conflicts'
import { ConsolidationPage, ConsolidationRunPage } from './pages/Consolidation'
import { ContextPage } from './pages/Context'
import { ImportRunPage, ImportsPage } from './pages/Imports'
import { MemoriesPage } from './pages/Memories'
import { MemoryPage } from './pages/MemoryDetail'
import { NewMemoryPage } from './pages/NewMemory'
import { OverviewPage } from './pages/Overview'
import { RecallsPage } from './pages/Recalls'
import { SearchPage } from './pages/Search'
import { RequestPage, TasksPage } from './pages/Tasks'

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
