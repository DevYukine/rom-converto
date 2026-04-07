<script setup lang="ts">
defineProps<{
  percent: number;
  message: string;
  running: boolean;
}>();
</script>

<template>
  <div v-if="running || percent > 0" class="space-y-1">
    <div class="flex items-center justify-between text-xs text-zinc-400">
      <span>{{ message }}</span>
      <span>{{ percent }}%</span>
    </div>
    <div class="h-2 overflow-hidden rounded-full bg-zinc-700">
      <div
        class="h-full rounded-full bg-sky-500 transition-all duration-200"
        :style="{ width: `${percent}%` }"
      />
    </div>
  </div>
</template>
