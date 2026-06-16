import { describe, it, expect, beforeEach, vi } from 'vitest'

const mockListRepoFiles = vi.fn()
const mockGetHfDownloadUrl = vi.fn()
const mockAddToast = vi.fn()

vi.mock('../../api/invoke', () => ({
  api: {
    listRepoFiles: (...args: unknown[]) => mockListRepoFiles(...args),
    getHfDownloadUrl: (...args: unknown[]) => mockGetHfDownloadUrl(...args),
  },
}))

vi.mock('../toast', () => ({
  addToast: (...args: unknown[]) => mockAddToast(...args),
}))

import type { HubFileInfo } from '../../types'

const makeHubFile = (path: string, overrides: Partial<HubFileInfo> = {}): HubFileInfo => ({
  type: 'file',
  path,
  size: 1024,
  ...overrides,
})

let hubModule: typeof import('../hub')

describe('hub store', () => {
  beforeEach(async () => {
    vi.resetModules()
    mockListRepoFiles.mockReset()
    mockGetHfDownloadUrl.mockReset()
    mockAddToast.mockReset()
    hubModule = await import('../hub')
  })

  it('$hub 初始状态: repoFiles 为空, loading 为 false, error 为 null', () => {
    expect(hubModule.$hub.repoFiles()).toEqual([])
    expect(hubModule.$hub.loading()).toBe(false)
    expect(hubModule.$hub.error()).toBeNull()
  })

  it('listRepoFiles 成功时设置 repoFiles', async () => {
    const files = [
      makeHubFile('model.safetensors', { size: 2048 }),
      makeHubFile('config.json', { size: 512 }),
    ]
    mockListRepoFiles.mockResolvedValue(files)

    const result = await hubModule.listRepoFiles('bert-base-uncased', 'main')

    expect(mockListRepoFiles).toHaveBeenCalledWith('bert-base-uncased', 'main')
    expect(result).toEqual(files)
    expect(hubModule.$hub.repoFiles()).toEqual(files)
    expect(hubModule.$hub.loading()).toBe(false)
    expect(hubModule.$hub.error()).toBeNull()
  })

  it('listRepoFiles 失败时设置 error 并调用 addToast', async () => {
    mockListRepoFiles.mockRejectedValue(new Error('网络超时'))

    const result = await hubModule.listRepoFiles('bad-repo')

    expect(result).toEqual([])
    expect(hubModule.$hub.repoFiles()).toEqual([])
    expect(hubModule.$hub.error()).toBe('Error: 网络超时')
    expect(hubModule.$hub.loading()).toBe(false)
    expect(mockAddToast).toHaveBeenCalledWith(
      expect.stringContaining('获取仓库文件列表失败'),
      'error',
    )
  })

  it('listRepoFiles 期间 loading 为 true', async () => {
    let resolveFn!: (value: HubFileInfo[]) => void
    mockListRepoFiles.mockReturnValue(new Promise<HubFileInfo[]>((resolve) => {
      resolveFn = resolve
    }))

    const promise = hubModule.listRepoFiles('some-repo')
    expect(hubModule.$hub.loading()).toBe(true)

    resolveFn([])
    await promise
    expect(hubModule.$hub.loading()).toBe(false)
  })

  it('clearRepoFiles 清空 repoFiles 和 error', async () => {
    const files = [makeHubFile('model.bin')]
    mockListRepoFiles.mockResolvedValue(files)
    await hubModule.listRepoFiles('test-repo')

    expect(hubModule.$hub.repoFiles()).toHaveLength(1)

    hubModule.clearRepoFiles()

    expect(hubModule.$hub.repoFiles()).toEqual([])
    expect(hubModule.$hub.error()).toBeNull()
  })

  it('getHfDownloadUrl 成功时返回下载 URL', async () => {
    const url = 'https://huggingface.co/bert-base/resolve/main/model.bin'
    mockGetHfDownloadUrl.mockResolvedValue(url)

    const result = await hubModule.getHfDownloadUrl('bert-base', 'model.bin', 'main')

    expect(mockGetHfDownloadUrl).toHaveBeenCalledWith('bert-base', 'model.bin', 'main')
    expect(result).toBe(url)
  })

  it('getHfDownloadUrl 失败时返回 null 并调用 addToast', async () => {
    mockGetHfDownloadUrl.mockRejectedValue(new Error('403 Forbidden'))

    const result = await hubModule.getHfDownloadUrl('private-repo', 'model.bin')

    expect(result).toBeNull()
    expect(mockAddToast).toHaveBeenCalledWith(
      expect.stringContaining('获取下载链接失败'),
      'error',
    )
  })
})
