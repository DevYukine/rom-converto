<script setup lang="ts">
const props = defineProps<{
  loading?: boolean;
  disabled?: boolean;
  disabledReason?: string;
  batchCurrent?: number;
  batchTotal?: number;
}>();

const showReason = computed(() => !props.loading && !!props.disabled && !!props.disabledReason);

defineEmits<{
  click: [];
  cancel: [];
}>();

const batchCounter = computed(() =>
  props.batchTotal && props.batchTotal > 0 && props.batchCurrent !== undefined && props.batchCurrent >= 0
    ? `${props.batchCurrent + 1} / ${props.batchTotal}`
    : null,
);
</script>

<template>
  <div class="flex w-full items-stretch gap-2">
    <InfoTooltip v-if="showReason" :message="disabledReason!" block class="flex-1">
      <button
        type="button"
        class="w-full rounded-lg bg-gradient-to-r from-sky-600 to-sky-500 px-5 py-2.5 text-sm font-semibold text-white shadow-lg shadow-sky-500/20 transition hover:from-sky-500 hover:to-sky-400 disabled:cursor-not-allowed disabled:opacity-40 disabled:shadow-none"
        :disabled="true"
        :aria-label="disabledReason"
        @click="$emit('click')"
      >
        <slot />
      </button>
    </InfoTooltip>
    <button
      v-else
      type="button"
      class="flex-1 rounded-lg bg-gradient-to-r from-sky-600 to-sky-500 px-5 py-2.5 text-sm font-semibold text-white shadow-lg shadow-sky-500/20 transition hover:from-sky-500 hover:to-sky-400 disabled:cursor-not-allowed disabled:opacity-40 disabled:shadow-none"
      :disabled="loading || disabled"
      @click="$emit('click')"
    >
      <span v-if="loading" class="inline-flex items-center gap-2">
        <svg class="h-4 w-4 animate-spin" fill="none" viewBox="0 0 24 24">
          <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4" />
          <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" />
        </svg>
        {{ batchCounter ?? 'Processing...' }}
      </span>
      <slot v-else />
    </button>
    <button
      v-if="loading"
      type="button"
      class="rounded-lg border border-rose-500/40 bg-rose-500/10 px-4 py-2.5 text-sm font-semibold text-rose-300 transition hover:bg-rose-500/20"
      @click="$emit('cancel')"
    >
      Cancel
    </button>
  </div>
</template>
