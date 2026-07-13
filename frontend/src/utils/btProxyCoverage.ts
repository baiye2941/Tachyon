import type { MagnetConfig, ProxyCoverage } from "../types"

/** FIX-16 所需的最小配置子集(避免与 ConfigDraft.magnet 字段全集耦合) */
export interface BtProxyCoverageInput {
  socksProxyUrl: string | null
  disableDhtWhenSocks: boolean
  enableUpnp: boolean
}

export type { ProxyCoverage }

/**
 * FIX-16:前端纯函数,镜像后端 `bt_proxy_coverage_status`,根据 MagnetConfig 计算
 * BT 各流量类别相对 SOCKS 代理的覆盖状态(隐私可见性)。
 *
 * 审计指出:应用侧已注入 socks_proxy_url、过滤 UDP tracker、禁用 DHT,但 librqbit
 * 内部对 peer TCP / HTTP(S) tracker / UDP tracker / DHT / uTP / UPnP 各路径是否走
 * SOCKS 不可从应用代码证明。本函数在 UI 层展示隐私边界,让用户知晓哪些流量经代理、
 * 哪些可能绕过(uTP/UPnP 基于 UDP/局域网,SOCKS5 不代理)。
 *
 * 与后端 `tachyon_engine::bt_proxy_coverage_status` 逻辑保持一致。
 */
export interface BtProxyCoverageReport {
  socksEnabled: boolean
  peerTcp: ProxyCoverage
  httpTracker: ProxyCoverage
  udpTrackerDht: ProxyCoverage
  utp: ProxyCoverage
  upnp: ProxyCoverage
}

export function computeBtProxyCoverage(config: BtProxyCoverageInput): BtProxyCoverageReport {
  const socksEnabled = config.socksProxyUrl != null && config.socksProxyUrl !== ""
  const upnp = config.enableUpnp
    ? socksEnabled
      ? ("MayBypass" as ProxyCoverage)
      : ("Direct" as ProxyCoverage)
    : ("Disabled" as ProxyCoverage)

  if (!socksEnabled) {
    return {
      socksEnabled: false,
      peerTcp: "Direct",
      httpTracker: "Direct",
      udpTrackerDht: "Direct",
      utp: "Direct",
      upnp,
    }
  }

  // SOCKS 启用:peer TCP / HTTP tracker 经 socks_proxy_url;UDP/DHT 看 disableDhtWhenSocks;
  // uTP 基于 UDP(SOCKS5 不代理 UDP)-> MayBypass;UPnP 局域网 -> MayBypass/Disabled
  return {
    socksEnabled: true,
    peerTcp: "ViaProxy",
    httpTracker: "ViaProxy",
    udpTrackerDht: config.disableDhtWhenSocks ? "Blocked" : "MayBypass",
    utp: "MayBypass",
    upnp,
  }
}

// 保留 MagnetConfig 重导出以兼容调用方(完整配置也满足 BtProxyCoverageInput 结构)
export type { MagnetConfig }
