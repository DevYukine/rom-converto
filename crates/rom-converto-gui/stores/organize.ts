import { defineStore } from "pinia";
import { shallowRef } from "vue";
import { listen } from "~/lib/ipc";
import type { OrganizeRow, RunRow } from "~/types";
import { MAX_LIVE_ROWS } from "./datScan";

const FLUSH_MS = 100;

export const useOrganizeStore = defineStore("organize", () => {
  const outputDir = ref("");
  const outputTemplate = ref("{console}/{basename}.{ext}");
  const dat = ref(false);
  const moveSource = ref(false);
  const playlists = ref(false);
  const multiDiscDirs = ref(false);
  const allowEncrypted = ref(false);
  const maxDepth = ref<number | null>(null);
  const keys = ref("");
  // Library-management options. Code lists (languages, regions, types) are
  // stored as one comma-separated string; glob, regex, and path lists are
  // one entry per line. Both are split in the opdef's buildArgs.
  const inputExclude = ref("");
  const filterRegex = ref("");
  const filterRegexExclude = ref("");
  const filterLanguage = ref("");
  const filterRegion = ref("");
  const noType = ref("");
  const onlyType = ref("");
  const onlyRetail = ref(false);
  const single = ref(false);
  const preferRegion = ref("");
  const preferLanguage = ref("");
  const preferRevision = ref("");
  const preferRetail = ref(false);
  const preferParent = ref(false);
  const preferVerified = ref(false);
  const preferGood = ref(false);
  const preferGameRegex = ref("");
  const preferFilenameRegex = ref("");
  const dirLetter = ref(false);
  const dirLetterCount = ref<number | null>(null);
  const dirLetterLimit = ref<number | null>(null);
  const dirLetterGroup = ref(false);
  // Empty means "follow the config file"; the segmented options carry the
  // explicit values.
  const zipFormat = ref("");
  const zipExclude = ref("");
  const linkMode = ref("");
  const symlinkRelative = ref(false);
  const removeHeaders = ref("");
  const trimAddPadding = ref(false);
  const patch = ref("");
  const patchOnly = ref(false);
  const clean = ref(false);
  const cleanExclude = ref("");
  const cleanBackup = ref("");
  const moveDeleteDirs = ref("");
  const verifyAfter = ref(false);
  // Organize resolves an unset policy itself, so the page always sends one
  // and defaults to the runner's own default, not the global one.
  const onConflict = ref("error");
  const skipSpaceCheck = ref(false);
  const statusFilter = ref<"all" | OrganizeRow["status"]>("all");
  // Rows the running organize streams on the op's fixed progress key. Events
  // arrive per library item, so they are buffered and folded into one array
  // on a timer: the result list re-renders a few times a second instead of
  // once per event. The index keyed by input is what replaces a unit's
  // pending row with its settled one.
  const liveRows = shallowRef<OrganizeRow[]>([]);
  const liveIndex = new Map<string, number>();
  let buffered: OrganizeRow[] = [];
  let flushTimer: ReturnType<typeof setTimeout> | null = null;
  let rowListener: Promise<void> | null = null;

  // Replaces the array rather than mutating it: a computed that returns the
  // same array identity does not notify its own dependents.
  function flushLiveRows() {
    if (flushTimer) clearTimeout(flushTimer);
    flushTimer = null;
    if (!buffered.length) return;
    const rows = liveRows.value.slice();
    for (const row of buffered) {
      const i = liveIndex.get(row.input);
      if (i === undefined) {
        if (rows.length >= MAX_LIVE_ROWS) continue;
        liveIndex.set(row.input, rows.length);
        rows.push(row);
      } else {
        rows[i] = row;
      }
    }
    buffered = [];
    liveRows.value = rows;
  }

  function clearRunState() {
    if (flushTimer) clearTimeout(flushTimer);
    flushTimer = null;
    buffered = [];
    liveIndex.clear();
    liveRows.value = [];
    statusFilter.value = "all";
  }

  function ensureRowListener() {
    rowListener ??= listen<RunRow>("organize-row", (event) => {
      if (event.payload.kind !== "organize") return;
      buffered.push(event.payload);
      flushTimer ??= setTimeout(flushLiveRows, FLUSH_MS);
    }).then(() => undefined);
    return rowListener;
  }

  function $reset() {
    outputDir.value = "";
    outputTemplate.value = "{console}/{basename}.{ext}";
    dat.value = false;
    moveSource.value = false;
    playlists.value = false;
    multiDiscDirs.value = false;
    allowEncrypted.value = false;
    maxDepth.value = null;
    keys.value = "";
    inputExclude.value = "";
    filterRegex.value = "";
    filterRegexExclude.value = "";
    filterLanguage.value = "";
    filterRegion.value = "";
    noType.value = "";
    onlyType.value = "";
    onlyRetail.value = false;
    single.value = false;
    preferRegion.value = "";
    preferLanguage.value = "";
    preferRevision.value = "";
    preferRetail.value = false;
    preferParent.value = false;
    preferVerified.value = false;
    preferGood.value = false;
    preferGameRegex.value = "";
    preferFilenameRegex.value = "";
    dirLetter.value = false;
    dirLetterCount.value = null;
    dirLetterLimit.value = null;
    dirLetterGroup.value = false;
    zipFormat.value = "";
    zipExclude.value = "";
    linkMode.value = "";
    symlinkRelative.value = false;
    removeHeaders.value = "";
    trimAddPadding.value = false;
    patch.value = "";
    patchOnly.value = false;
    clean.value = false;
    cleanExclude.value = "";
    cleanBackup.value = "";
    moveDeleteDirs.value = "";
    verifyAfter.value = false;
    onConflict.value = "error";
    skipSpaceCheck.value = false;
    clearRunState();
  }

  return {
    outputDir,
    outputTemplate,
    dat,
    moveSource,
    playlists,
    multiDiscDirs,
    allowEncrypted,
    maxDepth,
    keys,
    inputExclude,
    filterRegex,
    filterRegexExclude,
    filterLanguage,
    filterRegion,
    noType,
    onlyType,
    onlyRetail,
    single,
    preferRegion,
    preferLanguage,
    preferRevision,
    preferRetail,
    preferParent,
    preferVerified,
    preferGood,
    preferGameRegex,
    preferFilenameRegex,
    dirLetter,
    dirLetterCount,
    dirLetterLimit,
    dirLetterGroup,
    zipFormat,
    zipExclude,
    linkMode,
    symlinkRelative,
    removeHeaders,
    trimAddPadding,
    patch,
    patchOnly,
    clean,
    cleanExclude,
    cleanBackup,
    moveDeleteDirs,
    verifyAfter,
    onConflict,
    skipSpaceCheck,
    statusFilter,
    liveRows,
    clearRunState,
    flushLiveRows,
    ensureRowListener,
    $reset,
  };
});
