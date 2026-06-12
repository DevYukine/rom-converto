<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useRvlDecompressStore } from "~/stores/rvl-decompress";

const store = useRvlDecompressStore();
const { input, output, format, result, error, loading, queue } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("rvl-decompress");

const isBatch = computed(() => queue.value.length > 0);

const formatOptions = [
  { label: "ISO", value: "iso" },
  { label: "WBFS", value: "wbfs" },
];

const outputFilters = computed(() =>
  format.value === "wbfs"
    ? [
        { name: "WBFS", extensions: ["wbfs"] },
        { name: "ISO", extensions: ["iso"] },
      ]
    : [
        { name: "ISO", extensions: ["iso"] },
        { name: "WBFS", extensions: ["wbfs"] },
      ],
);

const batch = useBatchOperation("rvl-decompress", "cmd_decompress_disc", (item) => ({
  input: item.input,
  output: item.output || null,
  taskId: "rvl-decompress",
}));

watch(input, (val) => {
  if (val) output.value = deriveDiscPath(val, format.value);
});

function setFormat(value: string) {
  const next = value === "wbfs" ? "wbfs" : "iso";
  if (next === format.value) return;
  const prev = format.value;
  format.value = next;

  if (input.value && (!output.value || output.value === deriveDiscPath(input.value, prev))) {
    output.value = deriveDiscPath(input.value, next);
  }
  for (const item of queue.value) {
    if (item.status === "pending" && item.output === deriveDiscPath(item.input, prev)) {
      item.output = deriveDiscPath(item.input, next);
    }
  }
}

function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, deriveDiscPath(p, format.value));
  }
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, deriveDiscPath(path, format.value));
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  if (isBatch.value) {
    await batch.start(queue, result);
  } else {
    await run("cmd_decompress_disc", {
      input: input.value,
      output: output.value || null,
      taskId: "rvl-decompress",
    });
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Decompress Wii RVZ"
      description="Decompress an RVZ file back to a raw Wii disc image. Pick ISO or WBFS with the toggle. Drop multiple files for batch processing."
      :loading="loading || batch.running.value"
      :has-result="!!result"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <SegmentedControl
          :model-value="format"
          :options="formatOptions"
          label="Output format"
          :disabled="loading || batch.running.value"
          @update:model-value="setFormat"
        />

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
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, deriveDiscPath(p, format)) }"
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
            label="Output (auto-derived)"
            :save-dialog="true"
            :filters="outputFilters"
          />
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
          {{ isBatch ? `Decompress ${queue.filter(i => i.status === 'pending').length} Files` : 'Decompress' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :result="result" :error="error" />
    </div>
  </div>
</template>
