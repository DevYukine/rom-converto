<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCsoCompressStore } from "~/stores/cso-compress";

const store = useCsoCompressStore();
const { input, output, format, force, blockSize, result, error, loading, queue } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("cso-compress");

const isBatch = computed(() => queue.value.length > 0);

const FORMAT_OPTIONS = [
  { label: "CSO (PSP, PPSSPP)", value: "cso" },
  { label: "ZSO (PS2 via OPL)", value: "zso" },
];

const batch = useBatchOperation("cso-compress", "cmd_cso_compress", (item) => ({
  inputPath: item.input,
  output: item.output,
  format: format.value,
  force: force.value,
  blockSize: blockSize.value || null,
}));

watch(input, (val) => {
  if (val) output.value = deriveCsoPath(val, format.value);
});

watch(format, (fmt) => {
  if (input.value) output.value = deriveCsoPath(input.value, fmt);
  for (const item of queue.value) {
    if (item.status === "pending") item.output = deriveCsoPath(item.input, fmt);
  }
});

function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, deriveCsoPath(p, format.value));
  }
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, deriveCsoPath(path, format.value));
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  if (isBatch.value) {
    await batch.start(queue, result);
  } else {
    await run("cmd_cso_compress", {
      inputPath: input.value,
      output: output.value,
      format: format.value,
      force: force.value,
      blockSize: blockSize.value || null,
    });
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Compress to CSO/ZSO"
      description="Compress PSP/PS2 ISOs into block-compressed containers. CSO for PSP hardware and PPSSPP, ZSO for PS2 via Open PS2 Loader. Drop multiple .iso files for batch processing."
      :loading="loading || batch.running.value"
      :has-result="!!result"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <!-- Batch mode -->
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
            label="Add more ISO files"
            model-value=""
            :multiple="true"
            :filters="[{ name: 'ISO image', extensions: ['iso'] }]"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, deriveCsoPath(p, format)) }"
            @update:files="handleFiles"
          />
        </template>

        <!-- Single mode: 2-col on large screens -->
        <template v-else>
          <div class="grid gap-5 lg:grid-cols-2">
            <FileDropZone
              :model-value="input"
              label="Input ISO file"
              :multiple="true"
              :filters="[{ name: 'ISO image', extensions: ['iso'] }]"
              :primary="true"
              @update:model-value="handleSingleFile"
              @update:files="handleFiles"
            />

            <FileDropZone
              v-model="output"
              label="Output (auto-derived)"
              :save-dialog="true"
              :filters="[{ name: 'Compressed ISO', extensions: ['cso', 'zso'] }]"
            />
          </div>
        </template>

        <div class="space-y-3 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <SegmentedControl
            :model-value="format"
            label="Format"
            :options="FORMAT_OPTIONS"
            @update:model-value="(v: string) => { format = v as 'cso' | 'zso' }"
          />
          <FlagToggle
            v-model="force"
            label="Force Overwrite"
            description="Overwrite output file if it already exists"
          />
          <label class="flex flex-col gap-1.5">
            <span class="text-sm font-medium text-zinc-200">Block size</span>
            <span class="text-xs text-zinc-400">
              Power of two in bytes. Leave blank for the default of 2048, or 16384 for inputs of 2 GiB and beyond (matching maxcso).
            </span>
            <input
              v-model.number="blockSize"
              type="number"
              placeholder="default"
              class="mt-1 w-40 rounded-md border border-zinc-700 bg-zinc-800/50 px-3 py-1.5 text-sm text-zinc-200"
            />
          </label>
        </div>

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton
          :loading="loading || batch.running.value"
          :disabled="isBatch ? queue.every(i => i.status !== 'pending') : !input"
          @click="execute"
        >
          {{ isBatch ? `Compress ${queue.filter(i => i.status === 'pending').length} Files` : 'Compress' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :result="result" :error="error" />
    </div>
  </div>
</template>
