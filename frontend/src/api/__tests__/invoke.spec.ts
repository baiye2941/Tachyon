import { describe, it, expect, vi, beforeEach } from 'vitest'

// 劫持 @tauri-apps/api/core 的动态 import,返回 mock invoke
// vi.mock 会同时拦截静态与动态 import(P1-21 测试)
const invokeMock = vi.fn()
vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}))

import { isLocalPath, api } from '../invoke'

describe('isLocalPath (路径合法性校验)', () => {
  it('接受合法本地绝对路径', () => {
    expect(isLocalPath('C:\\Users\\test\\downloads')).toBe(true)
    expect(isLocalPath('/home/test/downloads')).toBe(true)
    expect(isLocalPath('D:\\downloads\\file.bin')).toBe(true)
  })

  it('接受 UNC 路径', () => {
    expect(isLocalPath('\\\\server\\share\\file.bin')).toBe(true)
  })

  it('接受相对路径', () => {
    expect(isLocalPath('./downloads/file.bin')).toBe(true)
    expect(isLocalPath('downloads')).toBe(true)
  })

  it('拒绝带 scheme 的 URL', () => {
    expect(isLocalPath('https://evil.com/payload')).toBe(false)
    expect(isLocalPath('http://127.0.0.1:8080/')).toBe(false)
    expect(isLocalPath('javascript:alert(1)')).toBe(false)
    expect(isLocalPath('file:///etc/passwd')).toBe(false)
    expect(isLocalPath('ftp://example.com/x')).toBe(false)
  })
})

describe('api.openFolder (P1-21 后端校验)', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue(undefined)
  })

  it('openFolder 调用 open_task_folder 命令并传入 taskId', async () => {
    await api.openFolder('task-abc-123')
    expect(invokeMock).toHaveBeenCalledTimes(1)
    expect(invokeMock).toHaveBeenCalledWith('open_task_folder', { taskId: 'task-abc-123' })
  })

  it('openFolderUnderRoot 调用 open_folder_under_download_root 命令并传入 path', async () => {
    await api.openFolderUnderRoot('D:\\downloads\\subdir')
    expect(invokeMock).toHaveBeenCalledTimes(1)
    expect(invokeMock).toHaveBeenCalledWith('open_folder_under_download_root', { path: 'D:\\downloads\\subdir' })
  })
})
