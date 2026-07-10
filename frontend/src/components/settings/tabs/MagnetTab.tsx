import type { SetStoreFunction } from "solid-js/store";
import { tr } from "../../../i18n";
import { addToast } from "../../../stores/toast";
import NumberInput from "../items/NumberInput";
import SectionLabel from "../items/SectionLabel";
import ToggleItem from "../items/ToggleItem";
import TrackerList from "../items/TrackerList";
import { PRESET_TRACKERS } from "../constants";
import type { ConfigDraft } from "../SettingsPanel";

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
