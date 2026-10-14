#!/usr/bin/env node
// Offline comparison adapter for the actual npm mem0ai@3.3.1 package.
// The supplied embedding endpoint and generation double are local only.
const fs = require('node:fs');
const path = require('node:path');
const { createRequire } = require('node:module');

process.env.MEM0_TELEMETRY = 'false';
process.env.OPENAI_API_KEY = 'offline';
const [sdkRoot, protocolPath, outputPath, embedUrl] = process.argv.slice(2);
if (!/^http:\/\/127\.0\.0\.1:\d+\/v1$/.test(embedUrl || '')) throw Error('local embeddings required');
const load = createRequire(path.resolve(sdkRoot, 'package.json'));
const sdkPackage = JSON.parse(fs.readFileSync(path.join(path.dirname(load.resolve('mem0ai')), '..', 'package.json')));
if (sdkPackage.version !== '3.3.1') throw Error('requires mem0ai 3.3.1');
for (const optionalNlp of ['natural', 'compromise']) {
  let installed = false;
  try { load.resolve(optionalNlp); installed = true; } catch {}
  if (installed) throw Error(`comparison requires built-in fallback; ${optionalNlp} is installed`);
}
const { Memory } = load('mem0ai/oss');
const protocol = JSON.parse(fs.readFileSync(protocolPath, 'utf8'));
fs.mkdirSync(outputPath, { recursive: true });

function section(prompt, title) {
  return prompt.split(`## ${title}\n`)[1].split('\n\n## ')[0];
}
function extract(prompt) {
  const current = section(prompt, 'New Messages');
  const history = section(prompt, 'Last k Messages');
  let texts = [];
  if (current.includes('FACTS_JSON:')) texts = JSON.parse(current.split('FACTS_JSON:')[1]);
  else {
    if (current.includes('TAIL_FACT_完整尾部')) texts.push('TAIL_FACT_完整尾部');
    for (const name of ['SESSION', 'RAW']) {
      if (current.includes(`PROBE_${name}`) && history.includes(`POISON_${name}`)) texts.push(`CONTAMINATED_${name}`);
    }
    if (current.includes('PROBE_REFERENCE') && history.includes('REFERENCE_BGE_M3')) texts.push('Resolved reference: BGE-M3');
  }
  return { memory: texts.map((text, id) => ({ id: String(id), text, attributed_to: 'user', linked_memory_ids: [] })) };
}
async function main() {
  const results = [];
  for (const test of protocol.cases) {
    const dir = path.resolve(outputPath, test.id);
    fs.mkdirSync(dir, { recursive: true });
    const mem = new Memory({
      embedder: { provider: 'openai', config: { apiKey: 'offline', model: 'shared-offline', embeddingDims: protocol.dimension, url: embedUrl, baseURL: embedUrl } },
      llm: { provider: 'openai', config: { apiKey: 'offline', model: 'offline', baseURL: 'http://127.0.0.1:1/v1' } },
      vectorStore: { provider: 'memory', config: { dimension: protocol.dimension, collectionName: 'memories', dbPath: path.join(dir, 'vectors.db') } },
      historyDbPath: path.join(dir, 'history.db'),
    });
    // Replace only model calls. All SDK add/search/scoring/storage code executes.
    const embeddingCalls = [];
    const embed = async texts => {
      const response = await fetch(`${embedUrl}/embeddings`, {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ input: texts, model: 'shared-offline' }),
      });
      if (!response.ok) throw Error(`embedding HTTP ${response.status}`);
      const data = await response.json();
      embeddingCalls.push(texts);
      return data.data.map(row => row.embedding);
    };
    mem.embedder.embed = async text => (await embed([text]))[0];
    mem.embedder.embedBatch = embed;
    const traces = [];
    mem.llm.generateResponse = async messages => {
      const response = extract(messages.at(-1).content);
      traces.push({ messages, response });
      return JSON.stringify(response);
    };
    // Disable UI/telemetry notices; they are not memory operations.
    mem._captureEvent = async () => {};
    mem._displayFirstRunNotice = async () => {};
    mem._displayTemporalUsageNotice = async () => {};
    mem._displayScaleThresholdNotice = async () => {};
    mem._displayPerformanceSlowQueryNotice = async () => {};
    const receipts = [], inputs = [], searches = [];
    let blankRejected = false;
    try {
      for (const [session, messages, infer] of test.steps) {
        receipts.push(await mem.add(messages.map(({ role, content }) => ({ role, content })), {
          userId: 'p', runId: session, infer,
        }));
        inputs.push(...await mem.db.getLastMessages(mem.buildSessionScope({ user_id: 'p', run_id: session }), 10));
      }
      try { await mem.add([{ role: 'user', content: ' \n\t' }], { userId: 'p', runId: 'blank' }); }
      catch (error) { blankRejected = /blank/.test(error.message); }
      for (const query of test.queries || []) {
        searches.push({ query, output: await mem.search(query, { filters: { user_id: 'p', run_id: 's' }, topK: 10, threshold: 0.1, explain: true }) });
      }
      const memories = await mem.getAll({ filters: { user_id: 'p' }, limit: 100 });
      const entities = mem._entityStore ? await mem._entityStore.list({ user_id: 'p' }, 1000) : [];
      results.push({ id: test.id, traces, receipts, inputs, searches, memories, entities, embeddingCalls, blankRejected });
      fs.writeFileSync(path.join(outputPath, `${test.id}.json`), JSON.stringify(results.at(-1), null, 2));
    } finally {
      // Close actual SQLite connections without relying on SDK-private cleanup APIs.
      for (const resource of [mem.db, mem.vectorStore, mem._entityStore]) {
        if (resource?.db?.open) resource.db.close();
      }
    }
  }
  fs.writeFileSync(path.join(outputPath, 'results.json'), JSON.stringify(results, null, 2));
  console.log(JSON.stringify({ cases: results.length, sdkVersion: '3.3.1', realModelCalls: 0 }));
}
main().catch(error => { console.error(error); process.exitCode = 1; });
