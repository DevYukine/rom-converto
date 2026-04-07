<script setup lang="ts">
defineProps<{
  loading?: boolean;
  disabled?: boolean;
}>();

defineEmits<{
  click: [];
}>();
</script>

<template>
  <button
    type="button"
    class="w-full rounded-lg bg-gradient-to-r from-sky-600 to-sky-500 px-5 py-2.5 text-sm font-semibold text-white shadow-lg shadow-sky-500/20 transition hover:from-sky-500 hover:to-sky-400 disabled:cursor-not-allowed disabled:opacity-40 disabled:shadow-none"
    :disabled="loading || disabled"
    @click="$emit('click')"
  >
    <span v-if="loading" class="inline-flex items-center gap-2">
      <svg class="h-4 w-4 animate-spin" fill="none" viewBox="0 0 24 24">
        <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4" />
        <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" />
      </svg>
      Processing...
    </span>
    <slot v-else />
  </button>
</template>
