<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useRvlInfoStore } from "~/stores/rvl-info";

const store = useRvlInfoStore();
const { input, info, rawJson, error, loading } = storeToRefs(store);

const RVL_FILTERS = [
  { name: "Wii disc image", extensions: ["iso", "wbfs", "rvz"] },
];
</script>

<template>
  <div>
    <PageHeader
      title="Wii info"
      description="Inspect a Wii disc image (.iso, .wbfs, or .rvz): game ID, region, partition layout, TMD title id and version, IMET banner names, and the channel icon."
      :loading="loading"
      :has-result="!!info"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <FileDropZone
          v-model="input"
          label="Disc image"
          :primary="true"
          :filters="RVL_FILTERS"
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

    <div class="mt-4">
      <OutputLog :error="error" />
    </div>

    <details v-if="rawJson" class="mt-4">
      <summary class="cursor-pointer text-sm text-zinc-500">Raw JSON payload</summary>
      <pre class="mt-2 overflow-auto rounded bg-black/40 p-3 text-xs">{{ rawJson }}</pre>
    </details>
  </div>
</template>
