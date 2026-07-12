import type { SetStoreFunction } from "solid-js/store";
import Button from "../../../shared/ui/Button";
import { tr } from "../../../i18n";
import type { ConfigDraft } from "../SettingsPanel";

export interface GeneralTabProps {
  draft: ConfigDraft;
  setDraft: SetStoreFunction<ConfigDraft>;
  onChooseDownloadDir: () => void;
  onExportBackup: () => void;
  onImportBackup: () => void;
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

      <div
        style={{
          "border-top": "1px solid var(--color-border-subtle)",
          "padding-top": "16px",
        }}
      >
        <div
          style={{
            "font-size": "14px",
            "font-weight": 600,
            color: "var(--color-text-title)",
            "margin-bottom": "12px",
          }}
        >
          {t("settings.backup.title")}
        </div>
        <div class="flex flex-col gap-4">
          <div>
            <div
              style={{
                "font-size": "13px",
                color: "var(--color-text-secondary)",
                "margin-bottom": "8px",
              }}
            >
              {t("settings.backup.exportDescription")}
            </div>
            <Button
              variant="secondary"
              size="sm"
              onClick={props.onExportBackup}
            >
              {t("settings.backup.export")}
            </Button>
          </div>
          <div>
            <div
              style={{
                "font-size": "13px",
                color: "var(--color-text-secondary)",
                "margin-bottom": "8px",
              }}
            >
              {t("settings.backup.importDescription")}
            </div>
            <Button
              variant="secondary"
              size="sm"
              onClick={props.onImportBackup}
            >
              {t("settings.backup.import")}
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}
