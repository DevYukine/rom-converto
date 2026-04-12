<script setup lang="ts">
defineProps<{
  label: string;
  valid: boolean;
}>();
</script>

<template>
  <div class="flex items-center gap-2 text-sm">
    <svg v-if="valid" class="h-4 w-4 text-emerald-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
      <path stroke-linecap="round" stroke-linejoin="round" d="M5 13l4 4L19 7" />
    </svg>
    <svg v-else class="h-4 w-4 text-red-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
      <path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" />
    </svg>
    <span :class="valid ? 'text-zinc-300' : 'text-zinc-500'">{{ label }}</span>
  </div>
</template>
