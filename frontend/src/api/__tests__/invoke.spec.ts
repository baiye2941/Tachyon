import { describe, it, expect } from 'vitest'
import { isLocalPath } from '../invoke'

describe('isLocalPath (F-02 shell.open 防御)', () => {
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
