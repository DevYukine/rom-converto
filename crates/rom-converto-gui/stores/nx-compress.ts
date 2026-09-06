import { defineStore } from "pinia";
import { useUiStore } from "~/stores/ui";

export type NxMode = "solid" | "block";

export function isXciInput(input: string): boolean {
  return input.toLowerCase().endsWith(".xci");
}

export const useNxCompressStore = defineStore("nx-compress", () => {
  const ui = useUiStore();
  const recursive = ref(true);
  const maxDepth = ref<number | null>(null);
  const output = ref("");
  const keys = ref("");
  const level = ref<number>(18);
  // Defaults follow nsz: solid for NSP, block for XCI. The auto switch
  // only kicks in when the user has not deliberately picked a mode.
  const mode = ref<NxMode>("solid");
  const blockSizeExp = ref<number>(20);
  const onConflict = ref(ui.defaultOnConflict);
  const skipSpaceCheck = ref(false);
  const outputTemplate = ref("");
  const reportFile = ref("");
  const userPickedMode = ref(false);
  const verifyAfter = ref(false);

  function setMode(m: NxMode) {
    mode.value = m;
    userPickedMode.value = true;
  }

  function $reset() {
    recursive.value = true;
    maxDepth.value = null;
    output.value = "";
    keys.value = "";
    level.value = 18;
    mode.value = "solid";
    blockSizeExp.value = 20;
    onConflict.value = ui.defaultOnConflict;
    skipSpaceCheck.value = false;
    outputTemplate.value = "";
    reportFile.value = "";
    userPickedMode.value = false;
    verifyAfter.value = false;
  }

  return {
    recursive,
    maxDepth,
    output,
    keys,
    level,
    mode,
    blockSizeExp,
    onConflict,
    skipSpaceCheck,
    outputTemplate,
    reportFile,
    userPickedMode,
    verifyAfter,
    setMode,
    $reset,
  };
});
