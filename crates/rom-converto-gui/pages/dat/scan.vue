<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useDatScanStore } from "~/stores/datScan";
import type { DatResultRow } from "~/components/DatResultList.vue";

const store = useDatScanStore();
const { input, maxDepth, result, error, loading } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("dat-scan");
const commandLine = ref("");

const { canRun, runBlockReason } = usePageGating({
  input,
  emptyInputReason: "Select a folder to scan against the Playmatch database.",
});

interface DatScanRow {
  path: string;
  status: "matched" | "misnamed" | "hint" | "unknown" | "unsupported" | "failed";
  gameName: string | null;
  canonicalStem: string | null;
  error: string | null;
}

interface DatScanResult {
  kind: "scan";
  matched: number;
  misnamed: number;
  hint: number;
  unknown: number;
  unsupported: number;
  failed: number;
  rows: DatScanRow[];
}

const scanResult = ref<DatScanResult | null>(null);

const STATUS_COLOR: Record<DatScanRow["status"], "emerald" | "amber" | "zinc" | "red"> = {
  matched: "emerald",
  misnamed: "amber",
  hint: "amber",
  unknown: "zinc",
  unsupported: "zinc",
  failed: "red",
};

const STATUS_LABEL: Record<DatScanRow["status"], string> = {
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

function onDepthInput(event: Event) {
  const raw = (event.target as HTMLInputElement).value;
  maxDepth.value = raw === "" ? null : Number(raw);
}

function scanArgs() {
  return {
    input: input.value,
    maxDepth: maxDepth.value,
  };
}

async function execute() {
  progress.reset();
  scanResult.value = null;
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

const rows = computed<DatResultRow[]>(() =>
  (scanResult.value?.rows ?? []).map((r) => ({
    key: r.path,
    icon: r.status === "matched" ? "ok" : r.status === "failed" ? "error" : r.status === "misnamed" || r.status === "hint" ? "warn" : "info",
    primary: basename(r.path),
    secondary: r.gameName ?? r.error ?? undefined,
    badge: STATUS_LABEL[r.status],
    badgeColor: STATUS_COLOR[r.status],
  })),
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

    <template v-if="scanResult">
      <div class="mb-4 flex flex-wrap gap-2">
        <span
          v-for="chip in CHIPS"
          :key="chip.key"
          class="inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium"
          :class="{
            'bg-emerald-500/10 text-emerald-400': chip.color === 'emerald',
            'bg-amber-500/10 text-amber-400': chip.color === 'amber',
            'bg-zinc-700/30 text-zinc-400': chip.color === 'zinc',
            'bg-red-500/10 text-red-400': chip.color === 'red',
          }"
        >
          {{ chip.label }}: {{ scanResult[chip.key] }}
        </span>
      </div>

      <div class="mb-4">
        <DatResultList :rows="rows" />
      </div>
    </template>

    <div v-else class="mb-4">
      <OutputLog :command="commandLine" :result="result" :error="error" />
    </div>

    <OperationCard>
      <div class="space-y-5">
        <FileDropZone
          v-model="input"
          label="Folder to scan"
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

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton :loading="loading" :disabled="!canRun" :disabled-reason="runBlockReason" @click="execute">
          Scan
        </RunButton>
      </div>
    </OperationCard>
  </div>
</template>
