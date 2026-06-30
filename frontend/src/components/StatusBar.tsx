import { createMemo } from "solid-js";
import { ArrowDownIcon, SunIcon, MoonIcon } from "./icons";
import Sparkline from "./Sparkline";
import Button from "../shared/ui/Button";
import LanguageSwitcher from "../shared/ui/LanguageSwitcher";
import { formatSpeed } from "../utils/format";
import { getHistory } from "../stores/speedHistory";
import { useI18n } from "../i18n";
import { useTheme } from "../hooks/useTheme";

interface StatusBarProps {
  isIdle: boolean;
  totalSpeed: number;
  activeCount: number;
  pausedCount: number;
  totalCount: number;
}

export default function StatusBar(props: StatusBarProps) {
  const i18n = useI18n();
  const { theme, toggleTheme } = useTheme();

  // 真实速度历史:取最近 30 个采样点,删除 Math.random 伪造数据
  const speedHistory = createMemo(() => {
    const history = getHistory();
    return history.slice(-30);
  });

  return (
    <div
      class="flex items-center justify-between flex-shrink-0"
      style={{
        height: "28px",
        background: "var(--color-bg-secondary)",
        "border-top": "1px solid var(--color-border-subtle)",
        padding: "0 12px",
        "font-size": "12px",
      }}
    >
      {/* Left */}
      <div
        class="flex items-center gap-2"
        role="status"
        aria-live="polite"
        aria-atomic="true"
      >
        <div
          style={{
            width: "8px",
            height: "8px",
            "border-radius": "50%",
            // 下载中 = 品牌紫(状态语义:紫=活跃工作)
            background: props.isIdle
              ? "var(--color-text-tertiary)"
              : "var(--color-accent-primary)",
          }}
          class={props.isIdle ? "" : "status-indicator-active"}
          aria-hidden="true"
        />
        <span style={{ color: "var(--color-text-secondary)" }}>
          {props.isIdle ? i18n.t("status.idle") : i18n.t("status.downloading")}
        </span>
        <span
          class="mono"
          style={{
            // Neon Cyan 仅限实时速度数字(速度 = 能量隐喻)
            color: props.isIdle
              ? "var(--color-text-secondary)"
              : "var(--color-speed-active)",
            display: "flex",
            "align-items": "center",
            gap: "4px",
            transition: "color 300ms ease",
          }}
        >
          <ArrowDownIcon aria-hidden="true" />
          <span class={props.isIdle ? "" : "speed-breathe"}>
            {formatSpeed(props.totalSpeed)}
          </span>
        </span>
        <span aria-hidden="true">
          <Sparkline data={speedHistory()} width={80} height={16} />
        </span>

        <span style={{ color: "var(--color-text-tertiary)" }}>
          {i18n.t("status.countSummary", {
            active: props.activeCount,
            paused: props.pausedCount,
            total: props.totalCount,
          })}
        </span>
      </div>

      {/* Right */}
      <div class="flex items-center gap-3">
        {/* 明暗主题切换:读写 localStorage + data-theme,去 AI 味的实色图标无辉光 */}
        <Button
          variant="ghost"
          shape="icon-sm"
          aria-label={i18n.t("status.theme.toggle") as string}
          title={
            (theme() === "dark"
              ? i18n.t("status.theme.light")
              : i18n.t("status.theme.dark")) as string
          }
          onClick={toggleTheme}
        >
          {theme() === "dark" ? <SunIcon /> : <MoonIcon />}
        </Button>
        <LanguageSwitcher />
      </div>
    </div>
  );
}
