<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useDolDecompressStore } from "~/stores/dol-decompress";

const store = useDolDecompressStore();
const { input, output, onConflict, skipSpaceCheck, result, error, loading, queue } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { run, cancelled, abort } = useOperation({ result, error, loading });
const progress = useProgress("dol-decompress");

const isBatch = computed(() => queue.value.length > 0);

const commandLine = ref("");

function decompressArgs(inputPath: string, outputPath: string) {
  return { input: inputPath, output: outputPath || null, taskId: "dol-decompress", onConflict: onConflict.value, skipSpaceCheck: skipSpaceCheck.value };
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
  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_decompress_disc", decompressArgs(rep.input, rep.output)) : "";
    await batch.start(queue, result);
  } else {
    const args = decompressArgs(input.value, output.value);
    commandLine.value = buildCliCommand("cmd_decompress_disc", args);
    await run("cmd_decompress_disc", args);
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

        <OutputDirField v-model="outputDir" />

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
