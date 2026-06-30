<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useDolDecompressStore } from "~/stores/dol-decompress";
import type { ReportRecord, RunOutcome } from "~/types/report";

const store = useDolDecompressStore();
const { input, output, onConflict, skipSpaceCheck, outputTemplate, reportFile, result, error, loading, queue } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { run, cancelled, abort } = useOperation({ result, error, loading });
const progress = useProgress("dol-decompress");

const isBatch = computed(() => queue.value.length > 0);

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

function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, resolve(deriveDiscIsoPath(p)));
  }
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, resolve(deriveDiscIsoPath(path)));
  } else {
    input.value = path;
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
      undefined,
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
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, resolve(deriveDiscIsoPath(p))) }"
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

          <FileDropZone
            v-model="output"
            label="Output file (auto-filled)"
            :save-dialog="true"
            :filters="[{ name: 'GameCube disc', extensions: ['iso', 'gcm'] }]"
          />
        </div>

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <ConflictPolicyControl v-model="onConflict" />
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

        <RunButton
          :loading="loading || batch.running.value"
          :batch-current="batch.currentIndex.value"
          :batch-total="queue.length"
          :disabled="isBatch ? queue.every(i => i.status !== 'pending') : !input"
          @click="execute"
          @cancel="isBatch ? batch.abort() : abort()"
        >
          {{ isBatch && queue.filter(i => i.status === 'pending').length > 1 ? `Decompress All (${queue.filter(i => i.status === 'pending').length})` : 'Decompress' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :cancelled="cancelled ? 'Operation cancelled.' : undefined" :error="error" />
    </div>
  </div>
</template>
