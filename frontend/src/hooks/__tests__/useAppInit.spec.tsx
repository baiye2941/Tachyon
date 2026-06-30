import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, waitFor } from '@solidjs/testing-library'
import { useAppInit } from '../useAppInit'
import { api } from '../../api/invoke'
import { addToast } from '../../stores/toast'

vi.mock('../../api/invoke', () => ({
  api: {
    getTaskList: vi.fn(),
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
}))

vi.mock('../../stores/speedHistory', () => ({
  pushSpeed: vi.fn(),
  setActiveTasksCount: vi.fn(),
}))

vi.mock('../useTauriEvent', () => ({
  useProgressListener: vi.fn(),
}))

function TestApp() {
  useAppInit(() => undefined)
  return <div>app</div>
}

describe('useAppInit', () => {
  beforeEach(() => {
    vi.mocked(api.subscribeProgress).mockReset()
    vi.mocked(api.getSnifferResources).mockReset()
    vi.mocked(addToast).mockReset()
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
