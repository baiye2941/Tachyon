import type { SetStoreFunction } from "solid-js/store";
import Button from "../../../shared/ui/Button";
import ToggleItem from "../items/ToggleItem";
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

      <ToggleItem
        label={t("settings.notifications.enabled")}
        value={props.draft.notifications.enabled}
        onChange={(v) => props.setDraft("notifications", "enabled", v)}
      />
      <div
        style={{
          "font-size": "11px",
          color: "var(--color-text-tertiary)",
          "margin-top": "-12px",
        }}
      >
        {t("settings.notifications.enabledHint")}
      </div>

      {/* P1-23-A: 剪贴板监听开关 */}
      <ToggleItem
        label={t("settings.clipboard.enableWatch")}
        value={props.draft.clipboard.enableWatch}
        onChange={(v) => props.setDraft("clipboard", "enableWatch", v)}
      />
      <div
        style={{
          "font-size": "11px",
          color: props.draft.clipboard.enableWatch
            ? "var(--color-warning)"
            : "var(--color-text-tertiary)",
          "margin-top": "-12px",
        }}
      >
        {props.draft.clipboard.enableWatch
          ? t("settings.clipboard.restartRequired")
          : t("settings.clipboard.enableWatchHint")}
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
