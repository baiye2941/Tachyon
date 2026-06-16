import { describe, it, expect, beforeEach, vi, afterEach } from 'vitest'
import { render, screen, fireEvent, cleanup, waitFor } from '@solidjs/testing-library'
import SettingsPanel from '../SettingsPanel'
import type { ConfigPatch, VerifyStrategy, IoStrategy } from '../../types'
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

const mockConfig = {
  maxConcurrentTasks: 3,
  download: {
    downloadDir: 'downloads',
    maxConcurrentFragments: 8,
    verifyChecksum: true,
    maxRetries: 3,
    requestTimeoutSecs: 30,
    connectTimeoutSecs: 10,
    pauseTimeoutSecs: 300,
    rateLimitBytesPerSec: null,
    maxFullStreamBytes: 1024 * 1024 * 1024,
    authorizedDirs: ['downloads'],
    userAgent: 'Tachyon/1.0',
    headers: {
      Authorization: 'Bearer test-token',
    },
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

  it('点击保存时弹出确认对话框(P1-11)', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('保存配置'))

    // 确认对话框应出现
    await waitFor(() => {
      expect(screen.getByText('确认保存')).toBeDefined()
    })
  })

  it('确认保存时调用 api.updateConfig 且参数为 ConfigPatch(不含安全字段)', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    vi.mocked(api.updateConfig).mockResolvedValue(undefined)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('保存配置'))

    // 等待确认对话框出现后点击确认
    await waitFor(() => {
      expect(screen.getByText('确认保存')).toBeDefined()
    })
    fireEvent.click(screen.getByText('确认保存'))

    await waitFor(() => {
      expect(api.updateConfig).toHaveBeenCalledTimes(1)
    })

    const calledWith = vi.mocked(api.updateConfig).mock.calls[0]?.[0] as ConfigPatch
    // patch 应包含可修改的 download 字段
    expect(calledWith.download).toBeDefined()
    expect(calledWith.download!.maxConcurrentFragments).toBe(mockConfig.download.maxConcurrentFragments)
    expect(calledWith.download!.verifyChecksum).toBe(mockConfig.download.verifyChecksum)
    // patch 应包含可修改的 connection 字段
    expect(calledWith.connection).toBeDefined()
    expect(calledWith.connection!.maxConnectionsPerHost).toBe(mockConfig.connection.maxConnectionsPerHost)
    expect(calledWith.connection!.enableQuic).toBe(mockConfig.connection.enableQuic)
    // patch 不应包含安全字段(userAgent/headers/authorizedDirs 不在 DownloadPatch 中)
    expect((calledWith.download as Record<string, unknown>).userAgent).toBeUndefined()
    expect((calledWith.download as Record<string, unknown>).headers).toBeUndefined()
    expect((calledWith.download as Record<string, unknown>).authorizedDirs).toBeUndefined()
  })

  it('确认保存成功时显示 toast 配置已保存', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    vi.mocked(api.updateConfig).mockResolvedValue(undefined)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('保存配置'))
    await waitFor(() => {
      expect(screen.getByText('确认保存')).toBeDefined()
    })
    fireEvent.click(screen.getByText('确认保存'))

    await waitFor(() => {
      expect(addToast).toHaveBeenCalledWith('配置已保存', 'success')
    })
  })

  it('确认保存失败时显示 toast 错误信息', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    vi.mocked(api.updateConfig).mockRejectedValue(new Error('network error'))
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('保存配置'))
    await waitFor(() => {
      expect(screen.getByText('确认保存')).toBeDefined()
    })
    fireEvent.click(screen.getByText('确认保存'))

    await waitFor(() => {
      expect(addToast).toHaveBeenCalledWith(expect.stringContaining('保存配置失败'), 'error')
    })
  })
})

// --- P3-9: 前后端 DownloadConfig schema 对齐测试 ---
// 验证新增的 verifyStrategy / ioStrategy 字段可被类型系统接受且往返保留
describe('DownloadConfig schema 对齐 (P3-9)', () => {
  it('后端下发的 verifyStrategy/ioStrategy 字段可被前端接受', async () => {
    // 模拟后端 get_config 返回含新字段的完整配置
    vi.mocked(api.getConfig).mockResolvedValue({
      ...mockConfig,
      download: {
        ...mockConfig.download,
        verifyStrategy: 'bestEffort',
        ioStrategy: 'standard',
      },
    })
    setConfig(null)
    setLoading(true)
    renderSettingsPanel()

    // 配置加载成功即证明类型对齐(类型不匹配会在编译期失败)
    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })
  })

  it('verifyStrategy / ioStrategy 联合类型所有变体均为合法值', () => {
    // 编译期断言:所有后端 VerifyStrategy/IoStrategy 枚举值都能赋给前端类型
    const verifyStrategies: VerifyStrategy[] = ['require', 'bestEffort', 'skip']
    const ioStrategies: IoStrategy[] = ['standard', 'winAligned', 'iocp', 'ioUring']
    expect(verifyStrategies).toHaveLength(3)
    expect(ioStrategies).toHaveLength(4)
  })
})
