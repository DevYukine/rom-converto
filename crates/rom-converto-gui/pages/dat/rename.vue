<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useDatRenameStore } from "~/stores/datRename";
import type { DatResultRow } from "~/components/DatResultList.vue";

const store = useDatRenameStore();
const { input, maxDepth, dryRun, onConflict, result, error, loading } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("dat-rename");
const commandLine = ref("");

const { canRun, runBlockReason } = usePageGating({
  input,
  emptyInputReason: "Select a file or directory to rename against the Playmatch database.",
});

type DatRenameAction =
  | "renamed"
  | "would-rename"
  | "already-canonical"
  | "skip-unmatched"
  | "skip-weak"
  | "skip-collision"
  | "skip-disc-set"
  | "failed";

interface DatRenameRow {
  from: string;
  to: string | null;
  action: DatRenameAction;
  detail: string | null;
}

interface DatRenameResult {
  kind: "rename";
  dryRun: boolean;
  renamed: number;
  skipped: number;
  failed: number;
  rows: DatRenameRow[];
}

const renameResult = ref<DatRenameResult | null>(null);

const ACTION_COLOR: Record<DatRenameAction, "emerald" | "sky" | "amber" | "red"> = {
  renamed: "emerald",
  "would-rename": "sky",
  "already-canonical": "sky",
  "skip-unmatched": "amber",
  "skip-weak": "amber",
  "skip-collision": "amber",
  "skip-disc-set": "amber",
  failed: "red",
};

const ACTION_LABEL: Record<DatRenameAction, string> = {
  renamed: "Renamed",
  "would-rename": "Would rename",
  "already-canonical": "Already canonical",
  "skip-unmatched": "Skip: unmatched",
  "skip-weak": "Skip: weak match",
  "skip-collision": "Skip: collision",
  "skip-disc-set": "Skip: disc set",
  failed: "Failed",
};

function onDepthInput(event: Event) {
  const raw = (event.target as HTMLInputElement).value;
  maxDepth.value = raw === "" ? null : Number(raw);
}

function renameArgs(dry: boolean) {
  return {
    input: input.value,
    maxDepth: maxDepth.value,
    dryRun: dry,
    onConflict: onConflict.value,
  };
}

async function runRename(dry: boolean) {
  progress.reset();
  renameResult.value = null;
  const args = renameArgs(dry);
  commandLine.value = buildCliCommand("cmd_dat_rename", args);
  await run("cmd_dat_rename", args);
  if (result.value) {
    try {
      const parsed = JSON.parse(result.value) as DatRenameResult;
      renameResult.value = parsed;
      result.value = `${parsed.renamed} renamed, ${parsed.skipped} skipped, ${parsed.failed} failed`;
    } catch {
      // leave result.value as the raw command output
    }
  }
}

async function execute() {
  await runRename(dryRun.value);
}

// Apply always re-runs the full pipeline (digest, query, plan, execute) with
// dryRun false rather than replaying the preview's plan, so filesystem state
// changed between preview and apply is re-planned, never acted on stale.
async function apply() {
  await runRename(false);
}

const rows = computed<DatResultRow[]>(() =>
  (renameResult.value?.rows ?? []).map((r) => ({
    key: r.from,
    icon: r.action === "renamed" || r.action === "would-rename" ? "ok" : r.action === "failed" ? "error" : r.action === "already-canonical" ? "info" : "warn",
    primary: r.to ? `${basename(r.from)} -> ${basename(r.to)}` : basename(r.from),
    secondary: r.detail ?? undefined,
    badge: ACTION_LABEL[r.action],
    badgeColor: ACTION_COLOR[r.action],
  })),
);
</script>

<template>
  <div>
    <PageHeader
      title="Rename"
      description="Rename ROMs to their canonical Playmatch database names. Only hash-verified matches are renamed."
      :loading="loading"
      :has-result="!!result"
      :has-error="!!error"
    />

    <template v-if="renameResult">
      <div class="mb-4 flex flex-wrap items-center gap-2">
        <span class="inline-flex items-center gap-1.5 rounded-full bg-emerald-500/10 px-2.5 py-1 text-xs font-medium text-emerald-400">
          Renamed: {{ renameResult.renamed }}
        </span>
        <span class="inline-flex items-center gap-1.5 rounded-full bg-amber-500/10 px-2.5 py-1 text-xs font-medium text-amber-400">
          Skipped: {{ renameResult.skipped }}
        </span>
        <span class="inline-flex items-center gap-1.5 rounded-full bg-red-500/10 px-2.5 py-1 text-xs font-medium text-red-400">
          Failed: {{ renameResult.failed }}
        </span>
        <span
          v-if="renameResult.dryRun"
          class="inline-flex items-center gap-1.5 rounded-full bg-sky-500/10 px-2.5 py-1 text-xs font-medium text-sky-400"
        >
          Preview only
        </span>
      </div>

      <div class="mb-4">
        <DatResultList :rows="rows" />
      </div>

      <div v-if="renameResult.dryRun" class="mb-4">
        <RunButton :loading="loading" :disabled="!canRun" @click="apply">
          Apply
        </RunButton>
      </div>
    </template>

    <div v-else class="mb-4">
      <OutputLog :command="commandLine" :result="result" :error="error" />
    </div>

    <OperationCard>
      <div class="space-y-5">
        <FileDropZone
          v-model="input"
          label="Input file or directory"
          :directory="false"
          :primary="true"
        />

        <div class="space-y-4 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <FlagToggle
            v-model="dryRun"
            label="Preview (dry run)"
            description="Show what each file would do without writing anything."
          />

          <div class="space-y-1.5">
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

          <ConflictPolicyControl v-model="onConflict" />
        </div>

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton :loading="loading" :disabled="!canRun" :disabled-reason="runBlockReason" @click="execute">
          {{ dryRun ? 'Preview' : 'Rename' }}
        </RunButton>
      </div>
    </OperationCard>
  </div>
</template>
