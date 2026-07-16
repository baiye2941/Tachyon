import { createResource, type Resource } from "solid-js";
import { api } from "../api/invoke";
import type { BtProxyCoverageReport } from "../utils/btProxyCoverage";

/**
 * 审计 A-09 + 闪烁修复:BT 代理覆盖运行时报告的应用级缓存。
 *
 * 此前 `MagnetTab.BtProxyCoveragePanel` 在每次切到磁力 tab 时用 `createResource`
 * 重新 `getBtProxyCoverage`,在 pending(隐藏面板)与 resolved(若运行时 SOCKS 启用
 * 则显示面板)之间产生一帧 DOM 闪烁。改用模块级单例 resource,全应用只取一次,
 * tab 重挂载不再重新 fetch,避免 show/hide 闪烁。
 *
 * Session 生命周期内代理覆盖基本不变(仅 update_config 重建 Session 才变),
 * 因此无需主动 invalidate;若需刷新可在 update_config 成功后调用 `refetchBtProxyCoverage`。
 */
type ResourceActions = {
  refetch: (info?: unknown) => void;
  mutate: (v: BtProxyCoverageReport | null) => void;
};

let _resource: Resource<BtProxyCoverageReport | null> | null = null;
let _actions: ResourceActions | null = null;

function fetcher(): Promise<BtProxyCoverageReport | null> {
  return api
    .getBtProxyCoverage()
    .then((r) => (r ?? null) as BtProxyCoverageReport | null)
    .catch(() => null);
}

/**
 * 获取应用级 BT 代理覆盖 resource。必须在 Solid 组件上下文内首次调用,
 * 之后 tab 重挂载直接复用已缓存 resource,不再重新 fetch。
 */
export function getBtProxyCoverageResource(): Resource<
  BtProxyCoverageReport | null
> {
  if (_resource === null || _actions === null) {
    const [res, actions] = createResource(fetcher, { initialValue: null });
    _resource = res;
    _actions = actions as unknown as ResourceActions;
  }
  return _resource;
}

/** update_config 重建 Session 后可调用以刷新运行时报告 */
export function refetchBtProxyCoverage(): void {
  _actions?.refetch();
}
