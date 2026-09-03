import { LocaleType, Univer } from "@univerjs/core";
import { UniverFormulaEnginePlugin } from "@univerjs/engine-formula";
import { UniverRPCWorkerThreadPlugin } from "@univerjs/rpc";
import { UniverSheetsPlugin } from "@univerjs/sheets";
import { UniverRemoteSheetsFormulaPlugin } from "@univerjs/sheets-formula";

const univer = new Univer({
  locale: LocaleType.EN_US,
  locales: {
    [LocaleType.EN_US]: {},
  },
});

univer.registerPlugin(UniverSheetsPlugin, {
  onlyRegisterFormulaRelatedMutations: true,
});
univer.registerPlugin(UniverFormulaEnginePlugin);
univer.registerPlugin(UniverRPCWorkerThreadPlugin);
univer.registerPlugin(UniverRemoteSheetsFormulaPlugin);
