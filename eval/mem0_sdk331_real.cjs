#!/usr/bin/env node
// Actual SDK 3.3.1 add/search over local JSON-lines control; generation is real.
const fs = require('node:fs'), path = require('node:path'), readline = require('node:readline');
const { createRequire } = require('node:module');
const [sdkRoot, protocolPath, storeRoot, embedUrl, llmUrl] = process.argv.slice(2);
process.env.MEM0_TELEMETRY = 'false';
const load = createRequire(path.resolve(sdkRoot, 'package.json'));
const pkg = JSON.parse(fs.readFileSync(path.join(path.dirname(load.resolve('mem0ai')), '..', 'package.json')));
if (pkg.version !== '3.3.1') throw Error('SDK 3.3.1 required');
for (const name of ['natural', 'compromise']) {
  let present = false; try { load.resolve(name); present = true; } catch {}
  if (present) throw Error(`unexpected optional NLP dependency ${name}`);
}
const { Memory } = load('mem0ai/oss');
const protocol = JSON.parse(fs.readFileSync(protocolPath));
const stores = new Map();
const log = console.log; console.log = (...args) => console.error(...args);

async function get(speaker) {
  if (stores.has(speaker)) return stores.get(speaker);
  const dir = path.resolve(storeRoot, speaker); fs.mkdirSync(dir, { recursive: true });
  const mem = new Memory({
    embedder: { provider: 'openai', config: { apiKey: 'local', model: 'bge-m3', embeddingDims: 1024, baseURL: embedUrl } },
    llm: { provider: 'openai', config: { apiKey: 'local-gateway', model: protocol.model, baseURL: llmUrl } },
    vectorStore: { provider: 'memory', config: { dimension: 1024, collectionName: 'memories', dbPath: path.join(dir, 'vectors.db') } },
    historyDbPath: path.join(dir, 'history.db'), customInstructions: protocol.customInstructions,
  });
  const embed = async texts => {
    if (!texts.length) return [];
    const r = await fetch(`${embedUrl}/embeddings`, { method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ input: texts, model: 'bge-m3' }) });
    if (!r.ok) throw Error(`embedding status ${r.status}`);
    return (await r.json()).data.map(row => row.embedding);
  };
  // The same real BGE-M3 adapter and input truncation as the service, not a model double.
  mem.embedder.embed = async text => (await embed([text]))[0];
  mem.embedder.embedBatch = embed;
  for (const method of ['_captureEvent', '_displayFirstRunNotice', '_displayTemporalUsageNotice',
                        '_displayScaleThresholdNotice', '_displayPerformanceSlowQueryNotice']) mem[method] = async () => {};
  // Endpoint gateway pins temperature/output cap/thinking and the historical observation date.
  await mem._ensureInitialized();
  stores.set(speaker, mem); return mem;
}

async function handle(command) {
  const mem = await get(command.speaker);
  const start = performance.now(); let result;
  if (command.op === 'add') result = await mem.add(command.messages, { userId: command.speaker, runId: command.session });
  else if (command.op === 'search') result = await mem.search(command.query, { filters: { user_id: command.speaker }, topK: 30, threshold: .1, rerank: false, explain: true });
  else if (command.op === 'export') {
    const memories = await mem.getAll({ filters: { user_id: command.speaker }, topK: 10000 });
    const [all] = await mem.vectorStore.list({ user_id: command.speaker }, 10000);
    if (all.length >= 10000 || all.length !== memories.results.length) throw Error('incomplete corpus export');
    result = { memories,
      entities: mem._entityStore ? await mem._entityStore.list({ user_id: command.speaker }, 10000) : [] };
  } else throw Error(`unknown op ${command.op}`);
  return { result, coreMs: performance.now() - start };
}

async function main() {
  for await (const line of readline.createInterface({ input: process.stdin })) {
    try { process.stdout.write(JSON.stringify({ ok: true, ...await handle(JSON.parse(line)) }) + '\n'); }
    catch (error) { process.stdout.write(JSON.stringify({ ok: false, error: String(error) }) + '\n'); }
  }
  for (const mem of stores.values()) for (const owner of [mem.db, mem.vectorStore, mem._entityStore]) if (owner?.db?.open) owner.db.close();
}
main().catch(error => { console.error(error); process.exitCode = 1; });
