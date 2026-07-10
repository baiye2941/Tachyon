import { Show, For, createMemo } from "solid-js";
import { CloseIcon } from "./icons";
import Button from "../shared/ui/Button";
import {
  groupedShortcuts,
  GROUP_LABEL_KEYS,
  platformKeys,
  type ShortcutGroup,
} from "../commands/shortcuts";
import { getShortcutKeys } from "../stores/shortcuts";
import { useFocusTrap } from "../hooks/useFocusTrap";
import { tr, type MessageKey } from "../i18n";

interface ShortcutHelpProps {
  visible: boolean;
  onClose: () => void;
}

/**
 * 快捷键帮助页(Iteration 07,DI-2)。
 *
 * `Ctrl+/` 或 `?` 触发。分组渲染全部快捷键,role=dialog 焦点陷阱 Esc 关闭。
 * 数据从 commands/shortcuts.ts 派生(单一来源)。
 */
export default function ShortcutHelp(props: ShortcutHelpProps) {
  let panelRef: HTMLDivElement | undefined;
  const t = (key: MessageKey) => tr(key);

  // macOS 检测(显示 Cmd 替代 Ctrl)
  const isMac =
    typeof navigator !== "undefined" &&
    /Mac|iPhone|iPad/.test(navigator.platform);
  const groups = createMemo(() => groupedShortcuts());
  const groupOrder: ShortcutGroup[] = ["global", "navigation", "task", "list"];

  useFocusTrap({
    active: () => props.visible,
    container: panelRef,
    onEscape: () => props.onClose(),
  });

  return (
    <Show when={props.visible}>
      <div
        class="panel-overlay"
        style={{ opacity: 1, transition: "opacity 200ms ease" }}
        onClick={() => props.onClose()}
      />
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="shortcut-help-title"
        tabindex={-1}
        class="fixed z-[var(--z-panel-content)] flex flex-col panel-surface"
        style={{
          top: "50%",
          left: "50%",
          transform: "translate(-50%, -50%)",
          width: "480px",
          "max-height": "70vh",
          "border-radius": "var(--radius-lg)",
          "box-shadow": "var(--shadow-xl)",
          overflow: "hidden",
          animation: "fadeIn 150ms ease forwards",
          outline: "none",
        }}
      >
        {/* Header */}
        <div class="panel-header">
          <span
            id="shortcut-help-title"
            style={{
              "font-size": "15px",
              "font-weight": 600,
              color: "var(--color-text-title)",
            }}
          >
            {t("shortcutHelp.title")}
          </span>
          <Button
            variant="ghost"
            shape="icon-sm"
            class="hover-light"
            aria-label={t("common.close")}
            onClick={() => props.onClose()}
          >
            <CloseIcon />
          </Button>
        </div>

        {/* 快捷键分组列表 */}
        <div class="flex-1 scroll-container" style={{ padding: "8px 0" }}>
          <For each={groupOrder}>
            {(gkey) => (
              <Show when={groups()[gkey].length > 0}>
                <div
                  style={{
                    padding: "8px 20px 4px",
                    "font-size": "10px",
                    "font-weight": 600,
                    "text-transform": "uppercase",
                    "letter-spacing": "0.5px",
                    color: "var(--color-text-tertiary)",
                  }}
                >
                  {t(GROUP_LABEL_KEYS[gkey])}
                </div>
                <For each={groups()[gkey]}>
                  {(s) => (
                    <div
                      class="flex items-center justify-between"
                      style={{ padding: "6px 20px", "font-size": "13px" }}
                    >
                      <span style={{ color: "var(--color-text-secondary)" }}>
                        {t(s.labelKey)}
                      </span>
                      <span class="flex items-center gap-1">
                        <For each={platformKeys(getShortcutKeys(s.labelKey), isMac)}>
                          {(key) => (
                            <kbd
                              class="mono rounded px-1.5 py-0.5"
                              style={{
                                "font-size": "11px",
                                color: "var(--color-text-secondary)",
                                background: "var(--color-bg-raised)",
                                border: "1px solid var(--color-border-default)",
                                "min-width": "20px",
                                "text-align": "center",
                              }}
                            >
                              {key}
                            </kbd>
                          )}
                        </For>
                      </span>
                    </div>
                  )}
                </For>
              </Show>
            )}
          </For>
        </div>

        {/* 底部提示 */}
        <div
          class="flex items-center"
          style={{
            padding: "10px 20px",
            "border-top": "1px solid var(--color-border-subtle)",
            "font-size": "11px",
            color: "var(--color-text-tertiary)",
          }}
        >
          {t("shortcutHelp.macHint")}
        </div>
      </div>
    </Show>
  );
}
