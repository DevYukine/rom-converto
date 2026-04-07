<script setup lang="ts">
const input = ref("");
const parent = ref("");
const fix = ref(false);

const { result, error, loading, run } = useOperation();
const progress = useProgress("chd-verify");

async function execute() {
  progress.reset();
  await run("cmd_chd_verify", {
    input: input.value,
    parent: parent.value || null,
    fix: fix.value,
  });
}
</script>

<template>
  <div class="mx-auto max-w-xl space-y-6">
    <h2 class="text-xl font-semibold">Verify CHD</h2>
    <p class="text-sm text-zinc-400">
      Verify the integrity of a CHD file by checking SHA1 hashes.
    </p>

    <FileDropZone
      v-model="input"
      label="Input CHD File"
      :filters="[{ name: 'CHD', extensions: ['chd'] }]"
      :primary="true"
    />

    <FileDropZone
      v-model="parent"
      label="Parent CHD (optional)"
      :filters="[{ name: 'CHD', extensions: ['chd'] }]"
    />

    <div class="rounded-lg border border-zinc-800 p-4">
      <FlagToggle
        v-model="fix"
        label="Fix SHA1"
        description="Automatically fix incorrect SHA1 values in the header"
      />
    </div>

    <ProgressBar
      :percent="progress.percent.value"
      :message="progress.message.value"
      :running="progress.running.value"
    />

    <RunButton :loading="loading" :disabled="!input" @click="execute">
      Verify
    </RunButton>

    <OutputLog :result="result" :error="error" />
  </div>
</template>
