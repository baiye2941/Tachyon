import { Show } from "solid-js";
import {
  CheckboxIcon,
  PauseIcon,
  PlayIcon,
  CancelIcon,
  TrashIcon,
  XIcon,
} from "../icons";
import Button from "../../shared/ui/Button";
import { useI18n } from "../../i18n";
import { useIsNarrowScreen } from "../../hooks/useMediaQuery";
import type { ToolbarProps } from "../Toolbar";

export default function ToolbarBatch(props: ToolbarProps) {
  const i18n = useI18n();
  const isNarrow = useIsNarrowScreen();

  return (
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
  );
}
