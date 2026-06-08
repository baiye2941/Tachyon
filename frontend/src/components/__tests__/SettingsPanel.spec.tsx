import { describe, it, expect, beforeEach, vi, afterEach } from 'vitest'
import { render, screen, fireEvent, cleanup, waitFor } from '@solidjs/testing-library'
import SettingsPanel from '../SettingsPanel'
import type { AppConfig } from '../../types'
import { setConfig, setLoading } from '../../stores/settings'
import { api } from '../../api/invoke'
import { addToast } from '../../stores/toast'

vi.mock('../../api/invoke', () => ({
  api: {
    getConfig: vi.fn(),
    updateConfig: vi.fn(),
  },
}))

vi.mock('../../stores/toast', () => ({
  addToast: vi.fn(),
}))

const renderSettingsPanel = () => render(() => <SettingsPanel visible={true} onClose={() => undefined} />)

const mockConfig: AppConfig = {
  maxConcurrentTasks: 3,
  download: {
    downloadDir: 'downloads',
    maxConcurrentFragments: 8,
    verifyChecksum: true,
    maxRetries: 3,
    requestTimeoutSecs: 30,
    userAgent: 'Tachyon/1.0',
  },
  connection: {
    maxConnectionsPerHost: 4,
    enableQuic: false,
    enableHttp2: true,
    maxGlobalConnections: 32,
    keepAliveTimeoutSecs: 60,
    connectTimeoutSecs: 10,
  },
  scheduler: {
    minFragmentSize: 1048576,
    maxFragmentSize: 5242880,
    samplingIntervalSecs: 5,
    ewmaAlpha: 0.3,
  },
}

describe('SettingsPanel', () => {
  beforeEach(() => {
    setConfig(null)
    setLoading(true)
    vi.mocked(api.getConfig).mockReset()
    vi.mocked(api.updateConfig).mockReset()
    vi.mocked(addToast).mockReset()
  })

  afterEach(() => {
    cleanup()
  })

  it('渲染 SettingsPanel 时显示加载状态', () => {
    vi.mocked(api.getConfig).mockReturnValue(new Promise(() => {}))
    renderSettingsPanel()
    expect(screen.getByText('加载配置中...')).toBeDefined()
  })

  it('从 api.getConfig 加载配置后正确填充表单字段', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    expect(screen.getByDisplayValue('downloads')).toBeDefined()
    fireEvent.click(screen.getByText('下载'))
    expect((screen.getByLabelText('最大并发任务数') as HTMLInputElement).value).toBe('3')
    expect((screen.getByLabelText('最大并发分片数') as HTMLInputElement).value).toBe('8')
    fireEvent.click(screen.getByText('连接'))
    expect((screen.getByLabelText('每个主机最大连接数') as HTMLInputElement).value).toBe('4')
  })

  it('点击保存时调用 api.updateConfig 且参数包含完整的 scheduler 字段', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    vi.mocked(api.updateConfig).mockResolvedValue(undefined)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('保存配置'))

    await waitFor(() => {
      expect(api.updateConfig).toHaveBeenCalledTimes(1)
    })

    const calledWith = vi.mocked(api.updateConfig).mock.calls[0][0] as AppConfig
    expect(calledWith.scheduler).toBeDefined()
    expect(calledWith.scheduler.minFragmentSize).toBe(mockConfig.scheduler.minFragmentSize)
    expect(calledWith.scheduler.maxFragmentSize).toBe(mockConfig.scheduler.maxFragmentSize)
    expect(calledWith.scheduler.samplingIntervalSecs).toBe(mockConfig.scheduler.samplingIntervalSecs)
    expect(calledWith.scheduler.ewmaAlpha).toBe(mockConfig.scheduler.ewmaAlpha)
  })

  it('保存成功时显示 toast 配置已保存', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    vi.mocked(api.updateConfig).mockResolvedValue(undefined)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('保存配置'))

    await waitFor(() => {
      expect(addToast).toHaveBeenCalledWith('配置已保存', 'success')
    })
  })

  it('保存失败时显示 toast 错误信息', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    vi.mocked(api.updateConfig).mockRejectedValue(new Error('network error'))
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('保存配置'))

    await waitFor(() => {
      expect(addToast).toHaveBeenCalledWith(expect.stringContaining('保存配置失败'), 'error')
    })
  })
})
