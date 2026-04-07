<script setup lang="ts">
const input = ref("");
const output = ref("");
const parent = ref("");

const { result, error, loading, run } = useOperation();
const progress = useProgress("chd-extract");

watch(input, (val) => {
  if (val) output.value = deriveCuePath(val);
});

async function execute() {
  progress.reset();
  await run("cmd_chd_extract", {
    input: input.value,
    output: output.value,
    parent: parent.value || null,
  });
}
</script>

<template>
  <div class="mx-auto max-w-xl space-y-6">
    <h2 class="text-xl font-semibold">Extract CHD</h2>
    <p class="text-sm text-zinc-400">
      Extract a CHD file to BIN/CUE disc image.
    </p>

    <FileDropZone
      v-model="input"
      label="Input CHD File"
      :filters="[{ name: 'CHD', extensions: ['chd'] }]"
      :primary="true"
    />

    <FileDropZone
      v-model="output"
      label="Output CUE File (auto-derived)"
      :save-dialog="true"
      :filters="[{ name: 'CUE Sheet', extensions: ['cue'] }]"
    />

    <FileDropZone
      v-model="parent"
      label="Parent CHD (optional)"
      :filters="[{ name: 'CHD', extensions: ['chd'] }]"
    />

    <ProgressBar
      :percent="progress.percent.value"
      :message="progress.message.value"
      :running="progress.running.value"
    />

    <RunButton :loading="loading" :disabled="!input || !output" @click="execute">
      Extract
    </RunButton>

    <OutputLog :result="result" :error="error" />
  </div>
</template>
