<script setup lang="ts">
const input = ref("");
const output = ref("");

const { result, error, loading, run } = useOperation();
const progress = useProgress("decompress");

watch(input, (val) => {
  if (val) output.value = deriveDecompressedPath(val);
});

async function execute() {
  progress.reset();
  await run("cmd_decompress_rom", {
    input: input.value,
    output: output.value || null,
  });
}
</script>

<template>
  <div class="mx-auto max-w-xl space-y-6">
    <h2 class="text-xl font-semibold">Decompress ROM</h2>
    <p class="text-sm text-zinc-400">
      Decompress a Z3DS file back to its original ROM format.
    </p>

    <FileDropZone
      v-model="input"
      label="Input Z3DS File"
      :filters="[{ name: 'Z3DS', extensions: ['zcia', 'zcci', 'zcxi', 'z3dsx'] }]"
      :primary="true"
    />

    <FileDropZone
      v-model="output"
      label="Output (auto-derived)"
      :save-dialog="true"
      :filters="[{ name: '3DS ROM', extensions: ['cia', 'cci', 'cxi', '3dsx'] }]"
    />

    <ProgressBar
      :percent="progress.percent.value"
      :message="progress.message.value"
      :running="progress.running.value"
    />

    <RunButton :loading="loading" :disabled="!input" @click="execute">
      Decompress
    </RunButton>

    <OutputLog :result="result" :error="error" />
  </div>
</template>
