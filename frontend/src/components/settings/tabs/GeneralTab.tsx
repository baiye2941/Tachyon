import type { SetStoreFunction } from "solid-js/store";
import Button from "../../../shared/ui/Button";
import { tr } from "../../../i18n";
import type { ConfigDraft } from "../SettingsPanel";

export interface GeneralTabProps {
  draft: ConfigDraft;
  setDraft: SetStoreFunction<ConfigDraft>;
  onChooseDownloadDir: () => void;
}

export default function GeneralTab(props: GeneralTabProps) {
  const t = tr;
  return (
    <div class="flex flex-col gap-5">
      <div>
        <div
          style={{
            "font-size": "13px",
            color: "var(--color-text-secondary)",
            "margin-bottom": "8px",
          }}
        >
          {t("settings.general.defaultDir")}
        </div>
        <div class="flex items-center gap-2">
          <input
            type="text"
            class="input flex-1"
            value={props.draft.download.downloadDir}
            onInput={(e) =>
              props.setDraft(
                "download",
                "downloadDir",
                e.currentTarget.value,
              )
            }
            style={{ "font-size": "13px" }}
          />
          <Button
            variant="secondary"
            size="sm"
            onClick={props.onChooseDownloadDir}
          >
            {t("common.browse")}
          </Button>
        </div>
      </div>
    </div>
  );
}
