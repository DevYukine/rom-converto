<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useDolCompressStore } from "~/stores/dol-compress";
import type { ComparisonSummary, ReportRecord, RunOutcome } from "~/types/report";

const store = useDolCompressStore();
const { input, output, level, chunkSize, onConflict, skipSpaceCheck, outputTemplate, reportFile, verifyAfter, result, error, loading, queue, recursive, maxDepth } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { expand } = useFolderScan(["iso", "gcm"]);
const scanDepth = () => (recursive.value ? maxDepth.value : 1);
const { run, cancelled, abort } = useOperation({ result, error, loading });
const progress = useProgress("dol-compress");

const previewMode = ref(false);
const { preview, single: previewSingle, batch: previewBatch, error: previewError } = usePreview("cmd_compress_disc");

const CHUNK_SIZES = [32768, 65536, 131072, 262144, 524288, 1048576, 2097152];

const isBatch = computed(() => queue.value.length > 0);
const { canRun, runBlockReason, templateActive } = usePageGating({ input, queue, outputTemplate });

const commandLine = ref("");

function compressArgs(inputPath: string, outputPath: string) {
  const tmpl = outputTemplate.value || null;
  return {
    input: inputPath,
    output: tmpl ? null : outputPath || null,
    level: level.value,
    chunkSize: chunkSize.value,
    taskId: "dol-compress",
    onConflict: onConflict.value,
    skipSpaceCheck: skipSpaceCheck.value,
    outputTemplate: tmpl,
    report: !!reportFile.value,
    reportFile: reportFile.value || null,
    verifyAfter: verifyAfter.value,
    dryRun: previewMode.value,
  };
}

const batch = useBatchOperation("dol-compress", "cmd_compress_disc", (item) =>
  compressArgs(item.input, item.output),
);
const comparisons = ref<ComparisonSummary[]>([]);

watch([input, outputDir], () => {
  if (input.value) output.value = resolve(deriveRvzPath(input.value));
});

watch(outputDir, () => {
  for (const it of queue.value) {
    if (it.status === "pending") it.output = resolve(deriveRvzPath(it.input));
  }
});

async function handleFiles(paths: string[]) {
  for (const p of paths) {
    for (const f of await expand(p, scanDepth())) {
      store.addToQueue(f, resolve(deriveRvzPath(f)));
    }
  }
}

async function handleSingleFile(path: string) {
  const found = await expand(path, scanDepth());
  if (found.length === 1 && found[0] === path && queue.value.length === 0) {
    input.value = path;
  } else {
    for (const f of found) {
      store.addToQueue(f, resolve(deriveRvzPath(f)));
    }
  }
}

async function execute() {
  progress.reset();
  const records: ReportRecord[] = [];
  comparisons.value = [];
  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_compress_disc", compressArgs(rep.input, rep.output)) : "";
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
        if (reportFile.value) await pushFailedRecord(records, item.input, "compress", err);
      },
    );
  } else {
    const args = compressArgs(input.value, output.value);
    commandLine.value = buildCliCommand("cmd_compress_disc", args);
    await runReportable("cmd_compress_disc", args, { result, error, loading, cancelled }, records, "compress", comparisons.value);
  }
  if (reportFile.value && records.length) {
    await writeRunReport(reportFile.value, records);
  }
}

async function runPreview() {
  const rep = isBatch.value ? (queue.value.find((i) => i.status === "pending") ?? queue.value[0]) : null;
  commandLine.value = isBatch.value
    ? rep ? buildCliCommand("cmd_compress_disc", compressArgs(rep.input, rep.output)) : ""
    : buildCliCommand("cmd_compress_disc", compressArgs(input.value, output.value));
  if (isBatch.value) {
    await previewBatch(queue, (item) => compressArgs(item.input, item.output));
  } else {
    await previewSingle(compressArgs(input.value, output.value));
  }
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
      title="Compress to RVZ"
      description="Compress a GameCube disc image (.iso / .gcm) to Dolphin's RVZ format. Legacy GCZ and NKit images are detected automatically, verified, and migrated. Drop multiple files for batch processing."
      :loading="loading || batch.running.value"
      :has-result="!!result"
      :has-error="!!error"
    />

    <div class="mb-4">
      <OutputLog :command="commandLine" :result="result" :preview="preview" :cancelled="cancelled ? 'Operation cancelled.' : undefined" :error="error" />
    </div>

    <div class="mb-4">
      <ComparisonList :comparisons="comparisons" />
    </div>

    <OperationCard>
      <div class="space-y-5">
        <template v-if="isBatch">
          <BatchFileList
            :items="queue"
            :current-index="batch.currentIndex.value"
            :running="batch.running.value"
            :progress="batch.progress"
            @remove="store.removeFromQueue"
            @clear="store.clearQueue"
          />

          <FileDropZone
            label="Add more files"
            model-value=""
            :multiple="true"
            :filters="[{ name: 'GameCube disc', extensions: ['iso', 'gcm', 'gcz'] }]"
            @update:model-value="(p: string) => { if (p) handleSingleFile(p) }"
            @update:files="handleFiles"
          />
        </template>

        <div v-else class="grid gap-5 lg:grid-cols-2">
          <FileDropZone
            :model-value="input"
            label="Input disc"
            :multiple="true"
            :filters="[{ name: 'GameCube disc', extensions: ['iso', 'gcm', 'gcz'] }]"
            :primary="true"
            @update:model-value="handleSingleFile"
            @update:files="handleFiles"
          />

          <InfoTooltip v-if="templateActive" :message="OUTPUT_TEMPLATE_TOOLTIP" block>
            <FileDropZone
              v-model="output"
              class="w-full"
              label="Output file (auto-filled)"
              :save-dialog="true"
              :disabled="true"
              :filters="[{ name: 'RVZ', extensions: ['rvz'] }]"
            />
          </InfoTooltip>
          <FileDropZone
            v-else
            v-model="output"
            label="Output file (auto-filled)"
            :save-dialog="true"
            :filters="[{ name: 'RVZ', extensions: ['rvz'] }]"
          />
        </div>

        <div class="space-y-3 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <label class="flex flex-col gap-1.5">
            <span class="text-sm font-medium text-zinc-200">Zstd level</span>
            <span class="text-xs text-zinc-400">
              1 is fastest, 22 is max ratio. Dolphin's documented suggestion is 5.
            </span>
            <div class="flex items-center gap-3 pt-1">
              <input
                v-model.number="level"
                type="range"
                min="1"
                max="22"
                step="1"
                class="flex-1 accent-sky-500"
              />
              <span class="w-16 shrink-0 text-right font-mono text-sm text-zinc-200">{{ level }}</span>
            </div>
          </label>
          <label class="flex flex-col gap-1.5">
            <span class="text-sm font-medium text-zinc-200">Chunk size (bytes)</span>
            <span class="text-xs text-zinc-400">
              Must be a power of two between 32768 (32 KiB) and 2097152 (2 MiB). Defaults to 131072 (128 KiB), which matches Dolphin's default.
            </span>
            <select v-model.number="chunkSize" class="mt-1 rounded-md border border-zinc-700 bg-zinc-800/50 px-3 py-1.5 text-sm text-zinc-200">
              <option v-for="size in CHUNK_SIZES" :key="size" :value="size">{{ size }}</option>
            </select>
          </label>
        </div>

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
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
          v-model="verifyAfter"
          label="Verify after conversion"
          description="Re-check each output after writing it and note whether the format supports a full round-trip decode."
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
          @cancel="isBatch ? batch.abort() : abort()"
        >
          {{ previewMode ? 'Preview' : (isBatch && queue.filter(i => i.status === 'pending').length > 1 ? `Compress all (${queue.filter(i => i.status === 'pending').length})` : 'Compress') }}
        </RunButton>
      </div>
    </OperationCard>
  </div>
</template>
