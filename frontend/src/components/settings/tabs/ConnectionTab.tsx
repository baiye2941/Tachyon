import type { SetStoreFunction } from "solid-js/store";
import { createResource, Show } from "solid-js";
import { tr } from "../../../i18n";
import { api } from "../../../api/invoke";
import SliderItem from "../items/SliderItem";
import ToggleItem from "../items/ToggleItem";
import type { ConfigDraft } from "../SettingsPanel";

export interface ConnectionTabProps {
  draft: ConfigDraft;
  setDraft: SetStoreFunction<ConfigDraft>;
}

export default function ConnectionTab(props: ConnectionTabProps) {
  const t = tr;
  // 审计 HTTP-10:QUIC 能力可见性——读取编译期能力,提示用户降级
  const [quicCap] = createResource(() => api.getQuicCapability().catch(() => null));
  return (
    <div class="flex flex-col gap-5">
      <SliderItem
        label={t("settings.connection.maxConnectionsPerHost")}
        value={props.draft.connection.maxConnectionsPerHost}
        min={1}
        max={16}
        onChange={(v) =>
          props.setDraft("connection", "maxConnectionsPerHost", v)
        }
        displayValue={`${props.draft.connection.maxConnectionsPerHost}`}
      />
      <SliderItem
        label={t("settings.connection.connectTimeout")}
        value={props.draft.connection.connectTimeoutSecs}
        min={5}
        max={120}
        onChange={(v) =>
          props.setDraft("connection", "connectTimeoutSecs", v)
        }
        displayValue={t("settings.connection.connectTimeoutValue", { n: props.draft.connection.connectTimeoutSecs })}
      />
      <SliderItem
        label={t("settings.connection.maxGlobalConnections")}
        value={props.draft.connection.maxGlobalConnections}
        min={1}
        max={256}
        onChange={(v) =>
          props.setDraft("connection", "maxGlobalConnections", v)
        }
        displayValue={`${props.draft.connection.maxGlobalConnections}`}
      />
      <SliderItem
        label={t("settings.connection.keepAliveTimeout")}
        value={props.draft.connection.keepAliveTimeoutSecs}
        min={1}
        max={120}
        onChange={(v) =>
          props.setDraft("connection", "keepAliveTimeoutSecs", v)
        }
        displayValue={t("settings.connection.keepAliveTimeoutValue", {
          n: props.draft.connection.keepAliveTimeoutSecs,
        })}
      />
      <ToggleItem
        label={t("settings.connection.enableHttp2")}
        value={props.draft.connection.enableHttp2}
        onChange={(v) => props.setDraft("connection", "enableHttp2", v)}
      />
      <ToggleItem
        label={t("settings.connection.enableQuic")}
        value={props.draft.connection.enableQuic}
        onChange={(v) => props.setDraft("connection", "enableQuic", v)}
      />
      {/* 审计 HTTP-10:enable_quic=true 但未编译 http3 时提示降级 */}
      <Show when={quicCap()}>{(cap) => (
        <Show when={cap().enableQuic && !cap().effectiveQuic}>
          <div style={{ "font-size": "11px", color: "var(--color-warning, #f59e0b)", "margin-top": "-8px" }}>
            {t("settings.connection.quicNotCompiled")}
          </div>
        </Show>
      )}</Show>
    </div>
  );
}
