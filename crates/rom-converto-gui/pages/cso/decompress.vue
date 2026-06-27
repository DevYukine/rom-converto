<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCsoDecompressStore } from "~/stores/cso-decompress";

const store = useCsoDecompressStore();
const { input, output, force, result, error, loading, queue } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { run } = useOperation({ result, error, loading });
const progress = useProgress("cso-decompress");

const isBatch = computed(() => queue.value.length > 0);

const commandLine = ref("");

function csoArgs(inputPath: string, outputPath: string) {
  return { inputPath, output: outputPath, force: force.value };
}

const batch = useBatchOperation("cso-decompress", "cmd_cso_decompress", (item) =>
  csoArgs(item.input, item.output),
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
    commandLine.value = rep ? buildCliCommand("cmd_cso_decompress", csoArgs(rep.input, rep.output)) : "";
    await batch.start(queue, result);
  } else {
    const args = csoArgs(input.value, output.value);
    commandLine.value = buildCliCommand("cmd_cso_decompress", args);
    await run("cmd_cso_decompress", args);
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Decompress CSO/ZSO"
      description="Restore the original ISO from a CSO or ZSO container. Drop multiple files for batch processing."
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
            label="Add more CSO/ZSO files"
            model-value=""
            :multiple="true"
            :filters="[{ name: 'Compressed ISO', extensions: ['cso', 'zso'] }]"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, resolve(deriveDiscIsoPath(p))) }"
            @update:files="handleFiles"
          />
        </template>

        <template v-else>
          <div class="grid gap-5 lg:grid-cols-2">
            <FileDropZone
              :model-value="input"
              label="Input CSO/ZSO file"
              :multiple="true"
              :filters="[{ name: 'Compressed ISO', extensions: ['cso', 'zso'] }]"
              :primary="true"
              @update:model-value="handleSingleFile"
              @update:files="handleFiles"
            />

            <FileDropZone
              v-model="output"
              label="Output file (auto-filled)"
              :save-dialog="true"
              :filters="[{ name: 'ISO image', extensions: ['iso'] }]"
            />
          </div>
        </template>

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <FlagToggle
            v-model="force"
            label="Force overwrite"
            description="Overwrite output file if it already exists"
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
