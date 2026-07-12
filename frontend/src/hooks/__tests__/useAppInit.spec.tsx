import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, waitFor } from '@solidjs/testing-library'
import { useAppInit } from '../useAppInit'
import { api } from '../../api/invoke'
import { addToast } from '../../stores/toast'
import type { AppConfig } from '../../types'

vi.mock('../../api/invoke', () => ({
  api: {
    getTaskList: vi.fn(),
    getConfig: vi.fn(),
    subscribeProgress: vi.fn(),
    getSnifferResources: vi.fn(),
  },
}))

vi.mock('../../stores/toast', () => ({
  addToast: vi.fn(),
}))

vi.mock('../../stores/downloads', () => ({
  $activeCount: { get: () => 0 },
  $totalSpeed: { get: () => 0 },
  refreshTaskList: vi.fn(),
}))

vi.mock('../../api/events', () => ({
  onRecoveryWarning: vi.fn(() => Promise.resolve(() => {})),
  onSnifferResourceAdded: vi.fn(() => Promise.resolve(() => {})),
  onClipboardUrlDetected: vi.fn(() => Promise.resolve(() => {})),
  onTaskNotification: vi.fn(() => Promise.resolve(() => {})),
}))

vi.mock('../../stores/speedHistory', () => ({
  pushSpeed: vi.fn(),
  setActiveTasksCount: vi.fn(),
}))

vi.mock('../useTauriEvent', () => ({
  useProgressListener: vi.fn(),
}))

vi.mock('@tauri-apps/plugin-notification', () => ({
  isPermissionGranted: vi.fn(() => Promise.resolve(false)),
  requestPermission: vi.fn(() => Promise.resolve('denied')),
  sendNotification: vi.fn(),
}))

function TestApp() {
  useAppInit(() => undefined, () => undefined)
  return <div>app</div>
}

describe('useAppInit', () => {
  beforeEach(() => {
    vi.mocked(api.getConfig).mockReset()
    vi.mocked(api.subscribeProgress).mockReset()
    vi.mocked(api.getSnifferResources).mockReset()
    vi.mocked(addToast).mockReset()
    vi.mocked(api.getConfig).mockResolvedValue({
      maxConcurrentTasks: 3,
      notifications: { enabled: false },
    } as AppConfig)
  })

  it('启动时调用 subscribeProgress 和 getSnifferResources', async () => {
    vi.mocked(api.subscribeProgress).mockResolvedValue(undefined)
    vi.mocked(api.getSnifferResources).mockResolvedValue([])

    render(() => <TestApp />)

    await waitFor(() => {
      expect(api.subscribeProgress).toHaveBeenCalledTimes(1)
      expect(api.getSnifferResources).toHaveBeenCalledTimes(1)
    })
  })

  it('subscribeProgress 失败时弹出 error toast', async () => {
    vi.mocked(api.subscribeProgress).mockRejectedValue(new Error('offline'))
    vi.mocked(api.getSnifferResources).mockResolvedValue([])

    render(() => <TestApp />)

    await waitFor(() => {
      expect(addToast).toHaveBeenCalled()
    })
  })
})
