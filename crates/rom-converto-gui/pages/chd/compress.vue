<script setup lang="ts">
const cuePath = ref("");
const output = ref("");
const force = ref(false);

const { result, error, loading, run } = useOperation();
const progress = useProgress("chd-compress");

watch(cuePath, (val) => {
  if (val) output.value = deriveChdPath(val);
});

async function execute() {
  progress.reset();
  await run("cmd_chd_compress", {
    cuePath: cuePath.value,
    output: output.value,
    force: force.value,
  });
}
</script>

<template>
  <div class="mx-auto max-w-xl space-y-6">
    <h2 class="text-xl font-semibold">Compress to CHD</h2>
    <p class="text-sm text-zinc-400">
      Compress a BIN/CUE disc image to CHD format.
    </p>

    <FileDropZone
      v-model="cuePath"
      label="Input CUE File"
      :filters="[{ name: 'CUE Sheet', extensions: ['cue'] }]"
      :primary="true"
    />

    <FileDropZone
      v-model="output"
      label="Output CHD File (auto-derived)"
      :save-dialog="true"
      :filters="[{ name: 'CHD', extensions: ['chd'] }]"
    />

    <div class="rounded-lg border border-zinc-800 p-4">
      <FlagToggle
        v-model="force"
        label="Force Overwrite"
        description="Overwrite output file if it already exists"
      />
    </div>

    <ProgressBar
      :percent="progress.percent.value"
      :message="progress.message.value"
      :running="progress.running.value"
    />

    <RunButton :loading="loading" :disabled="!cuePath || !output" @click="execute">
      Compress
    </RunButton>

    <OutputLog :result="result" :error="error" />
  </div>
</template>
