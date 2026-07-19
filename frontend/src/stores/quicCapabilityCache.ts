import { createResource, type Resource } from "solid-js";
import { api } from "../api/invoke";
import type { QuicCapability } from "../types";

/**
 * QUIC 编译期能力的应用级缓存(与 btProxyCoverageCache 同构)。
 *
 * 此前 ConnectionTab 每次挂载都 createResource 重新 getQuicCapability,
 * pending → resolved 之间内容晚到产生一帧闪动。QUIC 能力由编译期 feature
 * 决定,session 生命周期内不变,模块级单例全应用只取一次;
 * 配合 SettingsPanel 打开时预取,首次切到「连接」tab 数据已就绪。
 */
let _resource: Resource<QuicCapability | null> | null = null;

/**
 * 获取应用级 QUIC 能力 resource。必须在 Solid 组件上下文内首次调用,
 * 之后 tab 重挂载直接复用已缓存 resource,不再重新 fetch。
 */
export function getQuicCapabilityResource(): Resource<QuicCapability | null> {
  if (_resource === null) {
    const [res] = createResource(
      () => api.getQuicCapability().catch(() => null),
      { initialValue: null },
    );
    // eslint-disable-next-line solid/reactivity -- 模块级单例 resource,使用方在调用 getQuicCapabilityResource() 时读取
    _resource = res;
  }
  return _resource;
}
