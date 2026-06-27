<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCtrDecompressStore } from "~/stores/ctr-decompress";

const store = useCtrDecompressStore();
const { input, output, result, error, loading, queue } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { run } = useOperation({ result, error, loading });
const progress = useProgress("decompress");

const isBatch = computed(() => queue.value.length > 0);

const commandLine = ref("");

function decompressArgs(inputPath: string, outputPath: string) {
  return { input: inputPath, output: outputPath || null };
}

const batch = useBatchOperation("decompress", "cmd_decompress_rom", (item) =>
  decompressArgs(item.input, item.output),
);

watch([input, outputDir], () => {
  if (input.value) output.value = resolve(deriveDecompressedPath(input.value));
});

watch(outputDir, () => {
  for (const it of queue.value) {
    if (it.status === "pending") it.output = resolve(deriveDecompressedPath(it.input));
  }
});

function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, resolve(deriveDecompressedPath(p)));
  }
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, resolve(deriveDecompressedPath(path)));
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_decompress_rom", decompressArgs(rep.input, rep.output)) : "";
    await batch.start(queue, result);
  } else {
    const args = decompressArgs(input.value, output.value);
    commandLine.value = buildCliCommand("cmd_decompress_rom", args);
    await run("cmd_decompress_rom", args);
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Decompress ROM"
      description="Decompress Z3DS files back to their original ROM format. Drop multiple files for batch processing."
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
            :filters="[{ name: 'Z3DS', extensions: ['zcia', 'zcci', 'zcxi', 'z3dsx'] }]"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, resolve(deriveDecompressedPath(p))) }"
            @update:files="handleFiles"
          />
        </template>

        <div v-else class="grid gap-5 lg:grid-cols-2">
          <FileDropZone
            :model-value="input"
            label="Input Z3DS file"
            :multiple="true"
            :filters="[{ name: 'Z3DS', extensions: ['zcia', 'zcci', 'zcxi', 'z3dsx'] }]"
            :primary="true"
            @update:model-value="handleSingleFile"
            @update:files="handleFiles"
          />

          <FileDropZone
            v-model="output"
            label="Output file (auto-filled)"
            :save-dialog="true"
            :filters="[{ name: '3DS ROM', extensions: ['cia', 'cci', 'cxi', '3dsx'] }]"
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
        >
          {{ isBatch && queue.filter(i => i.status === 'pending').length > 1 ? `Decompress All (${queue.filter(i => i.status === 'pending').length})` : 'Decompress' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :error="error" />
    </div>
  </div>
</template>
