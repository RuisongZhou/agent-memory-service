import { describe, expect, it } from 'vitest'
import type { ModelSettings } from '../api/types'
import { changed, formOf, keysNeeded, mayReembed, origin, proposal } from './settings'

const text = (value: string, effective = value, source: 'file' | 'default' | 'flag' = value ? 'file' : 'default') => ({
  value,
  effective,
  source,
})

const settings = (): ModelSettings => ({
  editable: true,
  envFile: '/home/me/.config/agent-memory-service/.env',
  mock: false,
  rerank: true,
  fields: {
    llmBaseUrl: text('https://maas.qianwenaiapi.com/compatible-mode/v1'),
    llmModel: text('', 'qwen3.8-flash'),
    modelProvider: text('api'),
    embedUrl: text('', 'https://maas.qianwenaiapi.com/compatible-mode/v1'),
    rerankUrl: text('', 'https://maas.qianwenaiapi.com/compatible-api/v1'),
    embedModel: text('qwen3.7-text-embedding-flash'),
    rerankModel: text('qwen3.7-text-rerank'),
    llmKey: { set: true, source: 'file' },
    modelKey: { set: false, source: 'default' },
  },
  models: { backend: 'api', embedModel: 'qwen3.7-text-embedding-flash', rerankModel: 'qwen3.7-text-rerank', dim: 1024 },
  rolledBack: null,
})

describe('model settings form', () => {
  it('starts from the settings as they are and sends a key only when one is entered', () => {
    const s = settings()
    const form = formOf(s)
    expect(form.llmModel).toBe('qwen3.8-flash')
    expect(form.embedUrl).toBe('')
    expect(changed(form, s)).toEqual([])
    const req = proposal({ ...form, llmModel: ' qwen-max ', llmKey: '  ' })
    expect(req.llmModel).toBe('qwen-max')
    expect('llmKey' in req).toBe(false)
    expect(proposal({ ...form, modelKey: 'sk-new' }).modelKey).toBe('sk-new')
  })

  it('asks for a key again when its endpoint moves to another host', () => {
    const s = settings()
    const form = formOf(s)
    expect(keysNeeded({ ...form, llmBaseUrl: 'https://maas.qianwenaiapi.com/other/v1' }, s)).toEqual([])
    expect(keysNeeded({ ...form, llmBaseUrl: 'https://api.example.com/v1' }, s)).toEqual(['llmKey'])
    expect(keysNeeded({ ...form, llmBaseUrl: 'https://api.example.com/v1', llmKey: 'sk' }, s)).toEqual([])
    // The retrieval models fall back to the generation key, which must not follow them to another host.
    expect(keysNeeded({ ...form, embedUrl: 'https://embed.example.com/v1' }, s)).toEqual(['modelKey'])
    expect(keysNeeded({ ...form, embedUrl: 'https://embed.example.com/v1', llmKey: 'sk' }, s)).toEqual([])
    // Self-hosted servers take no key.
    expect(keysNeeded({ ...form, modelProvider: 'self-hosted' }, s)).toEqual([])
  })

  it('warns before a change that can embed every memory again', () => {
    const s = settings()
    const form = formOf(s)
    expect(mayReembed({ ...form, llmModel: 'qwen-max' }, s)).toBe(false)
    expect(mayReembed({ ...form, modelProvider: 'self-hosted' }, s)).toBe(true)
    expect(mayReembed({ ...form, embedModel: 'other' }, s)).toBe(true)
  })

  it('compares origins as the daemon does', () => {
    expect(origin('https://A.example:443/v1')).toBe(origin('https://a.example/x'))
    expect(origin('http://127.0.0.1:8081')).not.toBe(origin('http://127.0.0.1:8082'))
    expect(origin('127.0.0.1:8081')).not.toBe(origin('http://127.0.0.1:8081'))
  })
})
