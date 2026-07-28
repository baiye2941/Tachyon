import type { SetStoreFunction } from "solid-js/store";
import { For, Show, createMemo, createSignal } from "solid-js";
import { tr } from "../../../i18n";
import { addToast } from "../../../stores/toast";
import { api } from "../../../api/invoke";
import NumberInput from "../items/NumberInput";
import SectionLabel from "../items/SectionLabel";
import ToggleItem from "../items/ToggleItem";
import TrackerList from "../items/TrackerList";
import { PRESET_TRACKERS } from "../constants";
import { computeBtProxyCoverage, type BtProxyCoverageReport } from "../../../utils/btProxyCoverage";
import type { ProxyCoverage, SocksProxySource } from "../../../types";
import type { ConfigDraft } from "../SettingsPanel";
import { getBtProxyCoverageResource } from "../../../stores/btProxyCoverageCache";
import { errorMessage } from "../../../utils/appError";
import Button from "../../../shared/ui/Button";

export interface MagnetTabProps {
  draft: ConfigDraft;
  setDraft: SetStoreFunction<ConfigDraft>;
}

export default function MagnetTab(props: MagnetTabProps) {
  const t = tr;
  const [refreshing, setRefreshing] = createSignal(false);

  const refreshSubscription = async () => {
    if (!props.draft.magnet.trackerSubscriptionEnabled) {
      addToast(t("settings.magnet.subscriptionDisabledToast"), "error");
      return;
    }
    const url = props.draft.magnet.trackerSubscriptionUrl.trim();
    if (!url) {
      addToast(t("settings.magnet.subscriptionUrlEmpty"), "error");
      return;
    }
    // 先把开关/URL/用户列表写入配置,再拉取(命令读后端配置)
    setRefreshing(true);
    try {
      await api.updateConfig({
        magnet: {
          trackerSubscriptionEnabled: true,
          trackerSubscriptionUrl: url,
          // 把当前列表固化为用户列表,避免被订阅覆盖
          trackerSubscriptionUserTrackers: [...props.draft.magnet.trackers],
        },
      });
      const result = await api.refreshTrackerSubscription();
      // 重新拉全量配置,把合并后的 trackers 同步进草稿列表(用户可见)
      const fresh = await api.getConfig();
      props.setDraft("magnet", "trackers", fresh.magnet.trackers ?? []);
      props.setDraft(
        "magnet",
        "trackerSubscriptionLastUpdated",
        fresh.magnet.trackerSubscriptionLastUpdated ?? result.lastUpdated,
      );
      props.setDraft(
        "magnet",
        "trackerSubscriptionUrl",
        fresh.magnet.trackerSubscriptionUrl ?? url,
      );
      addToast(
        t("settings.magnet.subscriptionRefreshSuccess", {
          n: result.trackersCount,
          s: result.subscribedCount,
        }),
        "success",
      );
    } catch (e) {
      addToast(
        t("settings.magnet.subscriptionRefreshFailed", { error: errorMessage(e) }),
        "error",
      );
    } finally {
      setRefreshing(false);
    }
  };

  const presetUrls = [
    {
      id: "best",
      label: t("settings.magnet.subscriptionPreset.best"),
      url: "https://cf.trackerslist.com/best.txt",
    },
    {
      id: "http",
      label: t("settings.magnet.subscriptionPreset.http"),
      url: "https://cf.trackerslist.com/http.txt",
    },
    {
      id: "all",
      label: t("settings.magnet.subscriptionPreset.all"),
      url: "https://cf.trackerslist.com/all.txt",
    },
  ] as const;
  return (
    <div class="flex flex-col gap-5">
      <ToggleItem
        label={t("settings.magnet.enableDht")}
        value={props.draft.magnet.enableDht}
        onChange={(v) => props.setDraft("magnet", "enableDht", v)}
      />
      <ToggleItem
        label={t("settings.magnet.enableUpnp")}
        value={props.draft.magnet.enableUpnp}
        onChange={(v) => props.setDraft("magnet", "enableUpnp", v)}
      />
      <ToggleItem
        label={t("settings.magnet.disableDhtPersistence")}
        value={props.draft.magnet.disableDhtPersistence}
        onChange={(v) => props.setDraft("magnet", "disableDhtPersistence", v)}
      />
      <ToggleItem
        label={t("settings.magnet.allowPrivatePeers")}
        value={props.draft.magnet.allowPrivatePeers}
        onChange={(v) => props.setDraft("magnet", "allowPrivatePeers", v)}
      />
      <div
        style={{
          "font-size": "11px",
          color: "var(--color-text-tertiary)",
          "margin-top": "-12px",
          "line-height": "1.5",
        }}
      >
        {t("settings.magnet.allowPrivatePeersHint")}
      </div>
      <div>
        <span
          style={{
            "font-size": "13px",
            color: "var(--color-text-title)",
          }}
        >
          {t("settings.magnet.socksProxyUrl")}
        </span>
        <input
          type="text"
          class="input"
          style={{
            width: "100%",
            "font-size": "13px",
            "margin-top": "4px",
          }}
          placeholder={t("settings.magnet.socksProxyUrlPlaceholder")}
          value={props.draft.magnet.socksProxyUrl ?? ""}
          onInput={(e) => {
            const raw = e.currentTarget.value.trim();
            props.setDraft("magnet", "socksProxyUrl", raw === "" ? null : raw);
          }}
        />
        <span
          style={{
            "font-size": "11px",
            color: "var(--color-text-secondary)",
            "margin-top": "2px",
            display: "block",
          }}
        >
          {t("settings.magnet.socksProxyUrlHint")}
        </span>
        <Show when={(props.draft.magnet.socksProxyUrl ?? "").trim() !== ""}>
          <span
            class="text-xs"
            style={{
              color: "var(--color-accent-primary)",
              "line-height": "1.4",
            }}
          >
            {t("settings.magnet.socksHttpsHint")}
          </span>
        </Show>
      </div>

      {/* FIX-16: BT 代理流量覆盖状态(隐私可见性) —— 展示各流量类别是否经代理/可能绕过 */}
      <BtProxyCoveragePanel draft={props.draft} />

      {/* —— Task 9: Peer 超时配置(需重启生效) —— */}
      <SectionLabel text={t("settings.magnet.sectionPeer")} />
      <NumberInput
        label={t("settings.magnet.peerConnectTimeout")}
        value={props.draft.magnet.peerConnectTimeoutSecs}
        min={1}
        max={300}
        unit={t("time.seconds", { n: props.draft.magnet.peerConnectTimeoutSecs })}
        badge="restart"
        hint={t("settings.magnet.peerConnectTimeoutHint")}
        onChange={(v) => props.setDraft("magnet", "peerConnectTimeoutSecs", v)}
      />
      <NumberInput
        label={t("settings.magnet.peerReadWriteTimeout")}
        value={props.draft.magnet.peerReadWriteTimeoutSecs}
        min={1}
        max={600}
        unit={t("time.seconds", { n: props.draft.magnet.peerReadWriteTimeoutSecs })}
        badge="restart"
        hint={t("settings.magnet.peerReadWriteTimeoutHint")}
        onChange={(v) => props.setDraft("magnet", "peerReadWriteTimeoutSecs", v)}
      />

      {/* —— Task 9: 高级配置(tracker / defer_writes / socks-DHT) —— */}
      <SectionLabel text={t("settings.magnet.sectionAdvanced")} />
      <NumberInput
        label={t("settings.magnet.forceTrackerInterval")}
        value={props.draft.magnet.forceTrackerIntervalSecs}
        min={0}
        max={3600}
        unit={t("time.seconds", { n: props.draft.magnet.forceTrackerIntervalSecs })}
        badge="newTask"
        hint={t("settings.magnet.forceTrackerIntervalHint")}
        onChange={(v) => props.setDraft("magnet", "forceTrackerIntervalSecs", v)}
      />
      <NumberInput
        label={t("settings.magnet.deferWritesUpToMb")}
        value={props.draft.magnet.deferWritesUpToMb}
        min={0}
        max={256}
        unit="MB"
        badge="restart"
        hint={t("settings.magnet.deferWritesUpToMbHint")}
        onChange={(v) => props.setDraft("magnet", "deferWritesUpToMb", v)}
      />
      <ToggleItem
        label={t("settings.magnet.disableDhtWhenSocks")}
        value={props.draft.magnet.disableDhtWhenSocks}
        badge="restart"
        onChange={(v) => props.setDraft("magnet", "disableDhtWhenSocks", v)}
      />
      <div
        style={{
          "font-size": "11px",
          color: "var(--color-text-tertiary)",
          "margin-top": "-12px",
          "line-height": "1.5",
        }}
      >
        {t("settings.magnet.disableDhtWhenSocksHint")}
      </div>


      <SectionLabel text={t("settings.magnet.sectionSubscription")} />
      <ToggleItem
        label={t("settings.magnet.subscriptionEnabled")}
        value={props.draft.magnet.trackerSubscriptionEnabled}
        onChange={(v) => props.setDraft("magnet", "trackerSubscriptionEnabled", v)}
      />
      {/* 订阅关闭时折叠为一行摘要,打开时完整卡片淡入 */}
      <Show
        when={props.draft.magnet.trackerSubscriptionEnabled}
        fallback={
          <div class="sub-collapsed">
            {t("settings.magnet.subscriptionCollapsed", {
              n: props.draft.magnet.trackers.length,
            })}
          </div>
        }
      >
        <div class="sub-card">
          <div class="flex flex-col gap-1">
            <span class="sub-card-title">
              {t("settings.magnet.subscriptionUrl")}
            </span>
            <span class="sub-card-hint">
              {t("settings.magnet.subscriptionUrlHint")}
            </span>
          </div>

          <div class="flex flex-wrap gap-1.5">
            <For each={[...presetUrls]}>
              {(p) => {
                const active = () =>
                  props.draft.magnet.trackerSubscriptionUrl === p.url;
                return (
                  <button
                    type="button"
                    class="chip"
                    classList={{ "chip--active": active() }}
                    onClick={() =>
                      props.setDraft("magnet", "trackerSubscriptionUrl", p.url)
                    }
                  >
                    {p.label}
                  </button>
                );
              }}
            </For>
          </div>

          <input
            type="url"
            class="input sub-url-input"
            value={props.draft.magnet.trackerSubscriptionUrl}
            onInput={(e) =>
              props.setDraft(
                "magnet",
                "trackerSubscriptionUrl",
                e.currentTarget.value,
              )
            }
            onBlur={(e) =>
              props.setDraft(
                "magnet",
                "trackerSubscriptionUrl",
                e.currentTarget.value.trim(),
              )
            }
            placeholder="https://cf.trackerslist.com/best.txt"
            spellcheck={false}
            autocomplete="off"
          />

          <div class="sub-card-foot">
            <Show
              when={props.draft.magnet.trackerSubscriptionLastUpdated}
              fallback={
                <span class="sub-meta sub-meta--dim">
                  {t("settings.magnet.subscriptionNeverUpdated")}
                </span>
              }
            >
              {(ts) => (
                <span class="sub-meta">
                  {t("settings.magnet.subscriptionLastUpdated", { t: ts() })}
                  {" · "}
                  {t("settings.magnet.subscriptionListCount", {
                    n: props.draft.magnet.trackers.length,
                  })}
                </span>
              )}
            </Show>
            <Button
              variant="primary"
              size="sm"
              loading={refreshing()}
              onClick={() => void refreshSubscription()}
            >
              {refreshing()
                ? t("settings.magnet.subscriptionRefreshing")
                : t("settings.magnet.subscriptionRefresh")}
            </Button>
          </div>

          <span class="sub-sync-hint">
            {t("settings.magnet.subscriptionSyncHint")}
          </span>
        </div>
      </Show>
      <TrackerList
        trackers={props.draft.magnet.trackers}
        onAdd={(url) => {
          props.setDraft("magnet", "trackers", [...props.draft.magnet.trackers, url]);
        }}
        onRemove={(index) => {
          const updated = props.draft.magnet.trackers.filter((_, i) => i !== index);
          props.setDraft("magnet", "trackers", updated);
        }}
        onImportPresets={() => {
          const existing = new Set(props.draft.magnet.trackers);
          const newTrackers = PRESET_TRACKERS.filter((t) => !existing.has(t));
          props.setDraft("magnet", "trackers", [...props.draft.magnet.trackers, ...newTrackers]);
          addToast(
            tr("settings.magnet.importSuccess", { n: newTrackers.length }),
            "success"
          );
        }}
        onClearAll={() => {
          props.setDraft("magnet", "trackers", []);
        }}
      />
    </div>
  );
}

/// 审计 A-09:BT 代理流量覆盖面板。优先展示后端 Session 运行时 effective SOCKS;
/// draft 仅作未应用预测,避免环境代理实际生效时 UI 隐藏面板。
/// 闪烁修复:运行时报告用应用级缓存 resource,tab 重挂载不重新 fetch,
/// 避免 pending(隐藏) -> resolved(显示) 的单帧 DOM 闪烁。
function BtProxyCoveragePanel(props: { draft: ConfigDraft }) {
  const t = tr;
  const runtime = getBtProxyCoverageResource();

  const draftReport = createMemo((): BtProxyCoverageReport =>
    computeBtProxyCoverage(props.draft.magnet),
  );

  const report = createMemo((): BtProxyCoverageReport => {
    const rt = runtime();
    if (rt && rt.socksEnabled) return rt;
    // 运行时未启用 SOCKS 时,仍展示 draft 预测(用户编辑中的待应用配置)
    return draftReport();
  });

  const isDraftOnly = createMemo(() => {
    const rt = runtime();
    const d = draftReport();
    // 草稿有 SOCKS 但运行时无/未加载 → 待应用
    if (d.socksEnabled && !(rt && rt.socksEnabled)) return true;
    // 草稿 endpoint 与运行时不同 → 待应用提示
    if (rt && rt.socksEnabled && d.socksEnabled) {
      const draftUrl = (props.draft.magnet.socksProxyUrl ?? "").trim();
      // 仅显式 draft URL 与 runtime 来源比较:环境来源时 draft 为空不标 pending
      if (draftUrl !== "" && rt.socksSource === "environment") return true;
    }
    return false;
  });

  const sourceLabel = (s?: SocksProxySource): string => {
    switch (s) {
      case "explicit":
        return t("settings.magnet.coverageSourceExplicit");
      case "environment":
        return t("settings.magnet.coverageSourceEnvironment");
      default:
        return t("settings.magnet.coverageSourceNone");
    }
  };

  const rows = (): Array<{ label: string; status: ProxyCoverage }> => [
    { label: t("settings.magnet.coveragePeerTcp"), status: report().peerTcp },
    { label: t("settings.magnet.coverageHttpTracker"), status: report().httpTracker },
    { label: t("settings.magnet.coverageUdpTrackerDht"), status: report().udpTrackerDht },
    { label: t("settings.magnet.coverageUtp"), status: report().utp },
    { label: t("settings.magnet.coverageUpnp"), status: report().upnp },
  ];

  const statusColor = (s: ProxyCoverage): string => {
    switch (s) {
      case "ViaProxy": return "var(--color-success)";
      case "Blocked":
      case "Disabled": return "var(--color-text-secondary)";
      case "MayBypass": return "var(--color-warning)";
      default: return "var(--color-text-secondary)";
    }
  };

  const statusText = (s: ProxyCoverage): string => {
    switch (s) {
      case "Direct": return t("settings.magnet.coverageDirect");
      case "ViaProxy": return t("settings.magnet.coverageViaProxy");
      case "Blocked": return t("settings.magnet.coverageBlocked");
      case "Disabled": return t("settings.magnet.coverageDisabled");
      case "MayBypass": return t("settings.magnet.coverageMayBypass");
    }
  };

  return (
    <Show when={report().socksEnabled}>
      <div
        style={{
          "margin-top": "6px",
          padding: "8px 10px",
          "border-radius": "6px",
          "border": "1px solid var(--color-border-default)",
          background: "var(--color-bg-secondary)",
        }}
      >
        <div style={{ "font-size": "12px", "font-weight": 600, "margin-bottom": "4px" }}>
          {t("settings.magnet.coverageTitle")}
        </div>
        <Show when={report().socksSource || report().socksEndpointRedacted}>
          <div
            style={{
              "font-size": "11px",
              color: "var(--color-text-secondary)",
              "margin-bottom": "4px",
            }}
          >
            {t("settings.magnet.coverageSource")}: {sourceLabel(report().socksSource)}
            <Show when={report().socksEndpointRedacted}>
              {(ep) => <> · {ep()}</>}
            </Show>
          </div>
        </Show>
        <Show when={isDraftOnly()}>
          <div
            style={{
              "font-size": "11px",
              color: "var(--color-warning)",
              "margin-bottom": "4px",
            }}
          >
            {t("settings.magnet.coveragePendingApply")}
          </div>
        </Show>
        <For each={rows()}>
          {(row) => (
            <div
              style={{
                display: "flex",
                "justify-content": "space-between",
                "font-size": "11px",
                "line-height": "1.7",
              }}
            >
              <span>{row.label}</span>
              <span style={{ color: statusColor(row.status) }}>{statusText(row.status)}</span>
            </div>
          )}
        </For>
        <div
          style={{
            "font-size": "10px",
            color: "var(--color-text-tertiary)",
            "margin-top": "4px",
            "line-height": "1.4",
          }}
        >
          {t("settings.magnet.coverageHint")}
        </div>
      </div>
    </Show>
  );
}
