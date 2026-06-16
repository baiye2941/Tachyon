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
import type { AppConfig, ConfigPatch } from "../types";
import { CloseIcon } from "./icons";
import ConfirmDialog from "./ConfirmDialog";
import Button from "../shared/ui/Button";

type SettingsTab =
  | "general"
  | "download"
  | "connection"
  | "scheduler"
  | "about";

interface SettingsPanelProps {
  visible: boolean;
  onClose: () => void;
}

interface ConfigDraft {
  maxConcurrentTasks: number;
  download: {
    downloadDir: string;
    maxConcurrentFragments: number;
    maxRetries: number;
    verifyChecksum: boolean;
  };
  connection: {
    maxConnectionsPerHost: number;
    enableHttp2: boolean;
    enableQuic: boolean;
    connectTimeoutSecs: number;
  };
  scheduler: {
    minFragmentSize: number;
    maxFragmentSize: number;
    ewmaAlpha: number;
  };
}

export default function SettingsPanel(props: SettingsPanelProps) {
  const [activeTab, setActiveTab] = createSignal<SettingsTab>("general");
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

  const tabs: { id: SettingsTab; label: string }[] = [
    { id: "general", label: "通用" },
    { id: "download", label: "下载" },
    { id: "connection", label: "连接" },
    { id: "scheduler", label: "调度" },
    { id: "about", label: "关于" },
  ];

  const [draft, setDraft] = createStore<ConfigDraft>({
    maxConcurrentTasks: 3,
    download: {
      downloadDir: "",
      maxConcurrentFragments: 8,
      maxRetries: 3,
      verifyChecksum: true,
    },
    connection: {
      maxConnectionsPerHost: 4,
      enableHttp2: true,
      enableQuic: false,
      connectTimeoutSecs: 30,
    },
    scheduler: {
      minFragmentSize: 1048576,
      maxFragmentSize: 67108864,
      ewmaAlpha: 0.3,
    },
  });

  const [saving, setSaving] = createSignal(false);
  const [confirmOpen, setConfirmOpen] = createSignal(false);

  const applyConfig = (cfg: AppConfig) => {
    setDraft({
      maxConcurrentTasks: cfg.maxConcurrentTasks,
      download: {
        downloadDir: cfg.download.downloadDir,
        maxConcurrentFragments: cfg.download.maxConcurrentFragments,
        maxRetries: cfg.download.maxRetries,
        verifyChecksum: cfg.download.verifyChecksum,
      },
      connection: {
        maxConnectionsPerHost: cfg.connection.maxConnectionsPerHost,
        enableHttp2: cfg.connection.enableHttp2,
        enableQuic: cfg.connection.enableQuic,
        connectTimeoutSecs: cfg.connection.connectTimeoutSecs,
      },
      scheduler: {
        minFragmentSize: cfg.scheduler.minFragmentSize,
        maxFragmentSize: cfg.scheduler.maxFragmentSize,
        ewmaAlpha: cfg.scheduler.ewmaAlpha,
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
      addToast("加载配置失败: " + String(e), "error");
    } finally {
      $configLoading.set(false);
    }
  });

  const buildPatch = (): ConfigPatch => {
    return {
      maxConcurrentTasks: draft.maxConcurrentTasks,
      download: {
        downloadDir: draft.download.downloadDir,
        maxConcurrentFragments: draft.download.maxConcurrentFragments,
        maxRetries: draft.download.maxRetries,
        verifyChecksum: draft.download.verifyChecksum,
      },
      connection: {
        maxConnectionsPerHost: draft.connection.maxConnectionsPerHost,
        enableHttp2: draft.connection.enableHttp2,
        enableQuic: draft.connection.enableQuic,
        connectTimeoutSecs: draft.connection.connectTimeoutSecs,
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
      addToast("配置已保存", "success");
    } catch (e) {
      addToast("保存配置失败: " + String(e), "error");
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
      addToast("无法打开目录选择器: " + String(e), "error");
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
          background: "var(--color-bg-elevated)",
          "border-radius": "16px",
          border: "1px solid var(--color-border-subtle)",
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
                {tab.label}
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
              {tabs.find((t) => t.id === activeTab())?.label}设置
            </span>
            <Button
              variant="ghost"
              shape="icon-sm"
              class="hover-light"
              aria-label="关闭设置"
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
                  加载配置中...
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
                      默认下载路径
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
                        浏览
                      </Button>
                    </div>
                  </div>
                </div>
              </Show>

              <Show when={activeTab() === "download"}>
                <div class="flex flex-col gap-5">
                  <SliderItem
                    label="最大并发任务数"
                    value={draft.maxConcurrentTasks}
                    min={1}
                    max={16}
                    onChange={(v) => setDraft("maxConcurrentTasks", v)}
                    displayValue={`${draft.maxConcurrentTasks}`}
                  />
                  <SliderItem
                    label="最大并发分片数"
                    value={draft.download.maxConcurrentFragments}
                    min={1}
                    max={32}
                    onChange={(v) =>
                      setDraft("download", "maxConcurrentFragments", v)
                    }
                    displayValue={`${draft.download.maxConcurrentFragments}`}
                  />
                  <SliderItem
                    label="最大重试次数"
                    value={draft.download.maxRetries}
                    min={0}
                    max={10}
                    onChange={(v) => setDraft("download", "maxRetries", v)}
                    displayValue={`${draft.download.maxRetries} 次`}
                  />
                  <ToggleItem
                    label="校验文件完整性"
                    value={draft.download.verifyChecksum}
                    onChange={(v) => setDraft("download", "verifyChecksum", v)}
                  />
                </div>
              </Show>

              <Show when={activeTab() === "connection"}>
                <div class="flex flex-col gap-5">
                  <SliderItem
                    label="每个主机最大连接数"
                    value={draft.connection.maxConnectionsPerHost}
                    min={1}
                    max={16}
                    onChange={(v) =>
                      setDraft("connection", "maxConnectionsPerHost", v)
                    }
                    displayValue={`${draft.connection.maxConnectionsPerHost}`}
                  />
                  <SliderItem
                    label="连接超时"
                    value={draft.connection.connectTimeoutSecs}
                    min={5}
                    max={120}
                    onChange={(v) =>
                      setDraft("connection", "connectTimeoutSecs", v)
                    }
                    displayValue={`${draft.connection.connectTimeoutSecs} 秒`}
                  />
                  <ToggleItem
                    label="启用 HTTP/2"
                    value={draft.connection.enableHttp2}
                    onChange={(v) => setDraft("connection", "enableHttp2", v)}
                  />
                  <ToggleItem
                    label="启用 QUIC"
                    value={draft.connection.enableQuic}
                    onChange={(v) => setDraft("connection", "enableQuic", v)}
                  />
                </div>
              </Show>

              <Show when={activeTab() === "scheduler"}>
                <div class="flex flex-col gap-5">
                  <SliderItem
                    label="最小分片大小"
                    value={draft.scheduler.minFragmentSize}
                    min={262144}
                    max={10485760}
                    onChange={(v) =>
                      setDraft("scheduler", "minFragmentSize", v)
                    }
                    displayValue={`${(draft.scheduler.minFragmentSize / 1048576).toFixed(1)} MB`}
                  />
                  <SliderItem
                    label="最大分片大小"
                    value={draft.scheduler.maxFragmentSize}
                    min={10485760}
                    max={134217728}
                    onChange={(v) =>
                      setDraft("scheduler", "maxFragmentSize", v)
                    }
                    displayValue={`${(draft.scheduler.maxFragmentSize / 1048576).toFixed(0)} MB`}
                  />
                  <SliderItem
                    label="EWMA 平滑系数"
                    value={draft.scheduler.ewmaAlpha}
                    min={0.1}
                    max={0.9}
                    onChange={(v) => setDraft("scheduler", "ewmaAlpha", v)}
                    displayValue={draft.scheduler.ewmaAlpha.toFixed(2)}
                  />
                </div>
              </Show>

              <Show when={activeTab() === "about"}>
                <div
                  class="flex flex-col items-center gap-3"
                  style={{ padding: "40px 20px" }}
                >
                  <div
                    style={{
                      width: "48px",
                      height: "48px",
                      background:
                        "linear-gradient(135deg, var(--color-accent-primary) 0%, var(--color-accent-tertiary) 100%)",
                      "border-radius": "12px",
                      display: "flex",
                      "align-items": "center",
                      "justify-content": "center",
                      color: "var(--color-text-inverse)",
                      "font-size": "24px",
                      "font-weight": 700,
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
                    style={{
                      "font-size": "13px",
                      color: "var(--color-text-tertiary)",
                    }}
                  >
                    版本 0.1.0 · Rust + Tauri
                  </div>
                  <div
                    style={{
                      "font-size": "12px",
                      color: "var(--color-text-tertiary)",
                      "margin-top": "8px",
                    }}
                  >
                    高性能多线程下载加速器
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
              取消
            </Button>
            <Button
              variant="primary"
              size="sm"
              loading={$configLoading.get() || saving()}
              onClick={handleSave}
            >
              {saving() ? "保存中..." : "保存配置"}
            </Button>
          </div>
        </div>
      </div>

      {/* 配置保存确认对话框(P1-11) */}
      <ConfirmDialog
        open={confirmOpen()}
        title="确认修改配置"
        message="修改下载器配置可能影响安全相关字段（如下载目录、授权路径等），请确认是否继续？"
        confirmLabel="确认保存"
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
