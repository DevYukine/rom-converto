<script setup lang="ts">
const props = defineProps<{
  percent: number;
  message: string;
  running: boolean;
}>();

const labelId = useId();
const indeterminate = computed(() => props.running && props.percent === 0);
const statusText = computed(() => props.message || "Starting...");
</script>

<template>
  <div v-if="running || percent > 0" class="space-y-2">
    <div
      class="h-2.5 w-full overflow-hidden rounded-full bg-zinc-700/50"
      role="progressbar"
      :aria-valuenow="indeterminate ? undefined : Math.min(percent, 100)"
      aria-valuemin="0"
      aria-valuemax="100"
      :aria-labelledby="labelId"
    >
      <div
        class="h-full rounded-full bg-gradient-to-r from-sky-500 to-sky-400 transition-all duration-300"
        :class="{ 'animate-pulse': indeterminate }"
        :style="{ width: `${Math.min(percent, 100)}%` }"
      />
    </div>
    <div class="flex items-center justify-between text-xs text-zinc-400">
      <span :id="labelId">{{ statusText }}</span>
      <span v-if="percent > 0">{{ Math.min(percent, 100) }}%</span>
    </div>
  </div>
</template>
