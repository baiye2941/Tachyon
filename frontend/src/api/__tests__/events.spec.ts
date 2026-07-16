import { describe, it, expect, vi, beforeEach } from 'vitest'

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
}))

import { listen } from '@tauri-apps/api/event'
import { onProgressUpdate, onRecoveryWarning } from '../events'

describe('api/events FT-05', () => {
  beforeEach(() => {
    vi.mocked(listen).mockReset()
  })

  it('listen 成功时返回 unlisten', async () => {
    const unlisten = vi.fn()
    vi.mocked(listen).mockResolvedValue(unlisten as never)
    const fn = await onProgressUpdate(() => {})
    expect(fn).toBe(unlisten)
    expect(listen).toHaveBeenCalledWith('progress-update', expect.any(Function))
  })

  it('listen 失败时 reject 而非 no-op', async () => {
    vi.mocked(listen).mockRejectedValue(new Error('no tauri'))
    await expect(onProgressUpdate(() => {})).rejects.toThrow('no tauri')
  })

  it('onRecoveryWarning 同样透传 reject', async () => {
    vi.mocked(listen).mockRejectedValue(new Error('boom'))
    await expect(onRecoveryWarning(() => {})).rejects.toThrow('boom')
  })
})
