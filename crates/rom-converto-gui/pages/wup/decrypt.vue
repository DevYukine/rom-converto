<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useWupDecryptStore } from "~/stores/wup-decrypt";

const store = useWupDecryptStore();
const { input, output, result, error, loading } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("wup-decrypt");
const commandLine = ref("");

watch(input, (val) => {
  if (val && !output.value) {
    output.value = deriveDecryptedWupPath(val);
  }
});

function deriveDecryptedWupPath(dir: string): string {
  const trimmed = dir.replace(/[\\/]+$/, "");
  return `${trimmed}_decrypted`;
}

async function execute() {
  progress.reset();
  const out = output.value || deriveDecryptedWupPath(input.value);
  const args = { input: input.value, output: out };
  commandLine.value = buildCliCommand("cmd_wup_decrypt", args);
  await run("cmd_wup_decrypt", args);
}
</script>

<template>
  <div>
    <PageHeader
      title="Decrypt NUS title"
      description="Decrypt a Wii U NUS directory into a loadiine-shaped meta/code/content tree that Cemu can install or load directly. Title key is derived automatically when no ticket is present."
      :loading="loading"
      :has-result="!!result"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <FileDropZone
          v-model="input"
          label="Input NUS directory"
          :directory="true"
          :primary="true"
        />

        <FileDropZone
          v-model="output"
          label="Output Directory"
          :directory="true"
        />

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton
          :loading="loading"
          :disabled="!input"
          @click="execute"
        >
          Decrypt
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :error="error" />
    </div>
  </div>
</template>
