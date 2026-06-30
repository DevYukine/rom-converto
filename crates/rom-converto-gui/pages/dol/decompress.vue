<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useDolDecompressStore } from "~/stores/dol-decompress";
import type { ReportRecord, RunOutcome } from "~/types/report";

const store = useDolDecompressStore();
const { input, output, onConflict, skipSpaceCheck, outputTemplate, reportFile, result, error, loading, queue, recursive, maxDepth } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { expand } = useFolderScan(["rvz"]);
const scanDepth = () => (recursive.value ? maxDepth.value : 1);
const { run, cancelled, abort } = useOperation({ result, error, loading });
const progress = useProgress("dol-decompress");

const previewMode = ref(false);
const { preview, single: previewSingle, batch: previewBatch, error: previewError } = usePreview("cmd_decompress_disc");

const isBatch = computed(() => queue.value.length > 0);
const { canRun, runBlockReason, templateActive } = usePageGating({ input, queue, outputTemplate });

const commandLine = ref("");

function decompressArgs(inputPath: string, outputPath: string) {
  const tmpl = outputTemplate.value || null;
  return {
    input: inputPath,
    output: tmpl ? null : outputPath || null,
    taskId: "dol-decompress",
    onConflict: onConflict.value,
    skipSpaceCheck: skipSpaceCheck.value,
    outputTemplate: tmpl,
    report: !!reportFile.value,
    reportFile: reportFile.value || null,
    dryRun: previewMode.value,
  };
}

const batch = useBatchOperation("dol-decompress", "cmd_decompress_disc", (item) =>
  decompressArgs(item.input, item.output),
);

watch([input, outputDir], () => {
  if (input.value) output.value = resolve(deriveDiscIsoPath(input.value));
});

watch(outputDir, () => {
  for (const it of queue.value) {
    if (it.status === "pending") it.output = resolve(deriveDiscIsoPath(it.input));
  }
});

async function handleFiles(paths: string[]) {
  for (const p of paths) {
    for (const f of await expand(p, scanDepth())) {
      store.addToQueue(f, resolve(deriveDiscIsoPath(f)));
    }
  }
}

async function handleSingleFile(path: string) {
  const found = await expand(path, scanDepth());
  if (found.length === 1 && found[0] === path && queue.value.length === 0) {
    input.value = path;
  } else {
    for (const f of found) {
      store.addToQueue(f, resolve(deriveDiscIsoPath(f)));
    }
  }
}

async function execute() {
  progress.reset();
  const records: ReportRecord[] = [];
  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_decompress_disc", decompressArgs(rep.input, rep.output)) : "";
    await batch.start(
      queue,
      result,
      { errorRef: error },
      (res) => {
        const record = (res as RunOutcome)?.record;
        if (record) records.push(record);
      },
      async (item, err) => {
        if (reportFile.value) await pushFailedRecord(records, item.input, "decompress", err);
      },
    );
  } else {
    const args = decompressArgs(input.value, output.value);
    commandLine.value = buildCliCommand("cmd_decompress_disc", args);
    await runReportable("cmd_decompress_disc", args, { result, error, loading, cancelled }, records, "decompress");
  }
  if (reportFile.value && records.length) {
    await writeRunReport(reportFile.value, records);
  }
}

async function runPreview() {
  const rep = isBatch.value ? (queue.value.find((i) => i.status === "pending") ?? queue.value[0]) : null;
  commandLine.value = isBatch.value
    ? rep ? buildCliCommand("cmd_decompress_disc", decompressArgs(rep.input, rep.output)) : ""
    : buildCliCommand("cmd_decompress_disc", decompressArgs(input.value, output.value));
  if (isBatch.value) {
    await previewBatch(queue, (item) => decompressArgs(item.input, item.output));
  } else {
    await previewSingle(decompressArgs(input.value, output.value));
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
      title="Decompress GameCube RVZ"
      description="Decompress an RVZ file back to a raw GameCube ISO. Drop multiple files for batch processing."
      :loading="loading || batch.running.value"
      :has-result="!!result"
      :has-error="!!error"
    />

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
            :filters="[{ name: 'RVZ', extensions: ['rvz'] }]"
            @update:model-value="(p: string) => { if (p) handleSingleFile(p) }"
            @update:files="handleFiles"
          />
        </template>

        <div v-else class="grid gap-5 lg:grid-cols-2">
          <FileDropZone
            :model-value="input"
            label="Input RVZ"
            :multiple="true"
            :filters="[{ name: 'RVZ', extensions: ['rvz'] }]"
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
              :filters="[{ name: 'GameCube disc', extensions: ['iso', 'gcm'] }]"
            />
          </InfoTooltip>
          <FileDropZone
            v-else
            v-model="output"
            label="Output file (auto-filled)"
            :save-dialog="true"
            :filters="[{ name: 'GameCube disc', extensions: ['iso', 'gcm'] }]"
          />
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
            placeholder="e.g. {console}/{title}.{ext}"
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
          @cancel="isBatch ? batch.abort() : abort()"
        >
          {{ previewMode ? 'Preview' : (isBatch && queue.filter(i => i.status === 'pending').length > 1 ? `Decompress All (${queue.filter(i => i.status === 'pending').length})` : 'Decompress') }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :preview="preview" :cancelled="cancelled ? 'Operation cancelled.' : undefined" :error="error" />
    </div>
  </div>
</template>
