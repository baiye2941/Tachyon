/// <reference types="node" />
import { describe, it, expect, beforeEach, vi, afterEach } from 'vitest'
import { render, screen, fireEvent, cleanup, waitFor } from '@solidjs/testing-library'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import SettingsPanel from '../settings/SettingsPanel'
import { setConfig, setLoading } from '../../stores/settings'
import { api } from '../../api/invoke'

vi.mock('../../api/invoke', () => ({
  api: {
    getConfig: vi.fn(),
    updateConfig: vi.fn(),
    getSupportedProtocols: vi.fn(),
    getAppInfo: vi.fn(),
    exportBackup: vi.fn(),
    importBackup: vi.fn(),
    getBtProxyCoverage: vi.fn().mockResolvedValue(null),
    getQuicCapability: vi.fn().mockResolvedValue({
      enableQuic: false,
      effectiveQuic: false,
      http3Compiled: false,
    }),
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

const renderSettingsPanel = () =>
  render(() => <SettingsPanel visible={true} onClose={() => undefined} />)

// 与 SettingsPanel.spec.tsx 同构的最小配置夹具
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
    proxy: null as string | null,
    ioStrategy: 'standard' as const,
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
    allowPrivatePeers: false,
    peerAddrs: [],
  },
  hub: {
    sourceMode: 'mirror' as const,
  },
  notifications: {
    enabled: true,
  },
}

const readSrc = (rel: string): string =>
  readFileSync(resolve(process.cwd(), rel), 'utf-8')

describe('设置页 polish 回归', () => {
  // --- 契约 a:label 色层级统一 ---
  describe('label 色层级', () => {
    it('ToggleItem label 使用 --color-text-secondary(与 SliderItem 同层级)', () => {
      const content = readSrc('src/components/settings/items/ToggleItem.tsx')
      expect(content).toContain('var(--color-text-secondary)')
      expect(content).not.toContain('var(--color-text-title)')
    })

    it('SliderItem label 保持 --color-text-secondary(基准侧不回归)', () => {
      const content = readSrc('src/components/settings/items/SliderItem.tsx')
      expect(content).toContain('var(--color-text-secondary)')
    })
  })

  // --- 契约 b:hint 去掉负 margin hack ---
  describe('hint 布局', () => {
    it('GeneralTab 不再使用 margin-top:-12px 贴合上一控件', () => {
      const content = readSrc('src/components/settings/tabs/GeneralTab.tsx')
      expect(content).not.toContain('-12px')
    })
  })

  // --- 契约 c:备份按钮唯一入口 ---
  describe('备份按钮唯一入口', () => {
    beforeEach(() => {
      setConfig(null)
      setLoading(true)
      localStorage.clear()
      vi.mocked(api.getConfig).mockReset()
    })

    afterEach(() => {
      cleanup()
    })

    it('通用标签页保留 GeneralTab 备份区,页脚不再常驻导出/导入按钮', async () => {
      vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
      renderSettingsPanel()

      await waitFor(() => {
        expect(screen.queryByText('加载配置中...')).toBeNull()
      })

      // GeneralTab 备份区为唯一入口(带描述文案,功能更完整)
      expect(screen.getByText('导出配置与任务')).toBeDefined()
      expect(screen.getByText('导入配置与任务')).toBeDefined()
      // 页脚常驻入口已移除(文案与备份区不同,精确匹配不会误伤)
      expect(screen.queryByText('导出配置')).toBeNull()
      expect(screen.queryByText('导入配置')).toBeNull()
    })

    it('切换到其他标签页后看不到任何备份按钮', async () => {
      vi.mocked(api.getConfig).mockResolvedValue(mockConfig)
      renderSettingsPanel()

      await waitFor(() => {
        expect(screen.queryByText('加载配置中...')).toBeNull()
      })

      fireEvent.click(screen.getByText('下载'))

      // 页脚入口与 GeneralTab 入口均不可见
      expect(screen.queryByText('导出配置')).toBeNull()
      expect(screen.queryByText('导入配置')).toBeNull()
      expect(screen.queryByText('导出配置与任务')).toBeNull()
      expect(screen.queryByText('导入配置与任务')).toBeNull()
    })
  })
})
