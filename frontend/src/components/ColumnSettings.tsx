import { For, onMount, onCleanup } from "solid-js";
import { ALL_COLUMNS, type ColumnKey } from "./taskColumns";
import { tr } from "../i18n";

interface ColumnSettingsProps {
  visibleKeys: () => ColumnKey[];
  onToggle: (key: ColumnKey) => void;
  onReset: () => void;
  onClose: () => void;
}

export default function ColumnSettings(props: ColumnSettingsProps) {
  let panelRef: HTMLDivElement | undefined;

  onMount(() => {
    function handleClickOutside(e: MouseEvent) {
      if (panelRef && !panelRef.contains(e.target as Node)) {
        props.onClose();
      }
    }

    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") {
        props.onClose();
      }
    }

    document.addEventListener("mousedown", handleClickOutside);
    document.addEventListener("keydown", handleKeyDown);

    onCleanup(() => {
      document.removeEventListener("mousedown", handleClickOutside);
      document.removeEventListener("keydown", handleKeyDown);
    });
  });

  return (
    <div
      ref={panelRef}
      class="column-settings-panel"
      role="dialog"
      aria-modal="true"
      aria-label={tr("taskList.columns.title")}
      onClick={(e) => e.stopPropagation()}
    >
      <div class="column-settings-title">{tr("taskList.columns.title")}</div>
      <div class="column-settings-list">
        <For each={ALL_COLUMNS}>
          {(col) => {
            const disabled = col.key === "name";
            const checked = () => props.visibleKeys().includes(col.key);
            return (
              <label
                class="column-settings-item"
                classList={{ "column-settings-item--disabled": disabled }}
              >
                <input
                  type="checkbox"
                  checked={checked()}
                  disabled={disabled}
                  onChange={() => props.onToggle(col.key)}
                />
                <span>{tr(col.labelKey)}</span>
              </label>
            );
          }}
        </For>
      </div>
      <button
        type="button"
        class="column-settings-reset"
        onClick={() => {
          props.onReset();
          props.onClose();
        }}
      >
        {tr("taskList.columns.reset")}
      </button>
    </div>
  );
}
