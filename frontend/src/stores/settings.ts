import { createSignal } from 'solid-js'
import type { AppConfig } from '../types'

const [config, setConfig] = createSignal<AppConfig | null>(null)
const [loading, setLoading] = createSignal(true)

export { config, setConfig, loading, setLoading }

export const $config = {
  get: config,
  set: setConfig,
}

export const $configLoading = {
  get: loading,
  set: setLoading,
}