<script setup lang="ts">
const input = ref("");
const output = ref("");

const { result, error, loading, run } = useOperation();
const progress = useProgress("decrypt");

watch(input, (val) => {
  if (val) output.value = deriveDecryptedPath(val);
});

async function execute() {
  progress.reset();
  await run("cmd_decrypt_rom", {
    input: input.value,
    output: output.value,
  });
}
</script>

<template>
  <div class="mx-auto max-w-xl space-y-6">
    <h2 class="text-xl font-semibold">Decrypt ROM</h2>
    <p class="text-sm text-zinc-400">
      Decrypt an encrypted 3DS ROM (.cia, .3ds, .cci, .cxi).
    </p>

    <FileDropZone
      v-model="input"
      label="Input ROM"
      :filters="[{ name: '3DS ROM', extensions: ['cia', '3ds', 'cci', 'cxi'] }]"
      :primary="true"
    />

    <FileDropZone
      v-model="output"
      label="Output Path"
      :save-dialog="true"
      :filters="[{ name: '3DS ROM', extensions: ['cia', '3ds', 'cci', 'cxi'] }]"
    />

    <ProgressBar
      :percent="progress.percent.value"
      :message="progress.message.value"
      :running="progress.running.value"
    />

    <RunButton :loading="loading" :disabled="!input || !output" @click="execute">
      Decrypt
    </RunButton>

    <OutputLog :result="result" :error="error" />
  </div>
</template>
