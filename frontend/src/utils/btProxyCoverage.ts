import type { MagnetConfig, ProxyCoverage, BtProxyCoverageReport } from "../types"

/** FIX-16 所需的最小配置子集(避免与 ConfigDraft.magnet 字段全集耦合) */
export interface BtProxyCoverageInput {
  socksProxyUrl: string | null
  disableDhtWhenSocks: boolean
  enableUpnp: boolean
}

export type { ProxyCoverage, BtProxyCoverageReport }

/**
 * FIX-16:前端纯函数,镜像后端 `bt_proxy_coverage_status`,根据 MagnetConfig 计算
 * BT 各流量类别相对 SOCKS 代理的覆盖状态(隐私可见性)。
 *
 * 审计 A-09:此函数仅预测 draft 配置;运行时事实以 `api.getBtProxyCoverage` 为准。
 */
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
      socksSource: "none",
      socksEndpointRedacted: null,
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
    socksSource: "explicit",
    socksEndpointRedacted: null,
  }
}

// 保留 MagnetConfig 重导出以兼容调用方(完整配置也满足 BtProxyCoverageInput 结构)
export type { MagnetConfig }
