<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useDatScanStore } from "~/stores/datScan";
import type { DatScanRowEvent, DatScanResult, DatScanStatus, ScanLevel } from "~/stores/datScan";
import type { DatResultRow } from "~/components/DatResultList.vue";

const store = useDatScanStore();
const { input, maxDepth, scanLevel, quick, result, error, loading, commandLine, statusFilter, scanResult, liveRows } = storeToRefs(store);
const { run, cancelled, abort } = useOperation({ result, error, loading });
const progress = useProgress("dat-scan");

void store.ensureRowListener();

const { canRun, runBlockReason } = usePageGating({
  input,
  emptyInputReason: "Select a directory to scan against the Playmatch database.",
});

const STATUS_COLOR: Record<DatScanStatus, "emerald" | "amber" | "zinc" | "red"> = {
  matched: "emerald",
  misnamed: "amber",
  hint: "amber",
  unknown: "zinc",
  unsupported: "zinc",
  failed: "red",
};

const STATUS_LABEL: Record<DatScanStatus, string> = {
  matched: "Matched",
  misnamed: "Misnamed",
  hint: "Hint",
  unknown: "Unknown",
  unsupported: "Unsupported",
  failed: "Failed",
};

const CHIPS: { key: keyof Omit<DatScanResult, "kind" | "rows">; label: string; color: "emerald" | "amber" | "zinc" | "red" }[] = [
  { key: "matched", label: "Matched", color: "emerald" },
  { key: "misnamed", label: "Misnamed", color: "amber" },
  { key: "hint", label: "Hint", color: "amber" },
  { key: "unknown", label: "Unknown", color: "zinc" },
  { key: "unsupported", label: "Unsupported", color: "zinc" },
  { key: "failed", label: "Failed", color: "red" },
];

const SCAN_LEVELS: { label: string; value: ScanLevel }[] = [
  { label: "CRC + Size", value: "crc" },
  { label: "MD5", value: "md5" },
  { label: "SHA-1", value: "sha1" },
  { label: "SHA-256", value: "sha256" },
];

// Every level keeps crc32: it is nearly free to compute alongside the
// stronger digest and remains the fallback match rung.
const SCAN_LEVEL_ALGOS: Record<ScanLevel, string[]> = {
  crc: ["crc32"],
  md5: ["crc32", "md5"],
  sha1: ["crc32", "sha1"],
  sha256: ["crc32", "sha256"],
};

const SCAN_LEVEL_HINT: Record<ScanLevel, string> = {
  crc: "Size plus CRC32 identifies almost everything and is the fastest level.",
  md5: "Adds an MD5 digest per file for DATs that match on MD5.",
  sha1: "Adds a SHA-1 digest per file for stronger match confidence.",
  sha256: "Adds a SHA-256 digest per file; slowest, highest confidence.",
};

const sourceRows = computed<DatScanRowEvent[]>(() =>
  scanResult.value ? scanResult.value.rows : Array.from(liveRows.value.values()),
);

const statusCounts = computed<Record<string, number>>(() => {
  const counts: Record<string, number> = {};
  for (const r of sourceRows.value) counts[r.status] = (counts[r.status] ?? 0) + 1;
  return counts;
});

function toggleFilter(status: DatScanStatus) {
  statusFilter.value = statusFilter.value === status ? "all" : status;
}

function onDepthInput(event: Event) {
  const raw = (event.target as HTMLInputElement).value;
  maxDepth.value = raw === "" ? null : Number(raw);
}

function scanArgs() {
  return {
    input: input.value,
    maxDepth: maxDepth.value,
    algos: SCAN_LEVEL_ALGOS[scanLevel.value],
    quick: quick.value,
  };
}

async function execute() {
  progress.reset();
  store.clearScanState();
  const args = scanArgs();
  commandLine.value = buildCliCommand("cmd_dat_scan", args);
  await run("cmd_dat_scan", args);
  if (result.value) {
    try {
      const parsed = JSON.parse(result.value) as DatScanResult;
      scanResult.value = parsed;
      result.value = `${parsed.matched} matched, ${parsed.misnamed} misnamed, ${parsed.hint} hint, ${parsed.unknown} unknown, ${parsed.unsupported} unsupported, ${parsed.failed} failed`;
    } catch {
      // leave result.value as the raw command output
    }
  }
}

function toResultRow(r: DatScanRowEvent): DatResultRow {
  return {
    key: r.path,
    icon:
      r.status === "pending"
        ? "info"
        : r.status === "matched"
          ? "ok"
          : r.status === "failed"
            ? "error"
            : r.status === "misnamed" || r.status === "hint"
              ? "warn"
              : "info",
    primary: basename(r.path),
    secondary: r.gameName ?? r.error ?? undefined,
    badge: r.status === "pending" ? "Pending" : STATUS_LABEL[r.status],
    badgeColor: r.status === "pending" ? "zinc" : STATUS_COLOR[r.status],
  };
}

const rows = computed<DatResultRow[]>(() =>
  sourceRows.value
    .filter((r) => statusFilter.value === "all" || r.status === statusFilter.value)
    .map(toResultRow),
);
</script>

<template>
  <div>
    <PageHeader
      title="Scan"
      description="Batch-identify a library against the Playmatch database and summarize matched, misnamed, hint, unknown, unsupported and failed files."
      :loading="loading"
      :has-result="!!result"
      :has-error="!!error"
    />

    <div v-if="sourceRows.length > 0" class="mb-4 flex flex-wrap gap-2">
      <button
        type="button"
        class="inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium transition"
        :class="statusFilter === 'all' ? 'bg-sky-500/15 text-sky-300 ring-1 ring-sky-500/40' : 'bg-zinc-700/30 text-zinc-400 hover:text-zinc-300'"
        :aria-pressed="statusFilter === 'all'"
        @click="statusFilter = 'all'"
      >
        All: {{ sourceRows.length }}
      </button>
      <button
        v-for="chip in CHIPS"
        :key="chip.key"
        type="button"
        class="inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium transition"
        :class="[
          {
            'bg-emerald-500/10 text-emerald-400': chip.color === 'emerald',
            'bg-amber-500/10 text-amber-400': chip.color === 'amber',
            'bg-zinc-700/30 text-zinc-400': chip.color === 'zinc',
            'bg-red-500/10 text-red-400': chip.color === 'red',
          },
          statusFilter === chip.key ? 'ring-1 ring-current' : 'opacity-80 hover:opacity-100',
        ]"
        :aria-pressed="statusFilter === chip.key"
        @click="toggleFilter(chip.key)"
      >
        {{ chip.label }}: {{ statusCounts[chip.key] ?? 0 }}
      </button>
    </div>

    <div v-if="rows.length > 0" class="mb-4">
      <DatResultList :rows="rows" />
    </div>
    <div
      v-else-if="sourceRows.length > 0"
      class="mb-4 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3 text-sm text-zinc-400"
    >
      No {{ statusFilter }} files in this scan.
    </div>
    <div
      v-else-if="scanResult"
      class="mb-4 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3 text-sm text-zinc-400"
    >
      No files found under the selected directory.
    </div>

    <div v-if="!scanResult || progress.warnings.value.length > 0" class="mb-4">
      <OutputLog :command="commandLine" :result="result" :cancelled="cancelled ? 'Operation cancelled.' : undefined" :error="error" :warnings="progress.warnings.value" />
    </div>

    <OperationCard>
      <div class="space-y-5">
        <FileDropZone
          v-model="input"
          label="Directory to scan"
          :directory="true"
          :primary="true"
        />

        <div class="space-y-1.5 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <label class="block text-sm font-medium text-zinc-300">Max depth (optional)</label>
          <input
            type="number"
            min="1"
            :value="maxDepth ?? ''"
            placeholder="Unlimited"
            class="w-32 rounded-lg border border-zinc-700 bg-zinc-800/30 px-3 py-1.5 text-sm text-zinc-200 focus:border-sky-500 focus:outline-none"
            @input="onDepthInput"
          >
        </div>

        <div>
          <SegmentedControl v-model="scanLevel" label="Scan level" :options="SCAN_LEVELS" :disabled="loading" />
          <p class="mt-1.5 text-xs text-zinc-500">{{ SCAN_LEVEL_HINT[scanLevel] }}</p>
        </div>

        <FlagToggle
          v-model="quick"
          label="Quick scan"
          description="Trust a zip's own CRC32 for eligible cartridge images instead of extracting and hashing. Falls back automatically when that alone cannot identify the file."
          :disabled="loading"
        />

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton :loading="loading" :disabled="!canRun" :disabled-reason="runBlockReason" @click="execute" @cancel="abort()">
          Scan
        </RunButton>
      </div>
    </OperationCard>
  </div>
</template>
