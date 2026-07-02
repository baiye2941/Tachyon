import { For, Show } from "solid-js";
import {
  PlusIcon,
  SearchIcon,
  PauseIcon,
  PlayIcon,
  SettingsIcon,
  SelectIcon,
  CheckboxIcon,
  XIcon,
  TrashIcon,
  CancelIcon,
} from "./icons";
import Button from "../shared/ui/Button";
import { useI18n } from "../i18n";
import { useIsNarrowScreen } from "../hooks/useMediaQuery";
import { $ui } from "../stores/ui";
import { $model, searchRemoteModels } from "../stores/model";

function getFilterColor(type: string): string {
  switch (type) {
    case "status":
      return "var(--color-accent-primary)"; // 电青 - 状态筛选
    case "type":
      return "var(--color-status-connecting)"; // cyan-400 - 类型筛选
    case "size":
      return "var(--color-warning)"; // amber - 大小筛选
    case "speed":
      return "var(--color-speed-active)"; // Neon Cyan - 速度筛选
    case "name":
      return "var(--color-text-secondary)"; // silver - 名称筛选
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

interface FilterTag {
  type: string;
  value: string;
  raw: string;
}

interface ToolbarProps {
  searchQuery: string;
  onSearchChange: (q: string) => void;
  filters: FilterTag[];
  onRemoveFilter: (raw: string) => void;
  isMultiSelectMode: boolean;
  onToggleMultiSelect: () => void;
  selectedCount: number;
  onSelectAll: () => void;
  onPauseSelected: () => void;
  onResumeSelected: () => void;
  onCancelSelected: () => void;
  onDeleteSelected: () => void;
  onExitMultiSelect: () => void;
  listDensity: "comfortable" | "compact";
  onToggleDensity: () => void;
  onNewTask: () => void;
  onOpenSettings: () => void;
  onPauseAll: () => void;
  onResumeAll: () => void;
  onCancelAll: () => void;
}

export default function Toolbar(props: ToolbarProps) {
  const i18n = useI18n();
  const isNarrow = useIsNarrowScreen();

  let searchTimer: ReturnType<typeof setTimeout> | null = null;

  const handleModelSearchInput = (val: string) => {
    $model.setSearchQuery(val);
    if (searchTimer) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      searchRemoteModels(val);
    }, 500);
  };

  return (
    <div
      class="flex items-center justify-between flex-shrink-0"
      style={{
        height: "56px",
        padding: isNarrow() ? "0 8px" : "0 16px",
        "border-bottom": "1px solid var(--color-border-subtle)",
        "box-shadow": "var(--shadow-inset-top)",
        gap: "8px",
        position: "relative",
        "z-index": "2",
      }}
    >
      {props.isMultiSelectMode ? (
        <div class="flex items-center gap-2 flex-1 min-w-0">
          <Button
            variant="ghost"
            size="md"
            onClick={props.onSelectAll}
            aria-label={i18n.t("toolbar.selectAll") as string}
            title={i18n.t("toolbar.selectAll") as string}
          >
            <CheckboxIcon checked={props.selectedCount > 0} />
            <Show when={!isNarrow()}>
              <span>{i18n.t("toolbar.selectAll")}</span>
            </Show>
          </Button>

          <Show when={!isNarrow()}>
            <span
              style={{
                "font-size": "14px",
                color: "var(--color-text-secondary)",
              }}
            >
              {i18n.t("toolbar.selectedCount", { count: props.selectedCount })}
            </span>
          </Show>

          <div class="flex-1" />

          <Button
            variant="ghost"
            size="md"
            onClick={props.onPauseSelected}
            aria-label={i18n.t("toolbar.pause") as string}
            title={i18n.t("toolbar.pause") as string}
          >
            <PauseIcon />
            <Show when={!isNarrow()}>
              <span>{i18n.t("toolbar.pause")}</span>
            </Show>
          </Button>

          <Button
            variant="ghost"
            size="md"
            onClick={props.onResumeSelected}
            aria-label={i18n.t("toolbar.resume") as string}
            title={i18n.t("toolbar.resume") as string}
          >
            <PlayIcon />
            <Show when={!isNarrow()}>
              <span>{i18n.t("toolbar.resume")}</span>
            </Show>
          </Button>

          <Button
            variant="ghost"
            size="md"
            onClick={props.onCancelSelected}
            aria-label={i18n.t("toolbar.cancel") as string}
            title={i18n.t("toolbar.cancel") as string}
          >
            <CancelIcon />
            <Show when={!isNarrow()}>
              <span>{i18n.t("toolbar.cancel")}</span>
            </Show>
          </Button>

          <Button
            variant="danger"
            size="md"
            onClick={props.onDeleteSelected}
            aria-label={i18n.t("toolbar.delete") as string}
            title={i18n.t("toolbar.delete") as string}
          >
            <TrashIcon />
            <Show when={!isNarrow()}>
              <span>{i18n.t("toolbar.delete")}</span>
            </Show>
          </Button>

          <Button
            variant="ghost"
            size="md"
            onClick={props.onExitMultiSelect}
            aria-label={i18n.t("toolbar.exit") as string}
            title={i18n.t("toolbar.exit") as string}
          >
            <XIcon />
            <Show when={!isNarrow()}>
              <span>{i18n.t("toolbar.exit")}</span>
            </Show>
          </Button>
        </div>
      ) : (
        <Show
          when={$ui.hubVisible()}
          fallback={
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
            </div>
          }
        >
          {/* Hub 视图工具栏 */}
          <div class="flex items-center gap-2 flex-1 min-w-0">
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
                  placeholder={i18n.t("hub.search.placeholder") as string}
                  value={$model.searchQuery()}
                  onInput={(e) => handleModelSearchInput(e.currentTarget.value)}
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
            </div>

            <div class="flex-1" />

            {/* 来源筛选按钮 */}
            <Show when={$model.sourceFilter() !== "local"}>
              <Button
                variant="ghost"
                size="md"
                onClick={() => $model.setSourceFilter("local")}
              >
                {i18n.t("hub.tab.local")}
              </Button>
            </Show>
            <Show when={$model.sourceFilter() !== "remote"}>
              <Button
                variant="ghost"
                size="md"
                onClick={() => $model.setSourceFilter("remote")}
              >
                {i18n.t("hub.tab.remote")}
              </Button>
            </Show>
            <Show when={$model.sourceFilter() !== "favorite"}>
              <Button
                variant="ghost"
                size="md"
                onClick={() => $model.setSourceFilter("favorite")}
              >
                {i18n.t("hub.tab.favorite")}
              </Button>
            </Show>
          </div>
        </Show>
      )}
    </div>
  );
}
