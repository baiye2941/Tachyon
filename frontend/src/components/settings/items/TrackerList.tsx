import { createSignal, Show, For } from "solid-js";
import { tr } from "../../../i18n";
import Button from "../../../shared/ui/Button";
import { isValidTrackerUrl } from "../utils";

export interface TrackerListProps {
  trackers: string[];
  onAdd: (url: string) => void;
  onRemove: (index: number) => void;
  onImportPresets: () => void;
  onClearAll: () => void;
}

export default function TrackerList(props: TrackerListProps) {
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
