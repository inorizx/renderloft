<script setup lang="ts">
interface Props {
  label: string
  description?: string
  error?: string
  required?: boolean
  for?: string
}

defineProps<Props>()
</script>

<template>
  <div>
    <div class="flex items-center justify-between gap-3">
      <div class="flex min-w-0 items-center gap-1">
        <label :for="$props.for" class="truncate text-sm font-medium">{{ label }}</label>
        <span v-if="required" class="text-destructive" aria-hidden="true">*</span>
      </div>
      <div v-if="$slots.actions" class="shrink-0"><slot name="actions" /></div>
    </div>
    <p v-if="description" class="mt-1 text-xs leading-5 text-muted-foreground">{{ description }}</p>
    <div class="mt-2"><slot /></div>
    <p v-if="error" class="mt-1.5 text-xs leading-5 text-destructive">{{ error }}</p>
  </div>
</template>
