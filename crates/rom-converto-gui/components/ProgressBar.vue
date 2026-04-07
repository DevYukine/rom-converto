<script setup lang="ts">
defineProps<{
  percent: number;
  message: string;
  running: boolean;
}>();
</script>

<template>
  <div v-if="running || percent > 0" class="space-y-2">
    <div class="h-2.5 w-full overflow-hidden rounded-full bg-zinc-700/50">
      <div
        class="h-full rounded-full bg-gradient-to-r from-sky-500 to-sky-400 transition-all duration-300"
        :class="{ 'animate-pulse': running && percent === 0 }"
        :style="{ width: `${Math.min(percent, 100)}%` }"
      />
    </div>
    <div class="flex items-center justify-between text-xs text-zinc-400">
      <span>{{ message }}</span>
      <span v-if="percent > 0">{{ Math.min(percent, 100) }}%</span>
    </div>
  </div>
</template>
