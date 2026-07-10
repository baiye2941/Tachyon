import { For, Show } from "solid-js";
import {
  PlusIcon,
  SearchIcon,
  PauseIcon,
  PlayIcon,
  SettingsIcon,
  SelectIcon,
  XIcon,
  CancelIcon,
  StackIcon,
  ListBulletsIcon,
} from "../icons";
import Button from "../../shared/ui/Button";
import { useI18n } from "../../i18n";
import { useIsNarrowScreen } from "../../hooks/useMediaQuery";
import type { ToolbarProps } from "../Toolbar";

export default function ToolbarDefault(props: ToolbarProps) {
  const i18n = useI18n();
  const isNarrow = useIsNarrowScreen();

  return (
    <div class="flex items-center gap-2 flex-1 min-w-0">
      <Button
        variant="primary"
        size="md"
        class="new-download-btn"
        onClick={props.onNewTask}
        aria-label={i18n.t("toolbar.newDownload") as string}
        title={i18n.t("toolbar.newDownload") as string}
      >
        <PlusIcon />
        <span>
          {i18n.t(
            isNarrow()
              ? "toolbar.newDownloadShort"
              : "toolbar.newDownload",
          )}
        </span>
      </Button>

      <div class="relative flex items-center gap-2 min-w-0 flex-1">
        <div class="relative flex-1 min-w-0" style={{ "max-width": "420px" }}>
          <div
            class="absolute pointer-events-none flex items-center justify-center"
            style={{
              left: "10px",
              top: "50%",
              transform: "translateY(-50%)",
              width: "16px",
              height: "16px",
              color: "var(--color-text-tertiary)",
            }}
          >
            <SearchIcon />
          </div>
          <input
            type="text"
            placeholder={i18n.t("toolbar.searchPlaceholder") as string}
            value={props.searchQuery}
            onInput={(e) => props.onSearchChange(e.currentTarget.value)}
            class="input"
            style={{
              width: "100%",
              height: "34px",
              "padding-left": "34px",
              "padding-right": "12px",
              "padding-top": "0",
              "padding-bottom": "0",
              "font-size": "13px",
              "line-height": "34px",
            }}
          />
        </div>

        <Show when={props.filters.length > 0}>
          <div class="flex items-center gap-1.5 flex-shrink min-w-0 overflow-hidden">
            <For each={props.filters}>
              {(filter) => (
                <div
                  class="flex items-center gap-1 flex-shrink-0"
                  style={{
                    padding: "2px 8px",
                    height: "22px",
                    "border-radius": "11px",
                    "font-size": "11px",
                    "font-weight": 500,
                    background: "var(--color-bg-hover)",
                    border: `1px solid ${getFilterBorderColor(filter.type)}`,
                    color: getFilterColor(filter.type),
                    "white-space": "nowrap",
                  }}
                >
                  <span
                    style={{
                      "max-width": "120px",
                      overflow: "hidden",
                      "text-overflow": "ellipsis",
                    }}
                  >
                    {filter.raw}
                  </span>
                  <button
                    class="flex items-center justify-center"
                    style={{
                      width: "12px",
                      height: "12px",
                      background: "none",
                      border: "none",
                      cursor: "pointer",
                      color: "inherit",
                      opacity: 0.6,
                    }}
                    onClick={() => props.onRemoveFilter(filter.raw)}
                    aria-label={
                      i18n.t("toolbar.removeFilter", {
                        filter: filter.raw,
                      }) as string
                    }
                  >
                    <XIcon />
                  </button>
                </div>
              )}
            </For>
          </div>
        </Show>
      </div>

      <div class="flex-1" />

      <Button
        variant="ghost"
        shape="icon"
        title={i18n.t("toolbar.pauseAll") as string}
        aria-label={i18n.t("toolbar.pauseAll") as string}
        onClick={props.onPauseAll}
      >
        <PauseIcon />
      </Button>

      <Button
        variant="ghost"
        shape="icon"
        title={i18n.t("toolbar.resumeAll") as string}
        aria-label={i18n.t("toolbar.resumeAll") as string}
        onClick={props.onResumeAll}
      >
        <PlayIcon />
      </Button>

      <Button
        variant="ghost"
        shape="icon"
        title={i18n.t("toolbar.cancelAll") as string}
        aria-label={i18n.t("toolbar.cancelAll") as string}
        onClick={props.onCancelAll}
      >
        <CancelIcon />
      </Button>

      <Button
        variant="ghost"
        shape="icon"
        title={i18n.t("toolbar.settings") as string}
        aria-label={i18n.t("toolbar.settings") as string}
        onClick={props.onOpenSettings}
      >
        <SettingsIcon />
      </Button>

      <Button
        variant="ghost"
        shape="icon"
        title={i18n.t("toolbar.multiSelect") as string}
        aria-label={i18n.t("toolbar.multiSelect") as string}
        onClick={props.onToggleMultiSelect}
      >
        <SelectIcon />
      </Button>

      <Show when={props.onToggleGroupBy}>
        <Button
          variant="ghost"
          shape="icon"
          title={
            i18n.t(
              props.groupBy === "status"
                ? "taskList.view.flat"
                : "taskList.view.grouped",
            ) as string
          }
          aria-label={
            i18n.t("taskList.aria.toggleGroupBy", {
              mode:
                props.groupBy === "status"
                  ? (i18n.t("taskList.view.flat") as string)
                  : (i18n.t("taskList.view.grouped") as string),
            }) as string
          }
          onClick={props.onToggleGroupBy}
        >
          <Show
            when={props.groupBy === "status"}
            fallback={<StackIcon />}
          >
            <ListBulletsIcon />
          </Show>
        </Button>
      </Show>
    </div>
  );
}

function getFilterColor(type: string): string {
  switch (type) {
    case "status":
      return "var(--color-accent-primary)";
    case "type":
      return "var(--color-status-connecting)";
    case "size":
      return "var(--color-warning)";
    case "speed":
      return "var(--color-speed-active)";
    case "name":
      return "var(--color-text-secondary)";
    default:
      return "var(--color-text-secondary)";
  }
}

function getFilterBorderColor(type: string): string {
  switch (type) {
    case "status":
      return "var(--color-accent-glow)";
    case "type":
      return "var(--color-info-soft)";
    case "size":
      return "var(--color-warning-soft)";
    case "speed":
      return "var(--color-speed-soft)";
    case "name":
      return "var(--color-border-default)";
    default:
      return "var(--color-border-default)";
  }
}
