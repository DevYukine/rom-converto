<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useChdInfoStore } from "~/stores/chd-info";

const store = useChdInfoStore();
const { input, info, rawJson, error, loading } = storeToRefs(store);

const CHD_FILTERS = [
  { name: "CHD file", extensions: ["chd", "zip", "7z", "rar", "tar", "tgz", "gz"] },
];
</script>

<template>
  <div>
    <PageHeader
      title="CHD info"
      description="Read a .chd v5 file: compressors, hunk geometry, SHA-1 triplet, per-track CHT2 metadata, and the chdman build string (when present)."
      :loading="loading"
      :has-result="!!info"
      :has-error="!!error"
    />

    <div class="mb-4">
      <OutputLog :error="error" />
    </div>

    <OperationCard>
      <div class="space-y-5">
        <FileDropZone
          v-model="input"
          label="CHD file"
          :primary="true"
          :filters="CHD_FILTERS"
        />

        <RunButton
          :loading="loading"
          :disabled="!input"
          @click="store.execute"
        >
          Read info
        </RunButton>

        <RomInfoCard v-if="info" :info="info" />
      </div>
    </OperationCard>

    <details v-if="rawJson" class="mt-4">
      <summary class="cursor-pointer text-sm text-zinc-500">Raw JSON payload</summary>
      <pre class="mt-2 overflow-auto rounded bg-black/40 p-3 text-xs">{{ rawJson }}</pre>
    </details>
  </div>
</template>
