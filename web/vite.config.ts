import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig, type Plugin, type ProxyOptions } from 'vite'
import pkg from './package.json' with { type: 'json' }

// The UI and the daemon share the workspace version; `/health` reports the daemon's and the page compares the
// two on load (docs/design/web-ui.md §7.1).
function versionFile(): Plugin {
  const source = JSON.stringify({ version: pkg.version })
  return {
    name: 'version-json',
    configureServer(server) {
      server.middlewares.use('/ui/version.json', (_req, res) => {
        res.setHeader('Content-Type', 'application/json')
        res.end(source)
      })
    },
    generateBundle() {
      this.emitFile({ type: 'asset', fileName: 'version.json', source })
    },
  }
}

// `vite dev` forwards API calls to a daemon. The daemon accepts only its own Host and no foreign Origin, so the
// proxy rewrites Host and drops Origin.
const daemon: ProxyOptions = {
  target: process.env.MEMORY_URL ?? 'http://127.0.0.1:8765',
  changeOrigin: true,
  configure: (proxy) => proxy.on('proxyReq', (req) => req.removeHeader('origin')),
}

export default defineConfig({
  base: '/ui/',
  plugins: [react(), tailwindcss(), versionFile()],
  // A local tool served from the daemon: one chunk of about 150 kB gzipped is fine, so the 500 kB advice is noise.
  build: { chunkSizeWarningLimit: 650 },
  server: { proxy: { '/v1': daemon, '/admin': daemon, '/health': daemon } },
})
