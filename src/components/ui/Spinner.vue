<script setup lang="ts">
import { cn } from '@/lib/utils'
import { useLocale } from '@/shared/composables/useLocale'

interface Props {
  size?: 'sm' | 'default' | 'lg'
  class?: string
}

const props = withDefaults(defineProps<Props>(), { size: 'default' })
const { t } = useLocale()
</script>

<template>
  <svg
    role="status"
    :aria-label="t('common.loading')"
    :class="
      cn(
        'animate-spin text-primary',
        props.size === 'sm' && 'h-4 w-4',
        props.size === 'default' && 'h-5 w-5',
        props.size === 'lg' && 'h-7 w-7',
        props.class
      )
    "
    viewBox="0 0 24 24"
    fill="none"
  >
    <circle class="opacity-25" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="3" />
    <path
      class="opacity-90"
      d="M21 12a9 9 0 0 0-9-9"
      stroke="currentColor"
      stroke-linecap="round"
      stroke-width="3"
    />
  </svg>
</template>
