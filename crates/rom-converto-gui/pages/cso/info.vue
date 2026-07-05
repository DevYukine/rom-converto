<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCsoInfoStore } from "~/stores/cso-info";

const store = useCsoInfoStore();
const { input, info, rawJson, error, loading } = storeToRefs(store);

const CSO_FILTERS = [
  { name: "Compressed ISO", extensions: ["cso", "zso", "dax"] },
];
</script>

<template>
  <div>
    <PageHeader
      title="CSO/ZSO/DAX info"
      description="Read a .cso, .zso, or .dax container: format and version, block geometry, index shift, raw block count, and compression ratio."
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
          label="CSO/ZSO/DAX file"
          :primary="true"
          :filters="CSO_FILTERS"
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
