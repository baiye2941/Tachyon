import { For } from "solid-js";
import { tr } from "../../../i18n";
import { $config } from "../../../stores/settings";

export interface AboutTabProps {
  appVersion: string;
  protocols: string[];
}

export default function AboutTab(props: AboutTabProps) {
  const t = tr;
  return (
    <>
      <div
        class="flex flex-col items-center gap-3"
        style={{ padding: "32px 20px 24px" }}
      >
        <div
          style={{
            width: "48px",
            height: "48px",
            /* 品牌色:teal 标识块,与速度强调色解耦 */
            background: "var(--color-brand-teal)",
            "border-radius": "12px",
            display: "flex",
            "align-items": "center",
            "justify-content": "center",
            color: "var(--color-text-inverse)",
            "font-family": "var(--font-mono)",
            "font-size": "22px",
            "font-weight": 700,
            "box-shadow": "var(--shadow-sm)",
          }}
        >
          T
        </div>
        <div
          style={{
            "font-size": "18px",
            "font-weight": 600,
            color: "var(--color-text-title)",
          }}
        >
          Tachyon
        </div>
        <div
          class="mono"
          style={{
            "font-size": "12px",
            color: "var(--color-text-tertiary)",
          }}
        >
          {props.appVersion
            ? t("settings.about.versionValue", { v: props.appVersion })
            : t("settings.about.version")}
        </div>
        <div
          style={{
            "font-size": "12px",
            color: "var(--color-text-tertiary)",
            "margin-top": "4px",
            "text-align": "center",
          }}
        >
          {t("settings.about.tagline")}
        </div>

        {/* 支持的协议(spec 1.6) */}
        <div
          class="flex flex-wrap items-center justify-center gap-1.5"
          style={{ "margin-top": "16px", "max-width": "100%" }}
        >
          <For each={props.protocols}>
            {(proto) => (
              <span
                style={{
                  "font-size": "11px",
                  "font-weight": 600,
                  color: "var(--color-brand-teal)",
                  background: "var(--color-brand-teal-soft)",
                  padding: "2px 8px",
                  "border-radius": "9999px",
                  "text-transform": "uppercase",
                  "letter-spacing": "0.3px",
                }}
              >
                {proto}
              </span>
            )}
          </For>
        </div>
      </div>

      {/* 只读安全字段:user_agent / headers(后端白名单故意排除,
          spec 1.2 要求展示但不可编辑,标注受安全策略保护) */}
      <div
        class="flex flex-col gap-3"
        style={{ padding: "0 20px 24px" }}
      >
        <div
          style={{
            "font-size": "11px",
            "font-weight": 600,
            color: "var(--color-text-tertiary)",
            "text-transform": "uppercase",
            "letter-spacing": "0.5px",
            "margin-bottom": "4px",
          }}
        >
          {t("settings.about.securityFields")}
        </div>
        <div
          style={{
            display: "flex",
            "flex-direction": "column",
            gap: "6px",
            padding: "10px 12px",
            "border-radius": "8px",
            background: "var(--color-bg-hover)",
            border: "1px solid var(--color-border-subtle)",
          }}
        >
          <div
            class="flex items-center justify-between"
            style={{ gap: "12px" }}
          >
            <span
              style={{
                "font-size": "12px",
                color: "var(--color-text-secondary)",
              }}
            >
              {t("settings.about.userAgent")}
            </span>
            <span
              class="mono"
              style={{
                "font-size": "12px",
                color: "var(--color-text-tertiary)",
                "text-align": "right",
                "overflow-wrap": "anywhere",
              }}
            >
              {$config.get()?.download.userAgent ?? "---"}
            </span>
          </div>
          <div
            class="flex items-center justify-between"
            style={{ gap: "12px" }}
          >
            <span
              style={{
                "font-size": "12px",
                color: "var(--color-text-secondary)",
              }}
            >
              {t("settings.about.customHeaders")}
            </span>
            <span
              class="mono"
              style={{
                "font-size": "12px",
                color: "var(--color-text-tertiary)",
              }}
            >
              {t("settings.about.headersCount", {
                n: Object.keys(
                  $config.get()?.download.headers ?? {},
                ).length,
              })}
            </span>
          </div>
        </div>
        <div
          style={{
            "font-size": "11px",
            color: "var(--color-text-tertiary)",
            "line-height": "1.5",
          }}
        >
          {t("settings.about.securityHint")}
        </div>
      </div>
    </>
  );
}
