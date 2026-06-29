<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useChdCompressStore } from "~/stores/chd-compress";

const store = useChdCompressStore();
const { input, output, force, zstd, mode, hunkSize, result, error, loading, queue } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { run, cancelled, abort } = useOperation({ result, error, loading });
const progress = useProgress("chd-compress");

const MODE_OPTIONS = [
  { label: "Auto", value: "auto" },
  { label: "CD", value: "cd" },
  { label: "DVD", value: "dvd" },
];

const isBatch = computed(() => queue.value.length > 0);
const commandLine = ref("");

function chdArgs(inputPath: string, outputPath: string) {
  return {
    inputPath,
    output: outputPath,
    force: force.value,
    zstd: zstd.value,
    mode: mode.value === "auto" ? null : mode.value,
    hunkSize: hunkSize.value || null,
  };
}

const batch = useBatchOperation("chd-compress", "cmd_chd_compress", (item) =>
  chdArgs(item.input, item.output),
);

watch([input, outputDir], () => {
  if (input.value) output.value = resolve(deriveChdPath(input.value));
});

watch(outputDir, () => {
  for (const it of queue.value) {
    if (it.status === "pending") it.output = resolve(deriveChdPath(it.input));
  }
});

function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, resolve(deriveChdPath(p)));
  }
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, resolve(deriveChdPath(path)));
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_chd_compress", chdArgs(rep.input, rep.output)) : "";
    await batch.start(queue, result);
  } else {
    const args = chdArgs(input.value, output.value);
    commandLine.value = buildCliCommand("cmd_chd_compress", args);
    await run("cmd_chd_compress", args);
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Compress to CHD"
      description="Compress disc images to CHD. CUE/BIN and CD-media ISOs (PS1, PS2-CD) become CD-mode CHDs; PS2-DVD and PSP ISOs become DVD-mode CHDs (media type and hunk size are auto-detected). Drop multiple files for batch processing."
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
            label="Add more disc images"
            model-value=""
            :multiple="true"
            :filters="[{ name: 'Disc image', extensions: ['cue', 'iso'] }]"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, resolve(deriveChdPath(p))) }"
            @update:files="handleFiles"
          />
        </template>

        <template v-else>
          <div class="grid gap-5 lg:grid-cols-2">
            <FileDropZone
              :model-value="input"
              label="Input disc image (.cue or .iso)"
              :multiple="true"
              :filters="[{ name: 'Disc image', extensions: ['cue', 'iso'] }]"
              :primary="true"
              @update:model-value="handleSingleFile"
              @update:files="handleFiles"
            />

            <FileDropZone
              v-model="output"
              label="Output file (auto-filled)"
              :save-dialog="true"
              :filters="[{ name: 'CHD', extensions: ['chd'] }]"
            />
          </div>
        </template>

        <div class="space-y-3 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <FlagToggle
            v-model="force"
            label="Force overwrite"
            description="Overwrite output file if it already exists"
          />
          <FlagToggle
            v-model="zstd"
            label="zstd codec (DVD mode)"
            description="Better ratio, but the CHD is rejected by AetherSX2/NetherSX2. Leave off for maximum compatibility"
          />
          <div class="space-y-1.5">
            <SegmentedControl
              :model-value="mode"
              label="Disc mode"
              :options="MODE_OPTIONS"
              @update:model-value="(v: string) => { mode = v as 'auto' | 'cd' | 'dvd' }"
            />
            <p class="text-xs text-zinc-500">
              Auto picks CD or DVD from the image. Override only when detection is wrong.
            </p>
          </div>
          <label class="flex flex-col gap-1.5">
            <span class="text-sm font-medium text-zinc-200">Hunk size</span>
            <input
              v-model.number="hunkSize"
              type="number"
              placeholder="auto"
              class="mt-1 w-40 rounded-md border border-zinc-700 bg-zinc-800/50 px-3 py-1.5 text-sm text-zinc-200"
            />
          </label>
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
          {{ isBatch && queue.filter(i => i.status === 'pending').length > 1 ? `Compress All (${queue.filter(i => i.status === 'pending').length})` : 'Compress' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :cancelled="cancelled ? 'Operation cancelled.' : undefined" :error="error" />
    </div>
  </div>
</template>
