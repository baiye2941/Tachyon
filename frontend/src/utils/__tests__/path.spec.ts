import { describe, it, expect } from 'vitest'
import { getParentDirectory } from '../path'

describe('getParentDirectory', () => {
  it('Windows 文件路径返回所在目录', () => {
    expect(getParentDirectory('C:\\Users\\foo\\bar.zip')).toBe('C:\\Users\\foo')
  })

  it('POSIX 文件路径返回所在目录', () => {
    expect(getParentDirectory('/home/foo/bar.zip')).toBe('/home/foo')
  })

  it('Windows 目录路径（带末尾分隔符）返回自身', () => {
    expect(getParentDirectory('C:\\Users\\foo\\')).toBe('C:\\Users\\foo')
  })

  it('POSIX 目录路径（带末尾分隔符）返回自身', () => {
    expect(getParentDirectory('/home/foo/')).toBe('/home/foo')
  })

  it('相对文件路径返回当前目录', () => {
    expect(getParentDirectory('bar.zip')).toBe('.')
  })

  it('空路径返回空字符串', () => {
    expect(getParentDirectory('')).toBe('')
  })

  it('根路径返回自身', () => {
    expect(getParentDirectory('/')).toBe('/')
  })

  it('Windows 盘符根路径返回自身', () => {
    expect(getParentDirectory('C:\\')).toBe('C:\\')
  })

  it('Windows 盘符下文件返回盘符根', () => {
    expect(getParentDirectory('C:\\file.txt')).toBe('C:\\')
  })

  it('带 ./ 的相对路径返回当前目录', () => {
    expect(getParentDirectory('./bar.zip')).toBe('.')
  })

  it('POSIX 绝对路径下无扩展名文件返回所在目录', () => {
    expect(getParentDirectory('/home/foo/bar')).toBe('/home/foo')
  })

  it('仅含文件名的多级相对路径返回父目录', () => {
    expect(getParentDirectory('a/b/c.zip')).toBe('a/b')
  })

  it('UNC 共享根目录返回自身', () => {
    expect(getParentDirectory('\\\\server\\share')).toBe('\\\\server\\share')
  })

  it('UNC 共享根目录（带末尾分隔符）返回自身（去除分隔符）', () => {
    expect(getParentDirectory('\\\\server\\share\\')).toBe('\\\\server\\share')
  })

  it('UNC 共享下文件返回共享根', () => {
    expect(getParentDirectory('\\\\server\\share\\file.txt')).toBe('\\\\server\\share')
  })
})
