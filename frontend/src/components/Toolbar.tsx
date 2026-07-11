import { Show } from "solid-js";
import { useIsNarrowScreen } from "../hooks/useMediaQuery";
import { $ui } from "../stores/ui";
import ToolbarDefault from "./toolbar/ToolbarDefault";
import ToolbarBatch from "./toolbar/ToolbarBatch";
import ToolbarHub from "./toolbar/ToolbarHub";

export interface FilterTag {
  type: string;
  value: string;
  raw: string;
}

export interface ToolbarProps {
  searchQuery: string;
  onSearchChange: (q: string) => void;
  filters: FilterTag[];
  onRemoveFilter: (raw: string) => void;
  isMultiSelectMode: boolean;
  onToggleMultiSelect: () => void;
  selectedCount: number;
  totalCount: number;
  onSelectAll: () => void;
  onPauseSelected: () => void;
  onResumeSelected: () => void;
  onCancelSelected: () => void;
  onDeleteSelected: () => void;
  onOpenSelectedFolders: () => void;
  onCopySelectedLinks: () => void;
  onRedownloadSelected: () => void;
  onClearSelection: () => void;
  onExitMultiSelect: () => void;
  listDensity: "comfortable" | "compact";
  onToggleDensity: () => void;
  onNewTask: () => void;
  onOpenSettings: () => void;
  onPauseAll: () => void;
  onResumeAll: () => void;
  onCancelAll: () => void;
  groupBy?: "none" | "status";
  onToggleGroupBy?: () => void;
}

export default function Toolbar(props: ToolbarProps) {
  const isNarrow = useIsNarrowScreen();

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
      <Show
        when={props.isMultiSelectMode}
        fallback={
          <Show
            when={$ui.hubVisible()}
            fallback={<ToolbarDefault {...props} />}
          >
            <ToolbarHub />
          </Show>
        }
      >
        <ToolbarBatch {...props} />
      </Show>
    </div>
  );
}
