<script setup lang="ts">
import { cn } from '@/lib/utils'

interface Props {
  modelValue?: string
  id?: string
  class?: string
}

const props = defineProps<Props>()
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

const handleChange = (event: Event) => {
  emit('update:modelValue', (event.target as HTMLSelectElement).value)
}
</script>

<template>
  <select
    :id="id"
    :value="modelValue"
    :class="
      cn(
        'flex h-9 w-full rounded-md border border-input bg-secondary px-3 text-sm shadow-xs outline-none transition-[border-color,box-shadow] duration-150 focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/20 disabled:cursor-not-allowed disabled:opacity-50',
        props.class
      )
    "
    @change="handleChange"
  >
    <slot />
  </select>
</template>
