import type { SetStoreFunction } from "solid-js/store";
import { tr } from "../../../i18n";
import SliderItem from "../items/SliderItem";
import type { ConfigDraft } from "../SettingsPanel";

export interface SchedulerTabProps {
  draft: ConfigDraft;
  setDraft: SetStoreFunction<ConfigDraft>;
}

export default function SchedulerTab(props: SchedulerTabProps) {
  const t = tr;
  return (
    <div class="flex flex-col gap-5">
      <SliderItem
        label={t("settings.scheduler.minFragmentSize")}
        value={props.draft.scheduler.minFragmentSize}
        min={262144}
        max={10485760}
        onChange={(v) =>
          props.setDraft("scheduler", "minFragmentSize", v)
        }
        displayValue={`${(props.draft.scheduler.minFragmentSize / 1048576).toFixed(1)} MB`}
      />
      <SliderItem
        label={t("settings.scheduler.maxFragmentSize")}
        value={props.draft.scheduler.maxFragmentSize}
        min={10485760}
        max={134217728}
        onChange={(v) =>
          props.setDraft("scheduler", "maxFragmentSize", v)
        }
        displayValue={`${(props.draft.scheduler.maxFragmentSize / 1048576).toFixed(0)} MB`}
      />
      <SliderItem
        label={t("settings.scheduler.ewmaAlpha")}
        value={props.draft.scheduler.ewmaAlpha}
        min={0.1}
        max={0.9}
        step={0.05}
        onChange={(v) => props.setDraft("scheduler", "ewmaAlpha", v)}
        displayValue={props.draft.scheduler.ewmaAlpha.toFixed(2)}
      />
    </div>
  );
}
