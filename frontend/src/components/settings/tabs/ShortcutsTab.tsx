import {
  For,
  Show,
  createSignal,
  createMemo,
  createEffect,
  onMount,
  onCleanup,
} from "solid-js";
import {
  groupedShortcuts,
  GROUP_LABEL_KEYS,
  platformKeys,
  type Shortcut,
  type ShortcutGroup,
} from "../../../commands/shortcuts";
import {
  getShortcutKeys,
  setShortcut,
  resetShortcut,
  resetAllShortcuts,
  findConflict,
} from "../../../stores/shortcuts";
import { tr, type MessageKey } from "../../../i18n";
import Button from "../../../shared/ui/Button";

const GROUP_ORDER: ShortcutGroup[] = ["global", "navigation", "task", "list"];

export default function ShortcutsTab() {
  const t = tr;
  const [recordingLabelKey, setRecordingLabelKey] = createSignal<MessageKey | null>(null);
  const [previewKeys, setPreviewKeys] = createSignal<string[]>([]);
  const groups = createMemo(() => groupedShortcuts());

  function startRecording(labelKey: MessageKey) {
    setRecordingLabelKey(labelKey);
    setPreviewKeys([]);
  }

  function cancelRecording() {
    setRecordingLabelKey(null);
    setPreviewKeys([]);
  }

  function saveRecording(labelKey: MessageKey) {
    const keys = previewKeys();
    if (keys.length === 0) return;
    if (findConflict(labelKey, keys)) return;
    setShortcut(labelKey, keys);
    cancelRecording();
  }

  function handleCaptureKeyDown(e: KeyboardEvent) {
    const labelKey = recordingLabelKey();
    if (!labelKey) return;

    // 导航/保留键不捕获，保留浏览器默认行为
    if (e.key === "Tab" || e.key === "F5") return;

    if (e.key === "Escape") {
      cancelRecording();
      return;
    }

    // 确认要捕获该键后再阻止默认行为
    e.preventDefault();
    e.stopPropagation();

    // 忽略单独的修饰键按下
    if (["Control", "Alt", "Shift", "Meta"].includes(e.key)) return;

    const isMac =
      typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);
    const modifiers: string[] = [];
    if (e.ctrlKey || (isMac && e.metaKey)) modifiers.push("Ctrl");
    if (e.altKey) modifiers.push("Alt");
    if (e.shiftKey) modifiers.push("Shift");
    if (e.metaKey && !isMac) modifiers.push("Meta");

    const mainKey =
      e.key === " " ? "Space" : e.key.length === 1 ? e.key.toUpperCase() : e.key;
    setPreviewKeys([...modifiers, mainKey]);
  }

  onMount(() => {
    window.addEventListener("keydown", handleCaptureKeyDown, true);
  });

  onCleanup(() => {
    window.removeEventListener("keydown", handleCaptureKeyDown, true);
  });

  return (
    <div class="flex flex-col gap-5">
      <div class="flex items-center justify-between">
        <span
          style={{
            "font-size": "12px",
            color: "var(--color-text-tertiary)",
          }}
        >
          {t("settings.shortcuts.systemConflictHint")}
        </span>
        <Button variant="secondary" size="sm" onClick={resetAllShortcuts}>
          {t("settings.shortcuts.resetAll")}
        </Button>
      </div>

      <For each={GROUP_ORDER}>
        {(group) => (
          <Show when={groups()[group].length > 0}>
            <div
              style={{
                "font-size": "11px",
                "font-weight": 600,
                color: "var(--color-text-tertiary)",
                "text-transform": "uppercase",
                "letter-spacing": "0.5px",
              }}
            >
              {t(GROUP_LABEL_KEYS[group])}
            </div>
            <div class="flex flex-col">
              <For each={groups()[group]}>
                {(s) => (
                  <ShortcutRow
                    shortcut={s}
                    recordingLabelKey={recordingLabelKey}
                    previewKeys={previewKeys}
                    onStartRecording={startRecording}
                    onSave={saveRecording}
                    onCancel={cancelRecording}
                  />
                )}
              </For>
            </div>
          </Show>
        )}
      </For>
    </div>
  );
}

interface ShortcutRowProps {
  shortcut: Shortcut;
  recordingLabelKey: () => MessageKey | null;
  previewKeys: () => string[];
  onStartRecording: (labelKey: MessageKey) => void;
  onSave: (labelKey: MessageKey) => void;
  onCancel: () => void;
}

function ShortcutRow(props: ShortcutRowProps) {
  const t = tr;
  const isMac =
    typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);

  const isRecording = () => props.recordingLabelKey() === props.shortcut.labelKey;
  const currentKeys = () => getShortcutKeys(props.shortcut.labelKey);
  const displayKeys = () => platformKeys(currentKeys(), isMac);
  const differsFromDefault = createMemo(
    () => JSON.stringify(currentKeys()) !== JSON.stringify(props.shortcut.keys),
  );

  const conflict = () => {
    if (!isRecording()) return undefined;
    const preview = props.previewKeys();
    if (preview.length === 0) return undefined;
    return findConflict(props.shortcut.labelKey, preview);
  };

  let rowRef: HTMLDivElement | undefined;

  createEffect(() => {
    if (!isRecording()) return;

    function handleMouseDown(e: MouseEvent) {
      if (rowRef && !rowRef.contains(e.target as Node)) {
        props.onCancel();
      }
    }

    document.addEventListener("mousedown", handleMouseDown, { once: true });

    onCleanup(() => {
      document.removeEventListener("mousedown", handleMouseDown);
    });
  });

  return (
    <div
      ref={rowRef}
      class="flex items-center justify-between"
      style={{
        padding: "8px 0",
        "border-bottom": "1px solid var(--color-border-subtle)",
      }}
    >
      <span style={{ "font-size": "13px", color: "var(--color-text-secondary)" }}>
        {t(props.shortcut.labelKey)}
      </span>

      <div class="flex items-center gap-3">
        <Show
          when={isRecording()}
          fallback={
            <>
              <span class="flex items-center gap-1">
                <For each={displayKeys()}>
                  {(key) => (
                    <kbd class="kbd" style={{ "font-size": "11px" }}>
                      {key}
                    </kbd>
                  )}
                </For>
                <Show when={displayKeys().length === 0}>
                  <span style={{ "font-size": "12px", color: "var(--color-text-tertiary)" }}>
                    {t("settings.shortcuts.unbound")}
                  </span>
                </Show>
              </span>
              <Button
                variant="ghost"
                size="sm"
                aria-label={t("settings.shortcuts.editAria", {
                  name: t(props.shortcut.labelKey),
                })}
                onClick={() => props.onStartRecording(props.shortcut.labelKey)}
              >
                {t("common.edit")}
              </Button>
              <Show when={differsFromDefault()}>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => resetShortcut(props.shortcut.labelKey)}
                >
                  {t("settings.shortcuts.resetOne")}
                </Button>
              </Show>
            </>
          }
        >
          <span class="flex items-center gap-1">
            <Show
              when={props.previewKeys().length > 0}
              fallback={
                <span style={{ "font-size": "12px", color: "var(--color-text-tertiary)" }}>
                  {t("settings.shortcuts.recording")}
                </span>
              }
            >
              <For each={platformKeys(props.previewKeys(), isMac)}>
                {(key) => (
                  <kbd class="kbd" style={{ "font-size": "11px" }}>
                    {key}
                  </kbd>
                )}
              </For>
            </Show>
          </span>
          <Show when={conflict()}>
            <span style={{ "font-size": "12px", color: "var(--color-error)" }}>
              {t("settings.shortcuts.conflict", { label: t(conflict()!) })}
            </span>
          </Show>
          <Button
            variant="brand"
            size="sm"
            disabled={props.previewKeys().length === 0 || Boolean(conflict())}
            onClick={() => props.onSave(props.shortcut.labelKey)}
          >
            {t("common.save")}
          </Button>
          <Button variant="secondary" size="sm" onClick={props.onCancel}>
            {t("common.cancel")}
          </Button>
        </Show>
      </div>
    </div>
  );
}
