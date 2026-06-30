<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useHashStore } from "~/stores/hash";

const store = useHashStore();
const { input, algos, recursive, maxDepth, result, error, loading } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("hash");
const commandLine = ref("");

const { runBlockReason: inputReason } = usePageGating({
  input,
  emptyInputReason: "Select a file or folder to hash.",
});
const runBlockReason = computed(() => {
  if (inputReason.value) return inputReason.value;
  if (algos.value.length === 0) return "Select at least one hash algorithm.";
  return "";
});
const canRun = computed(() => runBlockReason.value === "");

const ALGO_OPTIONS = [
  { label: "CRC32", value: "crc32" },
  { label: "SHA1", value: "sha1" },
  { label: "MD5", value: "md5" },
  { label: "SHA256", value: "sha256" },
];

function toggleAlgo(value: string) {
  if (algos.value.includes(value)) {
    algos.value = algos.value.filter((a) => a !== value);
  } else {
    algos.value = [...algos.value, value];
  }
}

function hashArgs() {
  return {
    input: input.value,
    algos: algos.value,
    recursive: recursive.value,
    maxDepth: recursive.value ? maxDepth.value : null,
  };
}

async function execute() {
  progress.reset();
  const args = hashArgs();
  commandLine.value = buildCliCommand("cmd_hash", args);
  await run("cmd_hash", args);
}
</script>

<template>
  <div>
    <PageHeader
      title="Hash"
      description="Compute CRC32, SHA1, MD5, and SHA256 checksums for a file or, recursively, for every file in a folder."
      :loading="loading"
      :has-result="!!result"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <FileDropZone
          v-model="input"
          label="Input file or folder"
          :directory="recursive"
          :primary="true"
        />

        <div class="space-y-3 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <span class="block text-sm font-medium text-zinc-300">Algorithms</span>
          <div class="flex flex-wrap gap-x-6 gap-y-2">
            <label
              v-for="opt in ALGO_OPTIONS"
              :key="opt.value"
              class="flex cursor-pointer items-center gap-2 text-sm text-zinc-300"
            >
              <input
                type="checkbox"
                :checked="algos.includes(opt.value)"
                class="h-4 w-4 rounded border-zinc-600 bg-zinc-800 text-sky-500 focus:ring-sky-500"
                @change="toggleAlgo(opt.value)"
              >
              {{ opt.label }}
            </label>
          </div>
        </div>

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <RecursiveOptions
            :recursive="recursive"
            :max-depth="maxDepth"
            @update:recursive="recursive = $event"
            @update:max-depth="maxDepth = $event"
          />
        </div>

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton :loading="loading" :disabled="!canRun" :disabled-reason="runBlockReason" @click="execute">
          Hash
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :error="error" />
    </div>
  </div>
</template>
