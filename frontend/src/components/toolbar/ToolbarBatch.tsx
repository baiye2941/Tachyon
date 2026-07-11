import { Show } from "solid-js";
import {
  CheckboxIcon,
  PauseIcon,
  PlayIcon,
  CancelIcon,
  TrashIcon,
  XIcon,
  ArrowLeftIcon,
  FolderOpenIcon,
  LinkIcon,
  RefreshIcon,
} from "../icons";
import Button from "../../shared/ui/Button";
import { useI18n } from "../../i18n";
import { useIsNarrowScreen } from "../../hooks/useMediaQuery";
import type { ToolbarProps } from "../Toolbar";

export default function ToolbarBatch(props: ToolbarProps) {
  const i18n = useI18n();
  const isNarrow = useIsNarrowScreen();

  const isAllSelected = () =>
    props.totalCount > 0 && props.selectedCount === props.totalCount;
  const isPartialSelected = () =>
    props.selectedCount > 0 && props.selectedCount < props.totalCount;
  const selectLabel = () =>
    isAllSelected()
      ? (i18n.t("toolbar.deselectAll") as string)
      : (i18n.t("toolbar.selectAll") as string);

  return (
    <div class="flex items-center gap-2 flex-1 min-w-0">
      <Button
        variant="ghost"
        size="md"
        onClick={props.onSelectAll}
        aria-label={selectLabel()}
        title={selectLabel()}
        aria-pressed={isAllSelected()}
      >
        <CheckboxIcon
          checked={isAllSelected()}
          indeterminate={isPartialSelected()}
        />
        <Show when={!isNarrow()}>
          <span>{selectLabel()}</span>
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

      <div
        style={{
          width: "1px",
          height: "18px",
          background: "var(--color-border-subtle)",
          margin: "0 2px",
        }}
      />

      <Button
        variant="ghost"
        size="md"
        onClick={props.onOpenSelectedFolders}
        aria-label={i18n.t("toolbar.openFolder") as string}
        title={i18n.t("toolbar.openFolder") as string}
      >
        <FolderOpenIcon />
        <Show when={!isNarrow()}>
          <span>{i18n.t("toolbar.openFolder")}</span>
        </Show>
      </Button>

      <Button
        variant="ghost"
        size="md"
        onClick={props.onCopySelectedLinks}
        aria-label={i18n.t("toolbar.copyLink") as string}
        title={i18n.t("toolbar.copyLink") as string}
      >
        <LinkIcon />
        <Show when={!isNarrow()}>
          <span>{i18n.t("toolbar.copyLink")}</span>
        </Show>
      </Button>

      <Button
        variant="ghost"
        size="md"
        onClick={props.onRedownloadSelected}
        aria-label={i18n.t("toolbar.redownload") as string}
        title={i18n.t("toolbar.redownload") as string}
      >
        <RefreshIcon />
        <Show when={!isNarrow()}>
          <span>{i18n.t("toolbar.redownload")}</span>
        </Show>
      </Button>

      <div
        style={{
          width: "1px",
          height: "18px",
          background: "var(--color-border-subtle)",
          margin: "0 2px",
        }}
      />

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
        onClick={props.onClearSelection}
        aria-label={i18n.t("toolbar.clearSelection") as string}
        title={i18n.t("toolbar.clearSelection") as string}
      >
        <XIcon />
        <Show when={!isNarrow()}>
          <span>{i18n.t("toolbar.clearSelection")}</span>
        </Show>
      </Button>

      <Button
        variant="ghost"
        size="md"
        onClick={props.onExitMultiSelect}
        aria-label={i18n.t("toolbar.exitMultiSelect") as string}
        title={i18n.t("toolbar.exitMultiSelect") as string}
      >
        <ArrowLeftIcon />
        <Show when={!isNarrow()}>
          <span>{i18n.t("toolbar.exitMultiSelect")}</span>
        </Show>
      </Button>
    </div>
  );
}
