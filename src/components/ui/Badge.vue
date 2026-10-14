<script setup lang="ts">
import { cn } from '@/lib/utils'

interface Props {
  variant?: 'default' | 'secondary' | 'outline'
  class?: string
}

const props = withDefaults(defineProps<Props>(), { variant: 'default' })
</script>

<template>
  <span
    :class="
      cn(
        'inline-flex items-center rounded-sm px-2 py-0.5 text-xs font-medium leading-tight',
        props.variant === 'default' && 'bg-primary text-primary-foreground',
        props.variant === 'secondary' && 'bg-secondary text-secondary-foreground',
        props.variant === 'outline' && 'border border-border text-foreground',
        props.class
      )
    "
  >
    <slot />
  </span>
</template>
