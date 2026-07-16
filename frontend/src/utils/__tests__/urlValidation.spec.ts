import { describe, it, expect } from 'vitest'
import { extractSuggestedFileName, validateUrl } from '../urlValidation'

describe('extractSuggestedFileName', () => {
  it('从 URL 路径段提取文件名', () => {
    expect(extractSuggestedFileName('https://example.com/files/model.safetensors'))
      .toBe('model.safetensors')
  })

  it('解码百分号编码的文件名', () => {
    expect(extractSuggestedFileName('https://example.com/%E4%B8%AD%E6%96%87.txt'))
      .toBe('中文.txt')
  })

  it('无路径段时返回 undefined', () => {
    expect(extractSuggestedFileName('https://example.com/'))
      .toBeUndefined()
  })

  it('根路径无文件名时返回 undefined', () => {
    expect(extractSuggestedFileName('https://example.com'))
      .toBeUndefined()
  })

  it('处理带查询参数的 URL', () => {
    expect(extractSuggestedFileName('https://example.com/file.zip?token=abc123'))
      .toBe('file.zip')
  })

  it('无效 URL 返回 undefined', () => {
    expect(extractSuggestedFileName('not-a-url'))
      .toBeUndefined()
  })

  it('处理多级路径', () => {
    expect(extractSuggestedFileName('https://example.com/a/b/c/data.tar.gz'))
      .toBe('data.tar.gz')
  })

  it('从磁力链接 dn= 参数提取文件名', () => {
    expect(extractSuggestedFileName('magnet:?xt=urn:btih:abc123&dn=ubuntu-24.04.iso&tr=udp://tracker.example.com'))
      .toBe('ubuntu-24.04.iso')
  })

  it('磁力链接 dn 含百分号编码时做 UTF-8 解码', () => {
    expect(extractSuggestedFileName('magnet:?xt=urn:btih:abc&dn=%E4%B8%AD%E6%96%87%E7%94%B5%E5%BD%B1.torrent'))
      .toBe('中文电影.torrent')
  })

  it('磁力链接无 dn= 参数时回退到 info hash', () => {
    expect(extractSuggestedFileName('magnet:?xt=urn:btih:abc123&tr=udp://tracker.example.com'))
      .toBe('magnet-abc123')
  })

  it('磁力链接 dn= 为空值时回退到 info hash', () => {
    expect(extractSuggestedFileName('magnet:?xt=urn:btih:WFL25E2HOBS656ZRTF7JX3HWFWVCURZ5&dn=&tr=udp://tracker.example.com'))
      .toBe('magnet-WFL25E2HOBS656ZRTF7JX3HWFWVCURZ5')
  })

  it('空字符串返回 undefined', () => {
    expect(extractSuggestedFileName(''))
      .toBeUndefined()
  })
})

describe('validateUrl', () => {
  it('磁力链接有效', () => {
    expect(validateUrl('magnet:?xt=urn:btih:abc123&dn=test.zip').valid)
      .toBe(true)
    expect(validateUrl('magnet:?xt=urn:btih:abc123&dn=test.zip').protocol)
      .toBe('magnet')
  })

  it('审计 FT-13:ftp 不再标为可下载', () => {
    const r = validateUrl('ftp://example.com/file.zip')
    expect(r.valid).toBe(false)
    expect(r.protocol).toBe('unknown')
    expect(r.hintKey).toBe('url.hint.invalid')
  })

  it('http/https 仍有效', () => {
    expect(validateUrl('https://cdn.example.com/a.bin').valid).toBe(true)
    expect(validateUrl('http://cdn.example.com/a.bin').protocol).toBe('http')
  })
})
