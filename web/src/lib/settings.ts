import type { KeySettingName, ModelSettings, ModelSettingsReq, SettingSource, TextSettingName } from '../api/types'

export type Provider = 'api' | 'self-hosted'

/** The settings form: every text setting as typed, and a key only when one is entered. */
export type Form = Record<TextSettingName, string> & Record<KeySettingName, string>

export const TEXT_SETTINGS: readonly TextSettingName[] = [
  'llmBaseUrl',
  'llmModel',
  'modelProvider',
  'embedUrl',
  'rerankUrl',
  'embedModel',
  'rerankModel',
]

/** The base URL a provider uses when none is set (memory-config's defaults). */
export const DEFAULT_URLS: Record<Provider, { embedUrl: string; rerankUrl: string }> = {
  api: {
    embedUrl: 'https://maas.qianwenaiapi.com/compatible-mode/v1',
    rerankUrl: 'https://maas.qianwenaiapi.com/compatible-api/v1',
  },
  'self-hosted': { embedUrl: 'http://127.0.0.1:8081', rerankUrl: 'http://127.0.0.1:8082' },
}

/** The form for the settings as they are. The generation URL and model always hold a value; the rest may be
 * empty, which keeps the default. */
export function formOf(s: ModelSettings): Form {
  const form = { llmKey: '', modelKey: '' } as Form
  for (const name of TEXT_SETTINGS) form[name] = s.fields[name].value
  form.llmBaseUrl ||= s.fields.llmBaseUrl.effective
  form.llmModel ||= s.fields.llmModel.effective
  form.modelProvider ||= s.fields.modelProvider.effective
  return form
}

/** The request for `form`: values trimmed, a key only when one was entered. */
export function proposal(form: Form): ModelSettingsReq {
  const req = {} as ModelSettingsReq
  for (const name of TEXT_SETTINGS) req[name] = form[name].trim()
  for (const name of ['llmKey', 'modelKey'] as const) {
    const key = form[name].trim()
    if (key) req[name] = key
  }
  return req
}

/** The settings `form` changes. */
export function changed(form: Form, s: ModelSettings): string[] {
  const now = formOf(s)
  const names: string[] = TEXT_SETTINGS.filter((n) => form[n].trim() !== now[n])
  if (form.llmKey.trim()) names.push('llmKey')
  if (form.modelKey.trim()) names.push('modelKey')
  return names
}

/** The URL a retrieval setting resolves to: its value, else the provider's default. */
export const urlOf = (form: Form, name: 'embedUrl' | 'rerankUrl') =>
  form[name].trim() || DEFAULT_URLS[(form.modelProvider as Provider) ?? 'api']?.[name] || ''

/** Scheme, host and port: where a request goes. The daemon never sends a saved key to another one. */
export function origin(url: string): string | null {
  try {
    const u = new URL(url)
    return `${u.protocol}//${u.host.toLowerCase()}`
  } catch {
    return null
  }
}

/** Which keys `form` must enter again because their endpoint moves to another host (the daemon's rule). */
export function keysNeeded(form: Form, s: ModelSettings): KeySettingName[] {
  const f = s.fields
  const needed: KeySettingName[] = []
  const llmMoved = origin(form.llmBaseUrl.trim()) !== origin(f.llmBaseUrl.effective)
  if (llmMoved && !form.llmKey.trim() && f.llmKey.set) needed.push('llmKey')
  const moved =
    origin(urlOf(form, 'embedUrl')) !== origin(f.embedUrl.effective) ||
    origin(urlOf(form, 'rerankUrl')) !== origin(f.rerankUrl.effective)
  const reused = f.modelKey.set || (!form.llmKey.trim() && f.llmKey.set)
  if (form.modelProvider === 'api' && moved && !form.modelKey.trim() && reused) needed.push('modelKey')
  return needed
}

/** Whether `form` may change the embedding model, which embeds every memory again after the restart. */
export const mayReembed = (form: Form, s: ModelSettings) =>
  changed(form, s).some((n) => ['modelProvider', 'embedUrl', 'embedModel'].includes(n))

export const SOURCE_LABEL: Record<SettingSource, string> = {
  flag: '命令行参数',
  environment: '环境变量',
  file: '配置文件',
  default: '默认值',
}

/** Why the page cannot change a setting, or null. */
export function lockedBy(source: SettingSource): string | null {
  if (source === 'flag') return '由 daemon 的命令行参数设定，这里改不了'
  if (source === 'environment') return '由 daemon 的环境变量设定，这里改不了'
  return null
}
