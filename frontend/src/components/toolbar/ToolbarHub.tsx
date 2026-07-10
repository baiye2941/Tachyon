import { Show, onCleanup } from "solid-js";
import { SearchIcon } from "../icons";
import Button from "../../shared/ui/Button";
import { useI18n } from "../../i18n";
import { $model, searchRemoteModels } from "../../stores/model";

export default function ToolbarHub() {
  const i18n = useI18n();

  let searchTimer: ReturnType<typeof setTimeout> | null = null;

  const handleModelSearchInput = (val: string) => {
    $model.setSearchQuery(val);
    if (searchTimer) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      searchRemoteModels(val);
    }, 500);
  };

  onCleanup(() => {
    if (searchTimer) {
      clearTimeout(searchTimer);
      searchTimer = null;
    }
  });

  return (
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
  );
}
