<script setup lang="ts">
const input = ref("");
const output = ref("");

const { result, error, loading, run } = useOperation();
const progress = useProgress("compress");

watch(input, (val) => {
  if (val) output.value = deriveCompressedPath(val);
});

async function execute() {
  progress.reset();
  await run("cmd_compress_rom", {
    input: input.value,
    output: output.value || null,
  });
}
</script>

<template>
  <div class="mx-auto max-w-xl space-y-6">
    <h2 class="text-xl font-semibold">Compress ROM</h2>
    <p class="text-sm text-zinc-400">
      Compress a decrypted 3DS ROM to Z3DS format (.zcia, .zcci, .zcxi, .z3dsx).
    </p>

    <FileDropZone
      v-model="input"
      label="Input ROM"
      :filters="[{ name: '3DS ROM', extensions: ['cia', 'cci', '3ds', 'cxi', '3dsx'] }]"
      :primary="true"
    />

    <FileDropZone
      v-model="output"
      label="Output (auto-derived)"
      :save-dialog="true"
      :filters="[{ name: 'Z3DS', extensions: ['zcia', 'zcci', 'zcxi', 'z3dsx'] }]"
    />

    <ProgressBar
      :percent="progress.percent.value"
      :message="progress.message.value"
      :running="progress.running.value"
    />

    <RunButton :loading="loading" :disabled="!input" @click="execute">
      Compress
    </RunButton>

    <OutputLog :result="result" :error="error" />
  </div>
</template>
