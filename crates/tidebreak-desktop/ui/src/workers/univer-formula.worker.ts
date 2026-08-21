import sheetsCoreEnUS from "@univerjs/preset-sheets-core/locales/en-US";
import { UniverSheetsCoreWorkerPreset } from "@univerjs/preset-sheets-core/worker";
import { createUniver, LocaleType } from "@univerjs/presets";

createUniver({
  locale: LocaleType.EN_US,
  locales: {
    [LocaleType.EN_US]: sheetsCoreEnUS,
  },
  presets: [UniverSheetsCoreWorkerPreset()],
});
