import {
  createSignal,
  createEffect,
  For,
  Show,
  onMount,
  untrack,
} from "solid-js";
import { createStore } from "solid-js/store";
import { api } from "../api/invoke";
import { $config, $configLoading } from "../stores/settings";
import { addToast } from "../stores/toast";
import { $experimental } from "../stores/experimental";
import type { AppConfig, ConfigPatch } from "../types";
import { CloseIcon } from "./icons";
import ConfirmDialog from "./ConfirmDialog";
import Button from "../shared/ui/Button";
import { tr, type MessageKey } from "../i18n";

type SettingsTab =
  | "general"
  | "download"
  | "connection"
  | "scheduler"
  | "magnet"
  | "experimental"
  | "about";

interface SettingsPanelProps {
  visible: boolean;
  onClose: () => void;
  /** 初始打开的标签页(由 TitleBar 菜单\"关于\"等入口指定) */
  initialTab?: SettingsTab;
}

interface ConfigDraft {
  maxConcurrentTasks: number;
  download: {
    downloadDir: string;
    maxConcurrentFragments: number;
    maxRetries: number;
    requestTimeoutSecs: number;
    verifyChecksum: boolean;
    /** 限速(bytes/sec);null 表示不限速 */
    rateLimitBytesPerSec: number | null;
  };
  connection: {
    maxConnectionsPerHost: number;
    maxGlobalConnections: number;
    keepAliveTimeoutSecs: number;
    enableHttp2: boolean;
    enableQuic: boolean;
    connectTimeoutSecs: number;
  };
  scheduler: {
    minFragmentSize: number;
    maxFragmentSize: number;
    ewmaAlpha: number;
  };
  magnet: {
    enableDht: boolean;
    enableUpnp: boolean;
    trackers: string[];
  };
}

export default function SettingsPanel(props: SettingsPanelProps) {
  const t = (key: MessageKey, values?: Record<string, string | number>) =>
    tr(key, values as Record<string, string | number | unknown>);
  const [activeTab, setActiveTab] = createSignal<SettingsTab>(
    props.initialTab ?? "general",
  );
  const initialVisible = untrack(() => props.visible);
  const [shouldRender, setShouldRender] = createSignal(initialVisible);
  const [visible, setVisible] = createSignal(initialVisible);

  let closeTimer: number | null = null;

  const cancelCloseTimer = () => {
    if (closeTimer !== null) {
      clearTimeout(closeTimer);
      closeTimer = null;
    }
  };

  // 面板打开时,若调用方指定了 initialTab(如 TitleBar\"关于\"入口),切到该标签
  createEffect(() => {
    if (props.visible && props.initialTab) {
      setActiveTab(props.initialTab);
    }
  });

  createEffect(() => {
    if (props.visible) {
      cancelCloseTimer();
      if (!shouldRender()) {
        setShouldRender(true);
        requestAnimationFrame(() => {
          requestAnimationFrame(() => {
            setVisible(true);
          });
        });
      } else {
        setVisible(true);
      }
    } else if (shouldRender() && visible()) {
      setVisible(false);
      cancelCloseTimer();
      closeTimer = window.setTimeout(() => {
        setShouldRender(false);
        closeTimer = null;
      }, 250);
    }
  });

  const tabs: { id: SettingsTab; labelKey: MessageKey }[] = [
    { id: "general", labelKey: "settings.tab.general" },
    { id: "download", labelKey: "settings.tab.download" },
    { id: "connection", labelKey: "settings.tab.connection" },
    { id: "scheduler", labelKey: "settings.tab.scheduler" },
    { id: "magnet", labelKey: "settings.tab.magnet" },
    { id: "experimental", labelKey: "settings.tab.experimental" },
    { id: "about", labelKey: "settings.tab.about" },
  ];

  const [draft, setDraft] = createStore<ConfigDraft>({
    maxConcurrentTasks: 3,
    download: {
      downloadDir: "",
      maxConcurrentFragments: 8,
      maxRetries: 3,
      requestTimeoutSecs: 60,
      verifyChecksum: true,
      rateLimitBytesPerSec: null,
    },
    connection: {
      maxConnectionsPerHost: 4,
      maxGlobalConnections: 256,
      keepAliveTimeoutSecs: 90,
      enableHttp2: true,
      enableQuic: false,
      connectTimeoutSecs: 30,
    },
    scheduler: {
      minFragmentSize: 1048576,
      maxFragmentSize: 67108864,
      ewmaAlpha: 0.3,
    },
    magnet: {
      enableDht: true,
      enableUpnp: true,
      trackers: [],
    },
  });

  const [saving, setSaving] = createSignal(false);
  const [confirmOpen, setConfirmOpen] = createSignal(false);
  // About 标签:支持协议列表 + 应用版本(只读,来自后端)
  const [protocols, setProtocols] = createSignal<string[]>([]);
  const [appVersion, setAppVersion] = createSignal<string>("");

  const applyConfig = (cfg: AppConfig) => {
    setDraft({
      maxConcurrentTasks: cfg.maxConcurrentTasks,
      download: {
        downloadDir: cfg.download.downloadDir,
        maxConcurrentFragments: cfg.download.maxConcurrentFragments,
        maxRetries: cfg.download.maxRetries,
        requestTimeoutSecs: cfg.download.requestTimeoutSecs,
        verifyChecksum: cfg.download.verifyChecksum,
        rateLimitBytesPerSec: cfg.download.rateLimitBytesPerSec ?? null,
      },
      connection: {
        maxConnectionsPerHost: cfg.connection.maxConnectionsPerHost,
        maxGlobalConnections: cfg.connection.maxGlobalConnections,
        keepAliveTimeoutSecs: cfg.connection.keepAliveTimeoutSecs,
        enableHttp2: cfg.connection.enableHttp2,
        enableQuic: cfg.connection.enableQuic,
        connectTimeoutSecs: cfg.connection.connectTimeoutSecs,
      },
      scheduler: {
        minFragmentSize: cfg.scheduler.minFragmentSize,
        maxFragmentSize: cfg.scheduler.maxFragmentSize,
        ewmaAlpha: cfg.scheduler.ewmaAlpha,
      },
      magnet: {
        enableDht: cfg.magnet.enableDht,
        enableUpnp: cfg.magnet.enableUpnp,
        trackers: cfg.magnet.trackers,
      },
    });
  };

  onMount(async () => {
    $configLoading.set(true);
    try {
      const cfg = await api.getConfig();
      $config.set(cfg);
      applyConfig(cfg);
    } catch (e) {
      addToast(tr("toast.configLoadFailed", { error: String(e) }), "error");
    } finally {
      $configLoading.set(false);
    }
    // About 标签:并行拉取支持协议 + 应用信息(失败静默降级,不阻塞面板)
    try {
      const [proto, info] = await Promise.all([
        api.getSupportedProtocols(),
        api.getAppInfo(),
      ]);
      setProtocols(proto);
      setAppVersion(info.version);
    } catch {
      // 浏览器/无 Tauri 环境下静默降级,About 仍展示静态文案
    }
  });

  const buildPatch = (): ConfigPatch => {
    return {
      maxConcurrentTasks: draft.maxConcurrentTasks,
      download: {
        downloadDir: draft.download.downloadDir,
        maxConcurrentFragments: draft.download.maxConcurrentFragments,
        maxRetries: draft.download.maxRetries,
        requestTimeoutSecs: draft.download.requestTimeoutSecs,
        verifyChecksum: draft.download.verifyChecksum,
        rateLimitBytesPerSec: draft.download.rateLimitBytesPerSec,
      },
      connection: {
        maxConnectionsPerHost: draft.connection.maxConnectionsPerHost,
        maxGlobalConnections: draft.connection.maxGlobalConnections,
        keepAliveTimeoutSecs: draft.connection.keepAliveTimeoutSecs,
        enableHttp2: draft.connection.enableHttp2,
        enableQuic: draft.connection.enableQuic,
        connectTimeoutSecs: draft.connection.connectTimeoutSecs,
      },
      magnet: {
        enableDht: draft.magnet.enableDht,
        enableUpnp: draft.magnet.enableUpnp,
        trackers: draft.magnet.trackers,
      },
    };
  };

  const handleSave = () => {
    // 弹出确认对话框(P1-11)，替代 window.confirm
    setConfirmOpen(true);
  };

  const handleConfirmSave = async () => {
    setConfirmOpen(false);
    setSaving(true);
    try {
      await api.updateConfig(buildPatch());
      // 保存成功后重新拉取配置,确保前端状态与后端一致(安全字段由后端保留)
      const fresh = await api.getConfig();
      $config.set(fresh);
      applyConfig(fresh);
      addToast(tr("toast.configSaved"), "success");
    } catch (e) {
      addToast(tr("toast.configSaveFailed", { error: String(e) }), "error");
    } finally {
      setSaving(false);
    }
  };

  const handleChooseDownloadDir = async () => {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({ directory: true, multiple: false });
      if (typeof selected === "string") {
        setDraft("download", "downloadDir", selected);
      }
    } catch (e) {
      addToast(tr("toast.openDirPickerFailed", { error: String(e) }), "error");
    }
  };

  return (
    <Show when={shouldRender()}>
      {/* Overlay */}
      <div
        class="panel-overlay"
        style={{
          opacity: visible() ? 1 : 0,
          transition: "opacity 250ms ease",
        }}
        onClick={() => props.onClose()}
      />

      {/* Panel */}
      <div
        class="fixed z-[210]"
        style={{
          top: "50%",
          left: "50%",
          transform: `translate(-50%, -50%) scale(${visible() ? 1 : 0.95})`,
          opacity: visible() ? 1 : 0,
          transition:
            "transform 250ms cubic-bezier(0.32, 0.72, 0, 1), opacity 250ms ease",
          width: "min(640px, calc(100vw - 32px))",
          height: "min(520px, calc(100dvh - 64px))",
          /* 质感:顶部极淡向下高光 + 实色底,inset 上沿边线 */
          background:
            "linear-gradient(180deg, rgba(255,255,255,0.02) 0%, transparent 120px), var(--color-bg-elevated)",
          "border-radius": "16px",
          border: "1px solid var(--color-border-default)",
          "box-shadow": "var(--shadow-xl), inset 0 1px 0 rgba(255, 255, 255, 0.06)",
          display: "flex",
          overflow: "hidden",
        }}
      >
        {/* Sidebar */}
        <div
          style={{
            width: "160px",
            background: "var(--color-bg-secondary)",
            "border-right": "1px solid var(--color-border-subtle)",
            padding: "16px 8px",
            display: "flex",
            "flex-direction": "column",
            gap: "2px",
          }}
        >
          <For each={tabs}>
            {(tab) => (
              <button
                class="text-left"
                style={{
                  padding: "8px 12px",
                  "border-radius": "6px",
                  "font-size": "13px",
                  background:
                    activeTab() === tab.id
                      ? "var(--color-accent-soft)"
                      : "transparent",
                  color:
                    activeTab() === tab.id
                      ? "var(--color-accent-primary)"
                      : "var(--color-text-secondary)",
                  border: "none",
                  cursor: "pointer",
                  transition: "all 150ms ease",
                  "font-weight": activeTab() === tab.id ? 600 : 400,
                }}
                onClick={() => setActiveTab(tab.id)}
              >
                {t(tab.labelKey)}
              </button>
            )}
          </For>
        </div>

        {/* Content */}
        <div class="flex-1 flex flex-col" style={{ overflow: "hidden" }}>
          {/* Header */}
          <div class="panel-header">
            <span
              style={{
                "font-size": "15px",
                "font-weight": 600,
                color: "var(--color-text-title)",
              }}
            >
              {t(tabs.find((tb) => tb.id === activeTab())!.labelKey) + t("settings.titleSuffix")}
            </span>
            <Button
              variant="ghost"
              shape="icon-sm"
              class="hover-light"
              aria-label={t("settings.aria.close")}
              onClick={() => props.onClose()}
            >
              <CloseIcon />
            </Button>
          </div>

          {/* Scrollable content */}
          <div class="flex-1 overflow-y-auto" style={{ padding: "20px" }}>
            <Show
              when={!$configLoading.get()}
              fallback={
                <div
                  style={{
                    color: "var(--color-text-secondary)",
                    "font-size": "14px",
                  }}
                >
                  {t("settings.loadingConfig")}
                </div>
              }
            >
              <Show when={activeTab() === "general"}>
                <div class="flex flex-col gap-5">
                  <div>
                    <div
                      style={{
                        "font-size": "13px",
                        color: "var(--color-text-secondary)",
                        "margin-bottom": "8px",
                      }}
                    >
                      {t("settings.general.defaultDir")}
                    </div>
                    <div class="flex items-center gap-2">
                      <input
                        type="text"
                        class="input flex-1"
                        value={draft.download.downloadDir}
                        onInput={(e) =>
                          setDraft(
                            "download",
                            "downloadDir",
                            e.currentTarget.value,
                          )
                        }
                        style={{ "font-size": "13px" }}
                      />
                      <Button
                        variant="secondary"
                        size="sm"
                        onClick={handleChooseDownloadDir}
                      >
                        {t("common.browse")}
                      </Button>
                    </div>
                  </div>
                </div>
              </Show>

              <Show when={activeTab() === "download"}>
                <div class="flex flex-col gap-5">
                  <SliderItem
                    label={t("settings.download.maxConcurrentTasks")}
                    value={draft.maxConcurrentTasks}
                    min={1}
                    max={16}
                    onChange={(v) => setDraft("maxConcurrentTasks", v)}
                    displayValue={`${draft.maxConcurrentTasks}`}
                  />
                  <SliderItem
                    label={t("settings.download.maxConcurrentFragments")}
                    value={draft.download.maxConcurrentFragments}
                    min={1}
                    max={32}
                    onChange={(v) =>
                      setDraft("download", "maxConcurrentFragments", v)
                    }
                    displayValue={`${draft.download.maxConcurrentFragments}`}
                  />
                  <SliderItem
                    label={t("settings.download.maxRetries")}
                    value={draft.download.maxRetries}
                    min={0}
                    max={10}
                    onChange={(v) => setDraft("download", "maxRetries", v)}
                    displayValue={t("settings.download.maxRetriesValue", { n: draft.download.maxRetries })}
                  />
                  <SliderItem
                    label={t("settings.download.requestTimeout")}
                    value={draft.download.requestTimeoutSecs}
                    min={5}
                    max={120}
                    onChange={(v) =>
                      setDraft("download", "requestTimeoutSecs", v)
                    }
                    displayValue={t("settings.download.requestTimeoutValue", {
                      n: draft.download.requestTimeoutSecs,
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
                          draft.download.rateLimitBytesPerSec ?? ""
                        }
                        onInput={(e) => {
                          const raw = e.currentTarget.value.trim();
                          if (raw === "") {
                            setDraft("download", "rateLimitBytesPerSec", null);
                          } else {
                            const n = Number(raw);
                            setDraft(
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
                  <ToggleItem
                    label={t("settings.download.verifyChecksum")}
                    value={draft.download.verifyChecksum}
                    onChange={(v) => setDraft("download", "verifyChecksum", v)}
                  />
                </div>
              </Show>

              <Show when={activeTab() === "connection"}>
                <div class="flex flex-col gap-5">
                  <SliderItem
                    label={t("settings.connection.maxConnectionsPerHost")}
                    value={draft.connection.maxConnectionsPerHost}
                    min={1}
                    max={16}
                    onChange={(v) =>
                      setDraft("connection", "maxConnectionsPerHost", v)
                    }
                    displayValue={`${draft.connection.maxConnectionsPerHost}`}
                  />
                  <SliderItem
                    label={t("settings.connection.connectTimeout")}
                    value={draft.connection.connectTimeoutSecs}
                    min={5}
                    max={120}
                    onChange={(v) =>
                      setDraft("connection", "connectTimeoutSecs", v)
                    }
                    displayValue={t("settings.connection.connectTimeoutValue", { n: draft.connection.connectTimeoutSecs })}
                  />
                  <SliderItem
                    label={t("settings.connection.maxGlobalConnections")}
                    value={draft.connection.maxGlobalConnections}
                    min={1}
                    max={256}
                    onChange={(v) =>
                      setDraft("connection", "maxGlobalConnections", v)
                    }
                    displayValue={`${draft.connection.maxGlobalConnections}`}
                  />
                  <SliderItem
                    label={t("settings.connection.keepAliveTimeout")}
                    value={draft.connection.keepAliveTimeoutSecs}
                    min={1}
                    max={120}
                    onChange={(v) =>
                      setDraft("connection", "keepAliveTimeoutSecs", v)
                    }
                    displayValue={t("settings.connection.keepAliveTimeoutValue", {
                      n: draft.connection.keepAliveTimeoutSecs,
                    })}
                  />
                  <ToggleItem
                    label={t("settings.connection.enableHttp2")}
                    value={draft.connection.enableHttp2}
                    onChange={(v) => setDraft("connection", "enableHttp2", v)}
                  />
                  <ToggleItem
                    label={t("settings.connection.enableQuic")}
                    value={draft.connection.enableQuic}
                    onChange={(v) => setDraft("connection", "enableQuic", v)}
                  />
                </div>
              </Show>

              <Show when={activeTab() === "scheduler"}>
                <div class="flex flex-col gap-5">
                  <SliderItem
                    label={t("settings.scheduler.minFragmentSize")}
                    value={draft.scheduler.minFragmentSize}
                    min={262144}
                    max={10485760}
                    onChange={(v) =>
                      setDraft("scheduler", "minFragmentSize", v)
                    }
                    displayValue={`${(draft.scheduler.minFragmentSize / 1048576).toFixed(1)} MB`}
                  />
                  <SliderItem
                    label={t("settings.scheduler.maxFragmentSize")}
                    value={draft.scheduler.maxFragmentSize}
                    min={10485760}
                    max={134217728}
                    onChange={(v) =>
                      setDraft("scheduler", "maxFragmentSize", v)
                    }
                    displayValue={`${(draft.scheduler.maxFragmentSize / 1048576).toFixed(0)} MB`}
                  />
                  <SliderItem
                    label={t("settings.scheduler.ewmaAlpha")}
                    value={draft.scheduler.ewmaAlpha}
                    min={0.1}
                    max={0.9}
                    onChange={(v) => setDraft("scheduler", "ewmaAlpha", v)}
                    displayValue={draft.scheduler.ewmaAlpha.toFixed(2)}
                  />
                </div>
              </Show>

              <Show when={activeTab() === "magnet"}>
                <div class="flex flex-col gap-5">
                  <ToggleItem
                    label={t("settings.magnet.enableDht")}
                    value={draft.magnet.enableDht}
                    onChange={(v) => setDraft("magnet", "enableDht", v)}
                  />
                  <ToggleItem
                    label={t("settings.magnet.enableUpnp")}
                    value={draft.magnet.enableUpnp}
                    onChange={(v) => setDraft("magnet", "enableUpnp", v)}
                  />
                  <TrackerList
                    trackers={draft.magnet.trackers}
                    onAdd={(url) => {
                      setDraft("magnet", "trackers", [...draft.magnet.trackers, url]);
                    }}
                    onRemove={(index) => {
                      const updated = draft.magnet.trackers.filter((_, i) => i !== index);
                      setDraft("magnet", "trackers", updated);
                    }}
                    onImportPresets={() => {
                      const existing = new Set(draft.magnet.trackers);
                      const newTrackers = PRESET_TRACKERS.filter((t) => !existing.has(t));
                      setDraft("magnet", "trackers", [...draft.magnet.trackers, ...newTrackers]);
                      addToast(
                        tr("settings.magnet.importSuccess", { n: newTrackers.length }),
                        "success"
                      );
                    }}
                    onClearAll={() => {
                      setDraft("magnet", "trackers", []);
                    }}
                  />
                </div>
              </Show>

              <Show when={activeTab() === "experimental"}>
                <div class="flex flex-col gap-5">
                  <ToggleItem
                    label={t("experimental.huggingface")}
                    value={$experimental.isEnabled("huggingface")}
                    onChange={() => $experimental.toggle("huggingface")}
                  />
                  <div
                    style={{
                      "font-size": "12px",
                      color: "var(--color-text-tertiary)",
                      "line-height": "1.5",
                    }}
                  >
                    {t("experimental.huggingfaceDesc")}
                  </div>
                </div>
              </Show>

              <Show when={activeTab() === "about"}>
                <div
                  class="flex flex-col items-center gap-3"
                  style={{ padding: "32px 20px 24px" }}
                >
                  <div
                    style={{
                      width: "48px",
                      height: "48px",
                      /* 去 AI 味:135deg 渐变品牌块替换为实色 + inner highlight */
                      background: "var(--color-accent-primary)",
                      "border-radius": "12px",
                      display: "flex",
                      "align-items": "center",
                      "justify-content": "center",
                      color: "var(--color-text-inverse)",
                      "font-family": "var(--font-mono)",
                      "font-size": "22px",
                      "font-weight": 700,
                      "box-shadow":
                        "inset 0 1px 0 rgba(255,255,255,0.12), 0 1px 2px rgba(0,0,0,0.4)",
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
                    {appVersion()
                      ? t("settings.about.versionValue", { v: appVersion() })
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
                    <For each={protocols()}>
                      {(proto) => (
                        <span
                          style={{
                            "font-size": "11px",
                            "font-weight": 600,
                            color: "var(--color-accent-secondary)",
                            background: "var(--color-accent-soft)",
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
              </Show>
            </Show>
          </div>

          <div
            class="flex items-center justify-end gap-2"
            style={{
              padding: "12px 20px",
              "border-top": "1px solid var(--color-border-subtle)",
            }}
          >
            <Button
              variant="secondary"
              size="sm"
              onClick={() => props.onClose()}
            >
              {t("common.cancel")}
            </Button>
            <Button
              variant="primary"
              size="sm"
              loading={$configLoading.get() || saving()}
              onClick={handleSave}
            >
              {saving() ? t("common.saving") : t("settings.save")}
            </Button>
          </div>
        </div>
      </div>

      {/* 配置保存确认对话框(P1-11) */}
      <ConfirmDialog
        open={confirmOpen()}
        title={t("confirm.updateConfig.title")}
        message={t("confirm.updateConfig.message")}
        confirmLabel={t("confirm.updateConfig.confirmLabel")}
        loading={saving()}
        onConfirm={handleConfirmSave}
        onCancel={() => setConfirmOpen(false)}
      />
    </Show>
  );
}

function ToggleItem(props: {
  label: string;
  value: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <div class="flex items-center justify-between">
      <span style={{ "font-size": "13px", color: "var(--color-text-title)" }}>
        {props.label}
      </span>
      <button
        class="relative"
        style={{
          width: "40px",
          height: "22px",
          "border-radius": "11px",
          background: props.value
            ? "var(--color-accent-primary)"
            : "var(--graphite-2)",
          border: "none",
          cursor: "pointer",
          transition: "background 200ms ease",
        }}
        onClick={() => props.onChange(!props.value)}
      >
        <div
          style={{
            position: "absolute",
            width: "18px",
            height: "18px",
            "border-radius": "50%",
            background: "white",
            top: "2px",
            left: "2px",
            transform: props.value ? "translateX(18px)" : "translateX(0)",
            transition: "transform 200ms cubic-bezier(0.32, 0.72, 0, 1)",
          }}
        />
      </button>
    </div>
  );
}

function SliderItem(props: {
  label: string;
  value: number;
  min: number;
  max: number;
  onChange: (v: number) => void;
  displayValue: string;
}) {
  return (
    <div>
      <div
        class="flex items-center justify-between"
        style={{ "margin-bottom": "8px" }}
      >
        <span
          style={{ "font-size": "13px", color: "var(--color-text-secondary)" }}
        >
          {props.label}
        </span>
        <span
          class="mono"
          style={{ "font-size": "13px", color: "var(--color-text-title)" }}
        >
          {props.displayValue}
        </span>
      </div>
      <input
        type="range"
        aria-label={props.label}
        min={props.min}
        max={props.max}
        value={props.value}
        onInput={(e) => props.onChange(parseInt(e.currentTarget.value))}
        style={{ width: "100%" }}
      />
    </div>
  );
}

/** 精选公共 Tracker 预设列表 */
const PRESET_TRACKERS: string[] = [
  "udp://tracker.opentrackr.org:1337/announce",
  "udp://open.stealth.si:80/announce",
  "udp://tracker.torrent.eu.org:451/announce",
  "udp://tracker.bittor.pw:1337/announce",
  "udp://public.popcorn-tracker.org:6969/announce",
  "udp://tracker.dler.org:6969/announce",
  "udp://exodus.desync.com:6969/announce",
  "udp://open.demonii.com:1337/announce",
  "https://tracker.tamersunion.org:443/announce",
  "https://tracker.lilithraws.org:443/announce",
];

/** 检测 tracker URL 基本格式 */
function isValidTrackerUrl(url: string): boolean {
  const trimmed = url.trim();
  if (!trimmed) return false;
  return /^udp:\/\/.+|^https?:\/\/.+/.test(trimmed);
}

function TrackerList(props: {
  trackers: string[];
  onAdd: (url: string) => void;
  onRemove: (index: number) => void;
  onImportPresets: () => void;
  onClearAll: () => void;
}) {
  const i18n = tr;
  const [newUrl, setNewUrl] = createSignal("");
  const [urlError, setUrlError] = createSignal<string | null>(null);

  const handleAdd = () => {
    const trimmed = newUrl().trim();
    if (!isValidTrackerUrl(trimmed)) {
      setUrlError(i18n("settings.magnet.invalidUrl"));
      return;
    }
    if (props.trackers.includes(trimmed)) {
      setUrlError(i18n("settings.magnet.duplicateUrl"));
      return;
    }
    props.onAdd(trimmed);
    setNewUrl("");
    setUrlError(null);
  };

  const handleKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Enter") handleAdd();
  };

  return (
    <div>
      <div class="flex items-center justify-between" style={{ "margin-bottom": "12px" }}>
        <div class="flex items-center gap-2">
          <span style={{ "font-size": "13px", color: "var(--color-text-secondary)" }}>
            {i18n("settings.magnet.trackerList")}
          </span>
          <span
            class="mono"
            style={{
              "font-size": "12px",
              color: "var(--color-text-tertiary)",
              background: "var(--color-bg-secondary)",
              padding: "1px 6px",
              "border-radius": "4px",
            }}
          >
            {i18n("settings.magnet.trackerCount", { n: props.trackers.length })}
          </span>
        </div>
        <div class="flex items-center gap-2">
          <Button variant="secondary" size="sm" onClick={props.onImportPresets}>
            {i18n("settings.magnet.importPreset")}
          </Button>
          <Button
            variant="secondary"
            size="sm"
            onClick={props.onClearAll}
            disabled={props.trackers.length === 0}
          >
            {i18n("settings.magnet.clearAll")}
          </Button>
        </div>
      </div>

      {/* Tracker 列表 */}
      <div
        style={{
          "max-height": "200px",
          "overflow-y": "auto",
          border: "1px solid var(--color-border-subtle)",
          "border-radius": "8px",
          "margin-bottom": "8px",
        }}
      >
        <Show
          when={props.trackers.length > 0}
          fallback={
            <div
              style={{
                padding: "16px",
                "text-align": "center",
                "font-size": "13px",
                color: "var(--color-text-tertiary)",
              }}
            >
              暂无 Tracker
            </div>
          }
        >
          <For each={props.trackers}>
            {(tracker, index) => (
              <div
                class="flex items-center justify-between"
                style={{
                  padding: "6px 12px",
                  "border-bottom":
                    index() < props.trackers.length - 1
                      ? "1px solid var(--color-border-subtle)"
                      : "none",
                }}
              >
                <span
                  class="mono"
                  style={{
                    "font-size": "12px",
                    color: "var(--color-text-secondary)",
                    overflow: "hidden",
                    "text-overflow": "ellipsis",
                    "white-space": "nowrap",
                    flex: "1",
                    "margin-right": "8px",
                  }}
                >
                  {tracker}
                </span>
                <button
                  style={{
                    background: "none",
                    border: "none",
                    cursor: "pointer",
                    color: "var(--color-text-tertiary)",
                    padding: "2px 4px",
                    "border-radius": "4px",
                    "font-size": "14px",
                  }}
                  onClick={() => props.onRemove(index())}
                  aria-label={i18n("settings.magnet.removeTracker")}
                >
                  ×
                </button>
              </div>
            )}
          </For>
        </Show>
      </div>

      {/* 添加行 */}
      <div class="flex items-center gap-2">
        <input
          type="text"
          class="input flex-1"
          value={newUrl()}
          onInput={(e) => {
            setNewUrl(e.currentTarget.value);
            setUrlError(null);
          }}
          onKeyDown={handleKeyDown}
          placeholder={i18n("settings.magnet.addTrackerPlaceholder")}
          style={{
            "font-size": "12px",
            "font-family": "monospace",
            "border-color": urlError()
              ? "var(--color-error)"
              : undefined,
          }}
        />
        <Button
          variant="secondary"
          size="sm"
          onClick={handleAdd}
          disabled={!newUrl().trim()}
        >
          {i18n("settings.magnet.addTracker")}
        </Button>
      </div>
      <Show when={urlError()}>
        <div style={{ "font-size": "12px", color: "var(--color-error)", "margin-top": "4px" }}>
          {urlError()}
        </div>
      </Show>
    </div>
  );
}
