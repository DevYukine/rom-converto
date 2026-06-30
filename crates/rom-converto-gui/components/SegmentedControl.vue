<script setup lang="ts">
const props = defineProps<{
  modelValue: string;
  options: { label: string; value: string }[];
  label?: string;
  disabled?: boolean;
  disabledOptions?: string[];
  disabledReason?: string;
}>();

const labelId = useId();

defineEmits<{
  "update:modelValue": [value: string];
}>();

const isDisabled = (value: string) =>
  props.disabled || (props.disabledOptions?.includes(value) ?? false);
</script>

<template>
  <div class="space-y-1.5">
    <label v-if="label" :id="labelId" class="block text-sm font-medium text-zinc-300">{{ label }}</label>
    <div
      role="group"
      :aria-labelledby="label ? labelId : undefined"
      class="inline-flex gap-0.5 rounded-lg border border-zinc-700 bg-zinc-800/30 p-0.5"
    >
      <template v-for="option in options" :key="option.value">
        <InfoTooltip
          v-if="disabledOptions?.includes(option.value) && disabledReason"
          :message="disabledReason"
        >
          <button
            type="button"
            disabled
            :aria-pressed="option.value === modelValue"
            class="cursor-not-allowed rounded-md px-4 py-1.5 text-sm font-medium opacity-50 transition"
            :class="option.value === modelValue ? 'bg-sky-500 text-white shadow' : 'text-zinc-400'"
          >
            {{ option.label }}
          </button>
        </InfoTooltip>
        <button
          v-else
          type="button"
          :disabled="isDisabled(option.value)"
          :aria-pressed="option.value === modelValue"
          class="rounded-md px-4 py-1.5 text-sm font-medium transition"
          :class="[
            option.value === modelValue
              ? 'bg-sky-500 text-white shadow'
              : 'text-zinc-400 hover:text-zinc-200',
            isDisabled(option.value) ? 'cursor-not-allowed opacity-50' : 'cursor-pointer',
          ]"
          @click="!isDisabled(option.value) && option.value !== modelValue && $emit('update:modelValue', option.value)"
        >
          {{ option.label }}
        </button>
      </template>
    </div>
  </div>
</template>
