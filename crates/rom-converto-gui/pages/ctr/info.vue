<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCtrInfoStore } from "~/stores/ctr-info";

const store = useCtrInfoStore();
const { input, info, rawJson, error, loading } = storeToRefs(store);

const CTR_FILTERS = [
  { name: "3DS container", extensions: ["cia", "3ds", "cci", "cxi", "ncch"] },
];
</script>

<template>
  <div>
    <PageHeader
      title="3DS info"
      description="Inspect a CIA, CCI, or NCCH and show the SMDH metadata: multilingual titles, region, age ratings, and the embedded 48x48 icon."
      :loading="loading"
      :has-result="!!info"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <FileDropZone
          v-model="input"
          label="Input file"
          :primary="true"
          :filters="CTR_FILTERS"
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
