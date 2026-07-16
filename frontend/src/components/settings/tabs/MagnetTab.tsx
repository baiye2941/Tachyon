import type { SetStoreFunction } from "solid-js/store";
import { For, Show, createMemo } from "solid-js";
import { tr } from "../../../i18n";
import { addToast } from "../../../stores/toast";
import NumberInput from "../items/NumberInput";
import SectionLabel from "../items/SectionLabel";
import ToggleItem from "../items/ToggleItem";
import TrackerList from "../items/TrackerList";
import { PRESET_TRACKERS } from "../constants";
import { computeBtProxyCoverage, type BtProxyCoverageReport } from "../../../utils/btProxyCoverage";
import type { ProxyCoverage, SocksProxySource } from "../../../types";
import type { ConfigDraft } from "../SettingsPanel";
import { getBtProxyCoverageResource } from "../../../stores/btProxyCoverageCache";

export interface MagnetTabProps {
  draft: ConfigDraft;
  setDraft: SetStoreFunction<ConfigDraft>;
}

export default function MagnetTab(props: MagnetTabProps) {
  const t = tr;
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
      case "ViaProxy": return "var(--color-success, #22c55e)";
      case "Blocked":
      case "Disabled": return "var(--color-text-secondary, #888)";
      case "MayBypass": return "var(--color-warning, #f59e0b)";
      default: return "var(--color-text-secondary, #888)";
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
          "border": "1px solid var(--color-border, #333)",
          background: "var(--color-bg-secondary, #1a1a1a)",
        }}
      >
        <div style={{ "font-size": "12px", "font-weight": 600, "margin-bottom": "4px" }}>
          {t("settings.magnet.coverageTitle")}
        </div>
        <Show when={report().socksSource || report().socksEndpointRedacted}>
          <div
            style={{
              "font-size": "11px",
              color: "var(--color-text-secondary, #888)",
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
              color: "var(--color-warning, #f59e0b)",
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
