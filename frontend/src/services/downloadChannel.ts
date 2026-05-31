import { Channel } from '@tauri-apps/api/core'
import { batch } from 'solid-js'
import type { ProgressEvent } from '../types'
import { setTasks } from '../stores/downloads'

type ProgressHandler = (payload: ProgressEvent) => void

const handlers: Set<ProgressHandler> = new Set()

let channelInstance: Channel<ProgressEvent> | null = null

function ensureChannel(): Channel<ProgressEvent> {
  if (channelInstance) return channelInstance

  channelInstance = new Channel<ProgressEvent>('progress-update')

  channelInstance.onmessage = (payload: ProgressEvent) => {
    batch(() => {
      for (const handler of handlers) {
        handler(payload)
      }
    })
  }

  return channelInstance
}

export async function bindProgressChannel(): Promise<void> {
  const ch = ensureChannel()
  const { invoke } = await import('@tauri-apps/api/core')
  await invoke('bind_progress_channel', { channel: ch })
}

export function onProgressUpdate(handler: ProgressHandler): () => void {
  ensureChannel()
  handlers.add(handler)
  return () => {
    handlers.delete(handler)
  }
}
