import { createSignal } from 'solid-js'

const STORAGE_KEY = 'tachyon-experimental'

type ExperimentalFeatures = {
  huggingface: boolean
}

function loadFromStorage(): ExperimentalFeatures {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (raw) return JSON.parse(raw)
  } catch {
    // 解析失败时使用默认值
  }
  return { huggingface: false }
}

function saveToStorage(features: ExperimentalFeatures): void {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(features))
}

const [features, setFeatures] = createSignal<ExperimentalFeatures>(loadFromStorage())

export const $experimental = {
  isEnabled(key: keyof ExperimentalFeatures): boolean {
    return features()[key] ?? false
  },
  toggle(key: keyof ExperimentalFeatures): void {
    setFeatures((prev) => {
      const next = { ...prev, [key]: !prev[key] }
      saveToStorage(next)
      return next
    })
  },
}
