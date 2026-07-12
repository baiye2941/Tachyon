import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, waitFor, cleanup } from '@solidjs/testing-library'
import { createEffect } from 'solid-js'
import { useTaskNotifications } from '../useTaskNotifications'
import { $config } from '../../stores/settings'
import type { AppConfig } from '../../types'

vi.mock('@tauri-apps/plugin-notification', () => ({
  isPermissionGranted: vi.fn(),
  requestPermission: vi.fn(),
  sendNotification: vi.fn(),
}))

vi.mock('../../api/events', () => ({
  onTaskNotification: vi.fn(() => Promise.resolve(() => {})),
}))

import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from '@tauri-apps/plugin-notification'
import { onTaskNotification } from '../../api/events'

function TestApp(props: { config: AppConfig | null }) {
  createEffect(() => {
    $config.set(props.config)
  })
  useTaskNotifications()
  return <div>app</div>
}

describe('useTaskNotifications', () => {
  let notificationHandler: ((payload: {
    taskId: string
    title: string
    body: string
    type: 'completed' | 'failed'
  }) => void) | null = null
  let unlistenMock = vi.fn()

  beforeEach(() => {
    $config.set(null)
    vi.mocked(isPermissionGranted).mockReset()
    vi.mocked(requestPermission).mockReset()
    vi.mocked(sendNotification).mockReset()
    vi.mocked(onTaskNotification).mockReset()
    unlistenMock = vi.fn()
    notificationHandler = null
    vi.mocked(onTaskNotification).mockImplementation((handler) => {
      notificationHandler = handler
      return Promise.resolve(unlistenMock)
    })
  })

  afterEach(() => {
    cleanup()
  })

  const makeConfig = (enabled: boolean): AppConfig => ({
    maxConcurrentTasks: 3,
    download: {
      downloadDir: 'downloads',
      maxConcurrentFragments: 8,
      maxRetries: 3,
      requestTimeoutSecs: 30,
      connectTimeoutSecs: 10,
      verifyChecksum: true,
      pauseTimeoutSecs: 300,
      rateLimitBytesPerSec: null,
      maxFullStreamBytes: 1024 * 1024 * 1024,
      authorizedDirs: ['downloads'],
      userAgent: 'Tachyon/1.0',
      headers: {},
    },
    connection: {
      maxConnectionsPerHost: 4,
      maxGlobalConnections: 256,
      keepAliveTimeoutSecs: 60,
      connectTimeoutSecs: 10,
      enableHttp2: true,
      enableQuic: false,
    },
    scheduler: {
      minFragmentSize: 1048576,
      maxFragmentSize: 5242880,
      samplingIntervalSecs: 5,
      ewmaAlpha: 0.3,
    },
    magnet: {
      metadataTimeoutSecs: 30,
      downloadTimeoutSecs: 60,
      enableDht: true,
      enableUpnp: true,
      trackers: [],
      disableDhtPersistence: false,
      peerWaitTimeoutSecs: 300,
      socksProxyUrl: null,
      peerConnectTimeoutSecs: 8,
      peerReadWriteTimeoutSecs: 10,
      forceTrackerIntervalSecs: 120,
      deferWritesUpToMb: 16,
      disableDhtWhenSocks: true,
      peerAddrs: [],
    },
    hub: {
      sourceMode: 'mirror',
    },
    notifications: {
      enabled,
    },
  })

  it('权限已授予时监听事件并调用 sendNotification', async () => {
    vi.mocked(isPermissionGranted).mockResolvedValue(true)

    render(() => <TestApp config={makeConfig(true)} />)

    await waitFor(() => {
      expect(notificationHandler).not.toBeNull()
    })

    notificationHandler!({
      taskId: 't1',
      title: '下载完成: model.gguf',
      body: 'model.gguf 已下载完成',
      type: 'completed',
    })

    await waitFor(() => {
      expect(sendNotification).toHaveBeenCalledWith({
        title: '下载完成: model.gguf',
        body: 'model.gguf 已下载完成',
      })
    })
  })

  it('权限未授予时请求权限', async () => {
    vi.mocked(isPermissionGranted).mockResolvedValue(false)
    vi.mocked(requestPermission).mockResolvedValue('granted')

    render(() => <TestApp config={makeConfig(true)} />)

    await waitFor(() => {
      expect(requestPermission).toHaveBeenCalledTimes(1)
    })
  })

  it('通知设置关闭时不请求权限也不监听事件', async () => {
    vi.mocked(isPermissionGranted).mockResolvedValue(false)

    render(() => <TestApp config={makeConfig(false)} />)

    await new Promise((resolve) => setTimeout(resolve, 50))
    expect(requestPermission).not.toHaveBeenCalled()
    expect(notificationHandler).toBeNull()
  })

  it('事件到达时若设置已关闭不发送通知', async () => {
    vi.mocked(isPermissionGranted).mockResolvedValue(true)

    render(() => <TestApp config={makeConfig(true)} />)

    await waitFor(() => {
      expect(notificationHandler).not.toBeNull()
    })

    // 关闭通知
    $config.set(makeConfig(false))

    notificationHandler!({
      taskId: 't2',
      title: '下载失败: data.zip',
      body: 'connection reset',
      type: 'failed',
    })

    await new Promise((resolve) => setTimeout(resolve, 50))
    expect(sendNotification).not.toHaveBeenCalled()
  })
})
