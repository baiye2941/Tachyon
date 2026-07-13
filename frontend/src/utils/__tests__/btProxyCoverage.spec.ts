import { describe, it, expect } from 'vitest'
import { computeBtProxyCoverage, type ProxyCoverage } from '../btProxyCoverage'
import type { MagnetConfig } from '../../types'

const baseConfig = (overrides: Partial<MagnetConfig> = {}): MagnetConfig => ({
  enableDht: false,
  enableUpnp: false,
  disableDhtPersistence: false,
  disableDhtWhenSocks: true,
  peerAddrs: [],
  socksProxyUrl: null,
  trackers: [],
  ...overrides,
}) as MagnetConfig

describe('computeBtProxyCoverage', () => {
  it('无 SOCKS 时所有流量直连(UPnP 关闭为 Disabled)', () => {
    const r = computeBtProxyCoverage(baseConfig({ enableDht: true, enableUpnp: false }))
    expect(r.socksEnabled).toBe(false)
    expect(r.peerTcp).toBe<ProxyCoverage>('Direct')
    expect(r.httpTracker).toBe<ProxyCoverage>('Direct')
    expect(r.udpTrackerDht).toBe<ProxyCoverage>('Direct')
    expect(r.utp).toBe<ProxyCoverage>('Direct')
    expect(r.upnp).toBe<ProxyCoverage>('Disabled')
  })

  it('无 SOCKS 且 UPnP 开启时 UPnP 为 Direct', () => {
    const r = computeBtProxyCoverage(baseConfig({ enableUpnp: true }))
    expect(r.upnp).toBe<ProxyCoverage>('Direct')
  })

  it('SOCKS 启用 + disableDhtWhenSocks 时 peer/HTTP 经代理,UDP/DHT 被阻断,uTP/UPnP 可能绕过', () => {
    const r = computeBtProxyCoverage(
      baseConfig({
        socksProxyUrl: 'socks5://127.0.0.1:1080',
        disableDhtWhenSocks: true,
        enableUpnp: true,
      }),
    )
    expect(r.socksEnabled).toBe(true)
    expect(r.peerTcp).toBe<ProxyCoverage>('ViaProxy')
    expect(r.httpTracker).toBe<ProxyCoverage>('ViaProxy')
    expect(r.udpTrackerDht).toBe<ProxyCoverage>('Blocked')
    expect(r.utp).toBe<ProxyCoverage>('MayBypass')
    expect(r.upnp).toBe<ProxyCoverage>('MayBypass')
  })

  it('SOCKS 启用但未禁用 DHT 时 UDP/DHT 可能绕过', () => {
    const r = computeBtProxyCoverage(
      baseConfig({
        socksProxyUrl: 'socks5://127.0.0.1:1080',
        disableDhtWhenSocks: false,
        enableDht: true,
      }),
    )
    expect(r.udpTrackerDht).toBe<ProxyCoverage>('MayBypass')
  })

  it('SOCKS 启用且 UPnP 关闭时 UPnP 为 Disabled', () => {
    const r = computeBtProxyCoverage(
      baseConfig({ socksProxyUrl: 'socks5://127.0.0.1:1080', enableUpnp: false }),
    )
    expect(r.upnp).toBe<ProxyCoverage>('Disabled')
  })
})
