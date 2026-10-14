<script setup lang="ts">
import { cn } from '@/lib/utils'

interface Props {
  modelValue?: string
  placeholder?: string
  rows?: number
  class?: string
}

const props = defineProps<Props>()
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

const handleInput = (event: Event) => {
  emit('update:modelValue', (event.target as HTMLTextAreaElement).value)
}
</script>

<template>
  <textarea
    :value="modelValue"
    :placeholder="placeholder"
    :rows="rows || 3"
    :class="
      cn(
        'flex min-h-20 w-full resize-none rounded-md border border-input bg-secondary px-3 py-2 text-sm shadow-xs outline-none transition-[border-color,box-shadow] duration-150 placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/20 disabled:cursor-not-allowed disabled:opacity-50',
        props.class
      )
    "
    @input="handleInput"
  />
</template>
