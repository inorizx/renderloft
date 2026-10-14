<script setup lang="ts">
import { cn } from '@/lib/utils'

interface Props {
  label: string
  variant?: 'default' | 'secondary' | 'outline' | 'ghost'
  size?: 'sm' | 'default' | 'lg'
  class?: string
}

const props = withDefaults(defineProps<Props>(), { variant: 'ghost', size: 'default' })
</script>

<template>
  <button
    type="button"
    :aria-label="label"
    :class="
      cn(
        'inline-flex items-center justify-center rounded-md transition-[background-color,border-color,color,box-shadow,transform] duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50 active:translate-y-px',
        props.variant === 'default' && 'bg-primary text-primary-foreground hover:bg-primary/90',
        props.variant === 'secondary' &&
          'bg-secondary text-secondary-foreground hover:bg-secondary/80',
        props.variant === 'outline' &&
          'border border-input bg-background hover:bg-accent hover:text-accent-foreground',
        props.variant === 'ghost' &&
          'text-muted-foreground hover:bg-accent hover:text-accent-foreground',
        props.size === 'sm' && 'h-8 w-8',
        props.size === 'default' && 'h-9 w-9',
        props.size === 'lg' && 'h-10 w-10',
        props.class
      )
    "
  >
    <slot />
  </button>
</template>
