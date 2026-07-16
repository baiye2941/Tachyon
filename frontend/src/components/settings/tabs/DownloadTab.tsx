import type { SetStoreFunction } from "solid-js/store";
import { For } from "solid-js";
import { tr } from "../../../i18n";
import SliderItem from "../items/SliderItem";
import ToggleItem from "../items/ToggleItem";
import type { ConfigDraft } from "../SettingsPanel";
import type { IoStrategy } from "../../../types";

export interface DownloadTabProps {
  draft: ConfigDraft;
  setDraft: SetStoreFunction<ConfigDraft>;
}

const IO_STRATEGIES: Array<{ value: IoStrategy; labelKey: string }> = [
  { value: "standard", labelKey: "settings.download.ioStrategyStandard" },
  { value: "winAligned", labelKey: "settings.download.ioStrategyWinAligned" },
  { value: "iocp", labelKey: "settings.download.ioStrategyIocp" },
  { value: "ioUring", labelKey: "settings.download.ioStrategyIoUring" },
];

export default function DownloadTab(props: DownloadTabProps) {
  const t = tr;
  return (
    <div class="flex flex-col gap-5">
      <SliderItem
        label={t("settings.download.maxConcurrentTasks")}
        value={props.draft.maxConcurrentTasks}
        min={1}
        max={16}
        onChange={(v) => props.setDraft("maxConcurrentTasks", v)}
        displayValue={`${props.draft.maxConcurrentTasks}`}
      />
      <SliderItem
        label={t("settings.download.maxConcurrentFragments")}
        value={props.draft.download.maxConcurrentFragments}
        min={1}
        max={32}
        onChange={(v) =>
          props.setDraft("download", "maxConcurrentFragments", v)
        }
        displayValue={`${props.draft.download.maxConcurrentFragments}`}
      />
      <SliderItem
        label={t("settings.download.maxRetries")}
        value={props.draft.download.maxRetries}
        min={0}
        max={10}
        onChange={(v) => props.setDraft("download", "maxRetries", v)}
        displayValue={t("settings.download.maxRetriesValue", { n: props.draft.download.maxRetries })}
      />
      <SliderItem
        label={t("settings.download.requestTimeout")}
        value={props.draft.download.requestTimeoutSecs}
        min={5}
        max={120}
        onChange={(v) =>
          props.setDraft("download", "requestTimeoutSecs", v)
        }
        displayValue={t("settings.download.requestTimeoutValue", {
          n: props.draft.download.requestTimeoutSecs,
        })}
      />
      <div class="flex items-center justify-between">
        <span
          style={{
            "font-size": "13px",
            color: "var(--color-text-secondary)",
          }}
        >
          {t("settings.download.rateLimit")}
        </span>
        <div class="flex items-center gap-2">
          <input
            type="number"
            min={0}
            step={1048576}
            class="input"
            style={{ width: "120px", "font-size": "13px" }}
            placeholder={t("settings.download.rateLimitPlaceholder")}
            value={
              props.draft.download.rateLimitBytesPerSec ?? ""
            }
            onInput={(e) => {
              const raw = e.currentTarget.value.trim();
              if (raw === "") {
                props.setDraft("download", "rateLimitBytesPerSec", null);
              } else {
                const n = Number(raw);
                props.setDraft(
                  "download",
                  "rateLimitBytesPerSec",
                  Number.isFinite(n) && n > 0 ? Math.floor(n) : null,
                );
              }
            }}
          />
          <span
            class="mono"
            style={{
              "font-size": "11px",
              color: "var(--color-text-tertiary)",
              "white-space": "nowrap",
            }}
          >
            {t("settings.download.rateLimitUnit")}
          </span>
        </div>
      </div>
      <div
        style={{
          "font-size": "11px",
          color: "var(--color-text-tertiary)",
          "margin-top": "-12px",
        }}
      >
        {t("settings.download.rateLimitHint")}
      </div>

      {/* 审计:HTTP 下载显式代理 */}
      <div>
        <span
          style={{
            "font-size": "13px",
            color: "var(--color-text-title)",
          }}
        >
          {t("settings.download.proxy")}
        </span>
        <input
          type="text"
          class="input"
          style={{
            width: "100%",
            "font-size": "13px",
            "margin-top": "4px",
          }}
          placeholder={t("settings.download.proxyPlaceholder")}
          value={props.draft.download.proxy ?? ""}
          onInput={(e) => {
            const raw = e.currentTarget.value.trim();
            props.setDraft("download", "proxy", raw === "" ? null : raw);
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
          {t("settings.download.proxyHint")}
        </span>
      </div>

      {/* 审计 BT-19:I/O 后端策略 */}
      <div>
        <span
          style={{
            "font-size": "13px",
            color: "var(--color-text-title)",
          }}
        >
          {t("settings.download.ioStrategy")}
        </span>
        <select
          class="input"
          style={{
            width: "100%",
            "font-size": "13px",
            "margin-top": "4px",
          }}
          value={props.draft.download.ioStrategy}
          onChange={(e) => {
            props.setDraft(
              "download",
              "ioStrategy",
              e.currentTarget.value as IoStrategy,
            );
          }}
        >
          <For each={IO_STRATEGIES}>
            {(opt) => (
              <option value={opt.value}>{t(opt.labelKey as never)}</option>
            )}
          </For>
        </select>
        <span
          style={{
            "font-size": "11px",
            color: "var(--color-text-secondary)",
            "margin-top": "2px",
            display: "block",
          }}
        >
          {t("settings.download.ioStrategyHint")}
        </span>
      </div>

      <ToggleItem
        label={t("settings.download.verifyChecksum")}
        value={props.draft.download.verifyChecksum}
        onChange={(v) => props.setDraft("download", "verifyChecksum", v)}
      />
    </div>
  );
}
