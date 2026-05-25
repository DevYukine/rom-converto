<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useDolInfoStore } from "~/stores/dol-info";

const store = useDolInfoStore();
const { input, info, rawJson, error, loading } = storeToRefs(store);

const DOL_FILTERS = [
  { name: "GameCube disc image", extensions: ["iso", "gcm", "rvz"] },
];
</script>

<template>
  <div>
    <PageHeader
      title="GameCube info"
      description="Read a GameCube .iso, .gcm, or .rvz and show the disc header plus the opening.bnr metadata, including the 96x32 banner image."
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
          :filters="DOL_FILTERS"
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
