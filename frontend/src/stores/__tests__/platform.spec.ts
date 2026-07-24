import { describe, it, expect, afterEach } from 'vitest'
import { isMacPlatform } from '../shortcuts'

describe('isMacPlatform (E-08)', () => {
  const originalPlatform = Object.getOwnPropertyDescriptor(window.navigator, 'platform')
  const originalUA = Object.getOwnPropertyDescriptor(window.navigator, 'userAgent')
  const originalUAData = Object.getOwnPropertyDescriptor(window.navigator, 'userAgentData')

  afterEach(() => {
    if (originalPlatform) Object.defineProperty(window.navigator, 'platform', originalPlatform)
    if (originalUA) Object.defineProperty(window.navigator, 'userAgent', originalUA)
    if (originalUAData) {
      Object.defineProperty(window.navigator, 'userAgentData', originalUAData)
    } else {
      // remove synthetic property if we added one
      try {
        Reflect.deleteProperty(window.navigator as object, 'userAgentData')
      } catch {
        /* ignore */
      }
    }
  })

  it('优先使用 userAgentData.platform 判断 macOS', () => {
    Object.defineProperty(window.navigator, 'userAgentData', {
      value: { platform: 'macOS' },
      configurable: true,
    })
    // 即使旧 platform 不是 Mac, 也应按 userAgentData 判定
    Object.defineProperty(window.navigator, 'platform', {
      value: 'Win32',
      configurable: true,
    })
    expect(isMacPlatform()).toBe(true)
  })

  it('无 userAgentData 时回退 userAgent 判断 Apple 平台', () => {
    Object.defineProperty(window.navigator, 'userAgentData', {
      value: undefined,
      configurable: true,
    })
    Object.defineProperty(window.navigator, 'userAgent', {
      value:
        'Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 (KHTML, like Gecko)',
      configurable: true,
    })
    Object.defineProperty(window.navigator, 'platform', {
      value: 'Win32', // 故意污染旧 API
      configurable: true,
    })
    expect(isMacPlatform()).toBe(true)
  })

  it('非 Apple 平台返回 false', () => {
    Object.defineProperty(window.navigator, 'userAgentData', {
      value: { platform: 'Windows' },
      configurable: true,
    })
    Object.defineProperty(window.navigator, 'userAgent', {
      value: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      configurable: true,
    })
    expect(isMacPlatform()).toBe(false)
  })
})
