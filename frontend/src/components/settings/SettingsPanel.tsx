import { errorMessage } from "../../utils/appError";
import {
  createSignal,
  createEffect,
  For,
  Show,
  onMount,
  untrack,
} from "solid-js";
import { createStore } from "solid-js/store";
import { api } from "../../api/invoke";
import { $config, $configLoading } from "../../stores/settings";
import { addToast } from "../../stores/toast";
import type { AppConfig, ConfigPatch, HfSourceMode } from "../../types";
import { CloseIcon } from "../icons";
import ConfirmDialog from "../ConfirmDialog";
import Button from "../../shared/ui/Button";
import { tr, type MessageKey } from "../../i18n";
import { $experimental } from "../../stores/experimental";
import GeneralTab from "./tabs/GeneralTab";
import DownloadTab from "./tabs/DownloadTab";
import ConnectionTab from "./tabs/ConnectionTab";
import SchedulerTab from "./tabs/SchedulerTab";
import MagnetTab from "./tabs/MagnetTab";
import ExperimentalTab from "./tabs/ExperimentalTab";
import AboutTab from "./tabs/AboutTab";
import ShortcutsTab from "./tabs/ShortcutsTab";

export type SettingsTab =
  | "general"
  | "download"
  | "connection"
  | "scheduler"
  | "magnet"
  | "experimental"
  | "shortcuts"
  | "about";

export interface SettingsPanelProps {
  visible: boolean;
  onClose: () => void;
  /** 初始打开的标签页(由 TitleBar 菜单"关于"等入口指定) */
  initialTab?: SettingsTab;
}

export interface ConfigDraft {
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
    disableDhtPersistence: boolean;
    socksProxyUrl: string | null;
    peerConnectTimeoutSecs: number;
    peerReadWriteTimeoutSecs: number;
    forceTrackerIntervalSecs: number;
    deferWritesUpToMb: number;
    disableDhtWhenSocks: boolean;
  };
  hub: {
    sourceMode: HfSourceMode;
  };
  notifications: {
    enabled: boolean;
  };
}

const tabs: { id: SettingsTab; labelKey: MessageKey }[] = [
  { id: "general", labelKey: "settings.tab.general" },
  { id: "download", labelKey: "settings.tab.download" },
  { id: "connection", labelKey: "settings.tab.connection" },
  { id: "scheduler", labelKey: "settings.tab.scheduler" },
  { id: "magnet", labelKey: "settings.tab.magnet" },
  { id: "experimental", labelKey: "settings.tab.experimental" },
  { id: "shortcuts", labelKey: "settings.tab.shortcuts" },
  { id: "about", labelKey: "settings.tab.about" },
];

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

  // 面板打开时,若调用方指定了 initialTab(如 TitleBar"关于"入口),切到该标签
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
      disableDhtPersistence: false,
      socksProxyUrl: null,
      peerConnectTimeoutSecs: 8,
      peerReadWriteTimeoutSecs: 10,
      forceTrackerIntervalSecs: 120,
      deferWritesUpToMb: 16,
      disableDhtWhenSocks: true,
    },
    hub: {
      sourceMode: "mirror",
    },
    notifications: {
      enabled: true,
    },
  });

  const [saving, setSaving] = createSignal(false);
  const [confirmOpen, setConfirmOpen] = createSignal(false);
  const [importConfirmOpen, setImportConfirmOpen] = createSignal(false);
  const [exporting, setExporting] = createSignal(false);
  const [importing, setImporting] = createSignal(false);
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
        disableDhtPersistence: cfg.magnet.disableDhtPersistence,
        socksProxyUrl: cfg.magnet.socksProxyUrl,
        // 新增字段:向后兼容旧版后端快照(字段可能缺失),缺失时回退后端默认值
        peerConnectTimeoutSecs: cfg.magnet.peerConnectTimeoutSecs ?? 8,
        peerReadWriteTimeoutSecs: cfg.magnet.peerReadWriteTimeoutSecs ?? 10,
        forceTrackerIntervalSecs: cfg.magnet.forceTrackerIntervalSecs ?? 120,
        deferWritesUpToMb: cfg.magnet.deferWritesUpToMb ?? 16,
        disableDhtWhenSocks: cfg.magnet.disableDhtWhenSocks ?? true,
      },
      hub: {
        sourceMode: cfg.hub?.sourceMode ?? "mirror",
      },
      notifications: {
        enabled: cfg.notifications?.enabled ?? true,
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
      addToast(tr("toast.configLoadFailed", { error: errorMessage(e) }), "error");
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
        disableDhtPersistence: draft.magnet.disableDhtPersistence,
        socksProxyUrl: draft.magnet.socksProxyUrl || null,
        peerConnectTimeoutSecs: draft.magnet.peerConnectTimeoutSecs,
        peerReadWriteTimeoutSecs: draft.magnet.peerReadWriteTimeoutSecs,
        forceTrackerIntervalSecs: draft.magnet.forceTrackerIntervalSecs,
        deferWritesUpToMb: draft.magnet.deferWritesUpToMb,
        disableDhtWhenSocks: draft.magnet.disableDhtWhenSocks,
      },
      scheduler: {
        minFragmentSize: draft.scheduler.minFragmentSize,
        maxFragmentSize: draft.scheduler.maxFragmentSize,
        ewmaAlpha: draft.scheduler.ewmaAlpha,
      },
      hub: {
        sourceMode: draft.hub.sourceMode,
      },
      notifications: {
        enabled: draft.notifications.enabled,
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
      addToast(tr("toast.configSaveFailed", { error: errorMessage(e) }), "error");
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
      addToast(tr("toast.openDirPickerFailed", { error: errorMessage(e) }), "error");
    }
  };

  const handleExportBackup = async () => {
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const path = await save({
        defaultPath: "tachyon-config-backup.json",
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (!path) return;
      setExporting(true);
      await api.exportBackup(path);
      addToast(tr("toast.exportBackupSuccess"), "success");
    } catch (e) {
      addToast(tr("toast.exportBackupFailed", { error: errorMessage(e) }), "error");
    } finally {
      setExporting(false);
    }
  };

  const handleImportBackup = async () => {
    setImportConfirmOpen(false);
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({
        filters: [{ name: "JSON", extensions: ["json"] }],
        multiple: false,
      });
      if (typeof selected !== "string") return;
      setImporting(true);
      await api.importBackup(selected);
      const fresh = await api.getConfig();
      $config.set(fresh);
      applyConfig(fresh);
      addToast(tr("toast.importBackupSuccess"), "success");
    } catch (e) {
      addToast(tr("toast.importBackupFailed", { error: errorMessage(e) }), "error");
    } finally {
      setImporting(false);
    }
  };

  const tabContent = () => {
    const tab = activeTab();
    switch (tab) {
      case "general":
        return (
          <GeneralTab
            draft={draft}
            setDraft={setDraft}
            onChooseDownloadDir={handleChooseDownloadDir}
          />
        );
      case "download":
        return <DownloadTab draft={draft} setDraft={setDraft} />;
      case "connection":
        return <ConnectionTab draft={draft} setDraft={setDraft} />;
      case "scheduler":
        return <SchedulerTab draft={draft} setDraft={setDraft} />;
      case "magnet":
        return <MagnetTab draft={draft} setDraft={setDraft} />;
      case "experimental":
        return (
          <ExperimentalTab
            draft={draft}
            isHuggingfaceEnabled={$experimental.isEnabled("huggingface")}
            toggleHuggingface={() => $experimental.toggle("huggingface")}
            setDraft={setDraft}
          />
        );
      case "shortcuts":
        return <ShortcutsTab />;
      case "about":
        return (
          <AboutTab
            appVersion={appVersion()}
            protocols={protocols()}
          />
        );
      default:
        return null;
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
        class="fixed z-[var(--z-panel-content)]"
        style={{
          top: "50%",
          left: "50%",
          transform: `translate(-50%, -50%) scale(${visible() ? 1 : 0.95})`,
          opacity: visible() ? 1 : 0,
          transition:
            "transform 250ms cubic-bezier(0.32, 0.72, 0, 1), opacity 250ms ease",
          width: "min(640px, calc(100vw - 32px))",
          height: "min(520px, calc(100dvh - 64px))",
          /* 去 AI 味:实色底 + 边框分层,移除顶部高光渐变与 inset 装饰 */
          background: "var(--color-bg-elevated)",
          "border-radius": "16px",
          border: "1px solid var(--color-border-default)",
          "box-shadow": "var(--shadow-xl)",
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
          <div class="flex-1 scroll-container" style={{ padding: "20px" }}>
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
              {tabContent()}
            </Show>
          </div>

          <div
            class="flex items-center justify-between gap-2"
            style={{
              padding: "12px 20px",
              "border-top": "1px solid var(--color-border-subtle)",
            }}
          >
            <div class="flex items-center gap-2">
              <Button
                variant="secondary"
                size="sm"
                loading={exporting()}
                onClick={handleExportBackup}
              >
                {t("settings.exportBackup")}
              </Button>
              <Button
                variant="secondary"
                size="sm"
                loading={importing()}
                onClick={() => setImportConfirmOpen(true)}
              >
                {t("settings.importBackup")}
              </Button>
            </div>
            <div class="flex items-center gap-2">
              <Button
                variant="secondary"
                size="sm"
                onClick={() => props.onClose()}
              >
                {t("common.cancel")}
              </Button>
              <Button
                variant="brand"
                size="sm"
                loading={$configLoading.get() || saving()}
                onClick={handleSave}
              >
                {saving() ? t("common.saving") : t("settings.save")}
              </Button>
            </div>
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

      {/* 配置导入确认对话框:覆盖当前配置属于破坏性操作 */}
      <ConfirmDialog
        open={importConfirmOpen()}
        title={t("confirm.importBackup.title")}
        message={t("confirm.importBackup.message")}
        confirmLabel={t("confirm.importBackup.confirmLabel")}
        loading={importing()}
        onConfirm={handleImportBackup}
        onCancel={() => setImportConfirmOpen(false)}
      />
    </Show>
  );
}
