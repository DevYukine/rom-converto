<script setup lang="ts">
import { open } from "@tauri-apps/plugin-dialog";
import { storeToRefs } from "pinia";
import { useNxDecompressStore } from "~/stores/nx-decompress";
import type { ComparisonSummary, ReportRecord, RunOutcome } from "~/types/report";

const store = useNxDecompressStore();
const { queue, output, keys, onConflict, skipSpaceCheck, outputTemplate, reportFile, result, error, loading, recursive, maxDepth } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { expand } = useFolderScan(["nsz", "xcz"]);
const scanDepth = () => (recursive.value ? maxDepth.value : 1);
const progress = useProgress("nx-decompress");

const previewMode = ref(false);
const { preview, batch: previewBatch, error: previewError } = usePreview("cmd_nx_decompress");

const commandLine = ref("");

function decompressArgs(item: { input: string; output: string }) {
  const tmpl = outputTemplate.value || null;
  return {
    input: item.input,
    output: tmpl ? null : item.output || null,
    keys: keys.value || null,
    onConflict: onConflict.value,
    skipSpaceCheck: skipSpaceCheck.value,
    outputTemplate: tmpl,
    report: !!reportFile.value,
    reportFile: reportFile.value || null,
    dryRun: previewMode.value,
  };
}

const batch = useBatchOperation("nx-decompress", "cmd_nx_decompress", decompressArgs);

function handleReorder(ids: string[]) {
  batch.reorder(queue, ids);
}

function handleRemoveSelected(ids: string[]) {
  batch.removeSelected(queue, ids);
}
const comparisons = ref<ComparisonSummary[]>([]);

const dropZoneRef = ref<HTMLElement | null>(null);
let zoneId: string | null = null;

async function addPaths(paths: string[]) {
  for (const p of paths) {
    if (!p) continue;
    for (const f of await expand(p, scanDepth())) {
      store.addToQueue(f);
    }
  }
  if (!output.value && queue.value.length > 0) {
    const first = queue.value[0];
    if (first) output.value = resolve(deriveNspPath(first.input));
  }
}

onMounted(() => {
  if (dropZoneRef.value) {
    zoneId = registerDropZone(dropZoneRef.value, addPaths, 0);
  }
});

onUnmounted(() => {
  if (zoneId) unregisterDropZone(zoneId);
});

watch(outputDir, () => {
  if (output.value && queue.value.length === 1) {
    const first = queue.value[0];
    if (first) output.value = resolve(deriveNspPath(first.input));
  }
  for (const item of queue.value) {
    if (item.status === "pending") {
      item.output = resolve(deriveNspPath(item.input));
    }
  }
});

async function browseInputs() {
  const result = await open({
    directory: false,
    multiple: true,
    filters: [{ name: "Switch compressed", extensions: ["nsz", "xcz", "zip", "7z", "rar", "tar", "tgz", "gz"] }],
  });
  if (!result) return;
  addPaths(Array.isArray(result) ? result : [result]);
}

const { canRun, runBlockReason, templateActive } = usePageGating({
  queue,
  outputTemplate,
  emptyInputReason: "Add at least one file to the queue to continue.",
});

async function execute() {
  progress.reset();
  if (!outputTemplate.value) {
    for (const item of queue.value) {
      item.output =
        queue.value.length === 1
          ? output.value
          : resolve(deriveNspPath(item.input));
    }
  }
  const records: ReportRecord[] = [];
  comparisons.value = [];
  const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
  commandLine.value = rep ? buildCliCommand("cmd_nx_decompress", decompressArgs(rep)) : "";
  await batch.start(
    queue,
    result,
    { errorRef: error },
    (res) => {
      const record = (res as RunOutcome)?.record;
      if (record) records.push(record);
      const comparison = (res as RunOutcome)?.comparison;
      if (comparison) comparisons.value.push(comparison);
    },
    async (item, err) => {
      if (reportFile.value) await pushFailedRecord(records, item.input, "decompress", err);
    },
  );
  if (reportFile.value && records.length) {
    await writeRunReport(reportFile.value, records);
  }
}

async function runPreview() {
  if (!outputTemplate.value) {
    for (const item of queue.value) {
      item.output = queue.value.length === 1 ? output.value : resolve(deriveNspPath(item.input));
    }
  }
  const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
  commandLine.value = rep ? buildCliCommand("cmd_nx_decompress", decompressArgs(rep)) : "";
  await previewBatch(queue, decompressArgs);
  if (previewError.value) error.value = previewError.value;
}

function onRun() {
  if (previewMode.value) runPreview();
  else execute();
}
</script>

<template>
  <div>
    <PageHeader
      title="Decompress NSZ/XCZ"
      description="Decompress NSZ to NSP or XCZ to XCI. Output is byte-identical to the original installable container. Drop multiple files for batch processing."
      :loading="loading || batch.running.value"
      :has-result="!!result"
      :has-error="!!error"
    />

    <div class="mb-4">
      <OutputLog :command="commandLine" :result="result" :preview="preview" :error="error" />
    </div>

    <div class="mb-4">
      <ComparisonList :comparisons="comparisons" />
    </div>

    <OperationCard>
      <div class="space-y-5">
        <BatchFileList
          v-if="queue.length > 0"
          :items="queue"
          :running="batch.running.value"
          :progress-slots="batch.progressSlots"
          @remove="store.removeFromQueue"
          @clear="store.clearQueue"
          @reorder="handleReorder"
          @remove-selected="handleRemoveSelected"
          @retry-failed="execute"
        />

        <div class="space-y-1.5">
          <label class="block text-sm font-medium text-zinc-300">
            {{ queue.length > 0 ? "Add more inputs" : "Inputs" }}
          </label>
          <div
            ref="dropZoneRef"
            class="drop-zone flex cursor-default flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed border-zinc-700 bg-zinc-800/30 px-4 py-6 transition xl:py-8"
          >
            <svg class="h-8 w-8 text-zinc-500" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
              <path stroke-linecap="round" stroke-linejoin="round" d="M3 16.5v2.25A2.25 2.25 0 005.25 21h13.5A2.25 2.25 0 0021 18.75V16.5m-13.5-9L12 3m0 0l4.5 4.5M12 3v13.5" />
            </svg>
            <span class="text-center text-sm text-zinc-400">
              Drop NSZ or XCZ files. Multiple inputs queue sequentially.
            </span>
            <button
              type="button"
              class="rounded-md bg-zinc-700/60 px-3 py-1.5 text-xs font-medium text-zinc-200 transition hover:bg-zinc-700"
              @click="browseInputs"
            >
              Browse files
            </button>
          </div>
        </div>

        <InfoTooltip v-if="queue.length <= 1 && templateActive" :message="OUTPUT_TEMPLATE_TOOLTIP" block>
          <FileDropZone
            v-model="output"
            class="w-full"
            label="Output file (auto-filled)"
            :save-dialog="true"
            :disabled="true"
            :filters="[{ name: 'Switch container', extensions: ['nsp', 'xci'] }]"
          />
        </InfoTooltip>
        <FileDropZone
          v-else-if="queue.length <= 1"
          v-model="output"
          label="Output file (auto-filled)"
          :save-dialog="true"
          :filters="[{ name: 'Switch container', extensions: ['nsp', 'xci'] }]"
        />
        <div
          v-else
          class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3 text-xs text-zinc-400"
        >
          Multi-file batch: each output is derived from its input (.nsz -> .nsp,
          .xcz -> .xci) next to the source file.
        </div>

        <FileDropZone
          v-model="keys"
          label="prod.keys (optional, falls back to ~/.switch/prod.keys)"
          :filters="[{ name: 'prod.keys', extensions: ['keys', 'txt'] }]"
        />

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3 space-y-3">
          <ConflictPolicyControl v-model="onConflict" />
          <RecursiveOptions
            :recursive="recursive"
            :max-depth="maxDepth"
            @update:recursive="recursive = $event"
            @update:max-depth="maxDepth = $event"
          />
          <FlagToggle
            v-model="skipSpaceCheck"
            label="Skip free space check"
            description="Proceed even if the output filesystem looks too full to hold the result."
          />
        </div>

        <label class="flex flex-col gap-1.5">
          <span class="text-sm font-medium text-zinc-200">Output template (optional)</span>
          <span class="text-xs text-zinc-400">
            Build the output path from metadata tokens, for example {console}/{title}.{ext}. Replaces the explicit output path.
          </span>
          <input
            v-model="outputTemplate"
            type="text"
            placeholder="for example, {console}/{title}.{ext}"
            class="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-800/50 px-3 py-1.5 text-sm text-zinc-200"
          />
        </label>

        <OutputDirField v-model="outputDir" />

        <FileDropZone
          v-model="reportFile"
          label="Run report file (optional)"
          placeholder="No report"
          :save-dialog="true"
          :filters="[{ name: 'Report', extensions: ['csv', 'json', 'html'] }]"
        />

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <FlagToggle
          v-model="previewMode"
          label="Preview (dry run)"
          description="Show what each file would do without writing anything."
        />

        <RunButton
          :loading="loading || batch.running.value"
          :batch-current="batch.currentIndex.value"
          :batch-total="queue.length"
          :disabled="!canRun"
          :disabled-reason="runBlockReason"
          @click="onRun"
          @cancel="batch.abort"
        >
          {{ previewMode ? 'Preview' : (queue.filter(i => i.status === 'pending').length > 1 ? `Decompress all (${queue.filter(i => i.status === 'pending').length})` : 'Decompress') }}
        </RunButton>
      </div>
    </OperationCard>
  </div>
</template>
