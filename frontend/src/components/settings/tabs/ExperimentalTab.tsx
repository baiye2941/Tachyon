import type { SetStoreFunction } from "solid-js/store";
import { Show } from "solid-js";
import { tr } from "../../../i18n";
import SegmentedItem from "../items/SegmentedItem";
import ToggleItem from "../items/ToggleItem";
import type { ConfigDraft } from "../SettingsPanel";
import type { HfSourceMode } from "../../../types";

export interface ExperimentalTabProps {
  draft: ConfigDraft;
  isHuggingfaceEnabled: boolean;
  toggleHuggingface: () => void;
  setDraft: SetStoreFunction<ConfigDraft>;
}

export default function ExperimentalTab(props: ExperimentalTabProps) {
  const t = tr;
  return (
    <div class="flex flex-col gap-5">
      <ToggleItem
        label={t("experimental.huggingface")}
        value={props.isHuggingfaceEnabled}
        onChange={() => props.toggleHuggingface()}
      />
      <div
        style={{
          "font-size": "12px",
          color: "var(--color-text-tertiary)",
          "line-height": "1.5",
        }}
      >
        {t("experimental.huggingfaceDesc")}
      </div>
      <Show when={props.isHuggingfaceEnabled}>
        <SegmentedItem<HfSourceMode>
          label={t("settings.hub.sourceMode")}
          value={props.draft.hub.sourceMode}
          options={[
            { value: "official", label: t("settings.hub.sourceOfficial") },
            { value: "mirror", label: t("settings.hub.sourceMirror") },
            { value: "race", label: t("settings.hub.sourceRace") },
          ]}
          onChange={(v) => props.setDraft("hub", "sourceMode", v)}
        />
        <div
          style={{
            "font-size": "12px",
            color: "var(--color-text-tertiary)",
            "line-height": "1.5",
          }}
        >
          {t("settings.hub.sourceModeDesc")}
        </div>
      </Show>
    </div>
  );
}
