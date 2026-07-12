import { describe, it, expect, beforeEach, vi, afterEach } from 'vitest'
import { render, screen, fireEvent, cleanup, waitFor, within } from '@solidjs/testing-library'
import SettingsPanel from '../settings/SettingsPanel'
import type { ConfigPatch, VerifyStrategy, IoStrategy } from '../../types'
import { setConfig, setLoading } from '../../stores/settings'
import { api } from '../../api/invoke'
import { addToast } from '../../stores/toast'
import { refreshTaskList } from '../../stores/downloads'
import { save, open } from '@tauri-apps/plugin-dialog'
import { setShortcut, resetAllShortcuts } from '../../stores/shortcuts'

vi.mock('../../api/invoke', () => ({
  api: {
    getConfig: vi.fn(),
    updateConfig: vi.fn(),
    getSupportedProtocols: vi.fn(),
    getAppInfo: vi.fn(),
    exportBackup: vi.fn(),
    importBackup: vi.fn(),
  },
}))

vi.mock('../../stores/toast', () => ({
  addToast: vi.fn(),
}))

vi.mock('../../stores/downloads', () => ({
  refreshTaskList: vi.fn(),
}))

vi.mock('@tauri-apps/plugin-dialog', () => ({
  save: vi.fn(),
  open: vi.fn(),
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
    sourceMode: 'mirror' as const,
  },
  notifications: {
    enabled: true,
  },
}

describe('SettingsPanel', () => {
  beforeEach(() => {
    setConfig(null)
    setLoading(true)
    resetAllShortcuts()
    localStorage.clear()
    vi.mocked(api.getConfig).mockReset()
    vi.mocked(api.updateConfig).mockReset()
    vi.mocked(api.getSupportedProtocols).mockReset()
    vi.mocked(api.getAppInfo).mockReset()
    vi.mocked(api.exportBackup).mockReset()
    vi.mocked(api.importBackup).mockReset()
    vi.mocked(addToast).mockReset()
    vi.mocked(refreshTaskList).mockReset()
    vi.mocked(save).mockReset()
    vi.mocked(open).mockReset()
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
    // patch 应包含 magnet 字段(含 disableDhtPersistence)
    expect(calledWith.magnet).toBeDefined()
    expect(calledWith.magnet!.disableDhtPersistence).toBe(mockConfig.magnet.disableDhtPersistence)
    expect(calledWith.magnet!.socksProxyUrl).toBe(null)
    // patch 不应包含安全字段(userAgent/headers/authorizedDirs 不在 DownloadPatch 中)
    expect((calledWith.download as Record<string, unknown>).userAgent).toBeUndefined()
    expect((calledWith.download as Record<string, unknown>).headers).toBeUndefined()
    expect((calledWith.download as Record<string, unknown>).authorizedDirs).toBeUndefined()
  })

  it('切换禁用 DHT 持久化开关后保存,patch 携带新值(回归:设置页 DHT 配置开关)', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    vi.mocked(api.updateConfig).mockResolvedValue(undefined)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    // 切到 magnet tab
    fireEvent.click(screen.getByText('磁力链接'))
    await waitFor(() => {
      expect(screen.getByText('启用 DHT 协议')).toBeDefined()
    })

    // 找到"禁用 DHT 持久化"开关的 toggle 按钮并点击翻转
    const toggleLabel = screen.getByText('禁用 DHT 持久化(仅内存)')
    // label 被包在左侧分组 div 内,需向上找到 .justify-between 容器再取 button
    const toggleBtn = toggleLabel.closest('.flex.items-center.justify-between')!.querySelector('button')!
    fireEvent.click(toggleBtn)

    // 保存
    fireEvent.click(screen.getByText('保存配置'))
    await waitFor(() => {
      expect(screen.getByText('确认保存')).toBeDefined()
    })
    fireEvent.click(screen.getByText('确认保存'))

    await waitFor(() => {
      expect(api.updateConfig).toHaveBeenCalledTimes(1)
    })

    const calledWith = vi.mocked(api.updateConfig).mock.calls[0]?.[0] as ConfigPatch
    expect(calledWith.magnet).toBeDefined()
    expect(calledWith.magnet!.disableDhtPersistence).toBe(true)
  })

  it('填写 SOCKS5 代理后保存,patch 携带代理 URL(回归:BT 代理支持)', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    vi.mocked(api.updateConfig).mockResolvedValue(undefined)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('磁力链接'))
    await waitFor(() => {
      expect(screen.getByPlaceholderText('socks5://127.0.0.1:7897')).toBeDefined()
    })

    const input = screen.getByPlaceholderText('socks5://127.0.0.1:7897') as HTMLInputElement
    fireEvent.input(input, { target: { value: 'socks5://127.0.0.1:7897' } })

    fireEvent.click(screen.getByText('保存配置'))
    await waitFor(() => {
      expect(screen.getByText('确认保存')).toBeDefined()
    })
    fireEvent.click(screen.getByText('确认保存'))

    await waitFor(() => {
      expect(api.updateConfig).toHaveBeenCalledTimes(1)
    })

    const calledWith = vi.mocked(api.updateConfig).mock.calls[0]?.[0] as ConfigPatch
    expect(calledWith.magnet).toBeDefined()
    expect(calledWith.magnet!.socksProxyUrl).toBe('socks5://127.0.0.1:7897')
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

  it('patch 包含新增可编辑字段(requestTimeoutSecs/rateLimit/maxGlobalConnections/keepAlive)', async () => {
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
      expect(api.updateConfig).toHaveBeenCalledTimes(1)
    })

    const calledWith = vi.mocked(api.updateConfig).mock.calls[0]?.[0] as ConfigPatch
    // 新增可编辑字段应出现在 patch 中
    expect(calledWith.download!.requestTimeoutSecs).toBe(mockConfig.download.requestTimeoutSecs)
    expect(calledWith.download!.rateLimitBytesPerSec).toBe(mockConfig.download.rateLimitBytesPerSec)
    expect(calledWith.connection!.maxGlobalConnections).toBe(mockConfig.connection.maxGlobalConnections)
    expect(calledWith.connection!.keepAliveTimeoutSecs).toBe(mockConfig.connection.keepAliveTimeoutSecs)
  })

  // --- Task 9: Peer 优化新增配置项 ---
  it('加载配置后 magnet tab 渲染新增 peer 配置项(NumberInput + toggle)', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('磁力链接'))

    // 四个 NumberInput 应以 aria-label 可达
    expect((screen.getByLabelText('Peer 连接超时') as HTMLInputElement).value).toBe('8')
    expect((screen.getByLabelText('Peer 读写超时') as HTMLInputElement).value).toBe('10')
    expect((screen.getByLabelText('强制 Tracker 间隔') as HTMLInputElement).value).toBe('120')
    expect((screen.getByLabelText('延迟写入缓冲') as HTMLInputElement).value).toBe('16')
    // 新增 toggle "SOCKS5 时禁用 DHT" 应可见
    expect(screen.getByText('SOCKS5 时禁用 DHT')).toBeDefined()
    // 需重启生效 / 对新任务生效 标记应可见
    expect(screen.getAllByText('需重启生效').length).toBeGreaterThan(0)
    expect(screen.getByText('对新任务生效')).toBeDefined()
  })

  it('修改 Peer 连接超时后保存,patch 携带新值', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    vi.mocked(api.updateConfig).mockResolvedValue(undefined)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('磁力链接'))
    await waitFor(() => {
      expect(screen.getByLabelText('Peer 连接超时')).toBeDefined()
    })

    const input = screen.getByLabelText('Peer 连接超时') as HTMLInputElement
    fireEvent.input(input, { target: { value: '15' } })

    fireEvent.click(screen.getByText('保存配置'))
    await waitFor(() => {
      expect(screen.getByText('确认保存')).toBeDefined()
    })
    fireEvent.click(screen.getByText('确认保存'))

    await waitFor(() => {
      expect(api.updateConfig).toHaveBeenCalledTimes(1)
    })

    const calledWith = vi.mocked(api.updateConfig).mock.calls[0]?.[0] as ConfigPatch
    expect(calledWith.magnet).toBeDefined()
    expect(calledWith.magnet!.peerConnectTimeoutSecs).toBe(15)
  })

  it('切换 SOCKS5 禁用 DHT 开关后保存,patch 携带翻转值', async () => {
    // mockConfig 中 disableDhtWhenSocks=true,翻转后应为 false
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    vi.mocked(api.updateConfig).mockResolvedValue(undefined)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('磁力链接'))
    await waitFor(() => {
      expect(screen.getByText('SOCKS5 时禁用 DHT')).toBeDefined()
    })

    const toggleLabel = screen.getByText('SOCKS5 时禁用 DHT')
    // label 被包在左侧分组 div 内,需向上找到 .justify-between 容器再取 button
    const toggleBtn = toggleLabel.closest('.flex.items-center.justify-between')!.querySelector('button')!
    fireEvent.click(toggleBtn)

    fireEvent.click(screen.getByText('保存配置'))
    await waitFor(() => {
      expect(screen.getByText('确认保存')).toBeDefined()
    })
    fireEvent.click(screen.getByText('确认保存'))

    await waitFor(() => {
      expect(api.updateConfig).toHaveBeenCalledTimes(1)
    })

    const calledWith = vi.mocked(api.updateConfig).mock.calls[0]?.[0] as ConfigPatch
    expect(calledWith.magnet).toBeDefined()
    expect(calledWith.magnet!.disableDhtWhenSocks).toBe(false)
  })

  it('未修改时保存,patch 仍携带全部新增字段(全量回传,与现有模式一致)', async () => {
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
      expect(api.updateConfig).toHaveBeenCalledTimes(1)
    })

    const calledWith = vi.mocked(api.updateConfig).mock.calls[0]?.[0] as ConfigPatch
    expect(calledWith.magnet).toBeDefined()
    expect(calledWith.magnet!.peerConnectTimeoutSecs).toBe(mockConfig.magnet.peerConnectTimeoutSecs)
    expect(calledWith.magnet!.peerReadWriteTimeoutSecs).toBe(mockConfig.magnet.peerReadWriteTimeoutSecs)
    expect(calledWith.magnet!.forceTrackerIntervalSecs).toBe(mockConfig.magnet.forceTrackerIntervalSecs)
    expect(calledWith.magnet!.deferWritesUpToMb).toBe(mockConfig.magnet.deferWritesUpToMb)
    expect(calledWith.magnet!.disableDhtWhenSocks).toBe(mockConfig.magnet.disableDhtWhenSocks)
  })

  it('About 标签展示支持协议 + 只读 User-Agent', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    vi.mocked(api.getSupportedProtocols).mockResolvedValue(['http', 'https', 'ftp'])
    vi.mocked(api.getAppInfo).mockResolvedValue({ version: '1.2.3', name: 'Tachyon' })
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('关于'))

    await waitFor(() => {
      // 协议文本经 CSS text-transform:uppercase 视觉大写,但 DOM 文本内容为原始小写
      expect(screen.getByText('http')).toBeDefined()
      expect(screen.getByText('https')).toBeDefined()
    })
    // 只读 User-Agent 展示后端值
    expect(screen.getByText('Tachyon/1.0')).toBeDefined()
  })

  // --- 快捷键设置页 ---
  it('General 标签页渲染通知开关且可切换', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    // 默认在 General 标签,通知开关应可见
    expect(screen.getByText('任务完成/失败时显示系统通知')).toBeDefined()

    // 点击开关切换
    const toggleLabel = screen.getByText('任务完成/失败时显示系统通知')
    const toggleBtn = toggleLabel.closest('.flex.items-center.justify-between')!.querySelector('button')!
    fireEvent.click(toggleBtn)

    // 保存后 patch 应携带翻转后的通知设置
    fireEvent.click(screen.getByText('保存配置'))
    await waitFor(() => {
      expect(screen.getByText('确认保存')).toBeDefined()
    })
    fireEvent.click(screen.getByText('确认保存'))

    await waitFor(() => {
      expect(api.updateConfig).toHaveBeenCalledTimes(1)
    })

    const calledWith = vi.mocked(api.updateConfig).mock.calls[0]?.[0] as ConfigPatch
    expect(calledWith.notifications).toBeDefined()
    expect(calledWith.notifications!.enabled).toBe(false)
  })

  it('点击 shortcuts tab 进入快捷键设置页', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('快捷键'))
    expect(screen.getByText('部分组合键可能被浏览器或系统拦截')).toBeDefined()
    expect(screen.getByText('打开命令面板')).toBeDefined()
  })

  it('录制快捷键后保存,绑定更新', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('快捷键'))

    const editBtn = screen.getByLabelText('修改 打开命令面板 的快捷键')
    fireEvent.click(editBtn)

    fireEvent.keyDown(window, { key: 'X', ctrlKey: true })

    await waitFor(() => {
      expect(screen.getByText('X')).toBeDefined()
    })

    fireEvent.click(screen.getByText('保存'))

    await waitFor(() => {
      const row = screen.getByText('打开命令面板').closest('.flex.items-center.justify-between') as HTMLElement
      expect(row.textContent).toContain('Ctrl')
      expect(row.textContent).toContain('X')
    })
  })

  it('录制冲突快捷键时禁用保存并提示冲突', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('快捷键'))

    const editBtn = screen.getByLabelText('修改 打开命令面板 的快捷键')
    fireEvent.click(editBtn)

    // Ctrl+B 是切换侧边栏的默认绑定,与打开命令面板冲突
    fireEvent.keyDown(window, { key: 'B', ctrlKey: true })

    await waitFor(() => {
      expect(screen.getByText(/已被/)).toBeDefined()
    })

    const saveBtn = screen.getByText('保存') as HTMLButtonElement
    expect(saveBtn.disabled).toBe(true)
  })

  it('单条重置恢复默认绑定', async () => {
    setShortcut('shortcut.openCommandPalette', ['Ctrl', 'Shift', 'X'])
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('快捷键'))

    const row = screen.getByText('打开命令面板').closest('.flex.items-center.justify-between') as HTMLElement
    const resetBtn = within(row).getByText('恢复默认')
    fireEvent.click(resetBtn)

    await waitFor(() => {
      expect(row.textContent).toContain('Ctrl')
      expect(row.textContent).toContain('K')
      expect(row.textContent).not.toContain('X')
    })
  })

  it('全部重置恢复所有默认绑定', async () => {
    setShortcut('shortcut.openCommandPalette', ['Ctrl', 'Shift', 'X'])
    setShortcut('shortcut.toggleSidebar', ['Ctrl', 'Shift', 'B'])
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('快捷键'))

    fireEvent.click(screen.getByText('恢复全部默认'))

    await waitFor(() => {
      const paletteRow = screen.getByText('打开命令面板').closest('.flex.items-center.justify-between') as HTMLElement
      expect(paletteRow.textContent).toContain('Ctrl')
      expect(paletteRow.textContent).toContain('K')
      expect(paletteRow.textContent).not.toContain('X')
    })
  })

  // --- Task 4-10: 设置项导入/导出 ---
  it('通用标签页显示导出/导入按钮', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    expect(screen.getByText('导出配置与任务')).toBeDefined()
    expect(screen.getByText('导入配置与任务')).toBeDefined()
  })

  it('导出备份时调用文件对话框与 api.exportBackup', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    vi.mocked(save).mockResolvedValue('/tmp/tachyon-backup.json')
    vi.mocked(api.exportBackup).mockResolvedValue(undefined)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('导出配置与任务'))

    await waitFor(() => {
      expect(save).toHaveBeenCalledTimes(1)
    })
    expect(api.exportBackup).toHaveBeenCalledWith('/tmp/tachyon-backup.json')
    await waitFor(() => {
      expect(addToast).toHaveBeenCalledWith('备份已导出', 'success')
    })
  })

  it('导入备份(覆盖)时调用 api.importBackup(true) 并刷新任务列表', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    vi.mocked(open).mockResolvedValue('/tmp/tachyon-backup.json')
    vi.mocked(api.importBackup).mockResolvedValue(5)
    vi.mocked(refreshTaskList).mockResolvedValue(undefined)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('导入配置与任务'))

    await waitFor(() => {
      expect(screen.getByText('导入备份')).toBeDefined()
    })
    fireEvent.click(screen.getByText('覆盖'))

    await waitFor(() => {
      expect(api.importBackup).toHaveBeenCalledWith('/tmp/tachyon-backup.json', true)
    })
    await waitFor(() => {
      expect(refreshTaskList).toHaveBeenCalledTimes(1)
    })
    await waitFor(() => {
      expect(addToast).toHaveBeenCalledWith('已导入 5 个任务', 'success')
    })
  })

  it('导入备份(合并)时调用 api.importBackup(false) 并刷新任务列表', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    vi.mocked(open).mockResolvedValue('/tmp/tachyon-backup.json')
    vi.mocked(api.importBackup).mockResolvedValue(2)
    vi.mocked(refreshTaskList).mockResolvedValue(undefined)
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('导入配置与任务'))

    await waitFor(() => {
      expect(screen.getByText('导入备份')).toBeDefined()
    })
    fireEvent.click(screen.getByText('合并'))

    await waitFor(() => {
      expect(api.importBackup).toHaveBeenCalledWith('/tmp/tachyon-backup.json', false)
    })
    await waitFor(() => {
      expect(refreshTaskList).toHaveBeenCalledTimes(1)
    })
  })

  it('导出备份失败时显示错误 toast', async () => {
    vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
    vi.mocked(save).mockResolvedValue('/tmp/tachyon-backup.json')
    vi.mocked(api.exportBackup).mockRejectedValue(new Error('disk full'))
    renderSettingsPanel()

    await waitFor(() => {
      expect(screen.queryByText('加载配置中...')).toBeNull()
    })

    fireEvent.click(screen.getByText('导出配置与任务'))

    await waitFor(() => {
      expect(addToast).toHaveBeenCalledWith(expect.stringContaining('导出备份失败'), 'error')
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
