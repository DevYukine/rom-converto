<script setup lang="ts">
defineProps<{ modelValue: string }>();
defineEmits<{ "update:modelValue": [value: string] }>();
</script>

<template>
  <FileDropZone
    :model-value="modelValue"
    label="Output Directory (optional)"
    placeholder="Same as source (default)"
    :directory="true"
    @update:model-value="$emit('update:modelValue', $event)"
  />
</template>
