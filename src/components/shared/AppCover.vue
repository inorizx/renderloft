<template>
  <div class="grid place-items-center overflow-hidden border border-border bg-secondary">
    <img
      v-if="props.src && !hasError"
      :src="props.src"
      :alt="props.alt"
      class="size-full object-cover"
      @error="hasError = true"
    />
    <Boxes v-else :class="props.iconClass" class="text-muted-foreground" aria-hidden="true" />
  </div>
</template>

<script setup lang="ts">
import { ref, watch } from 'vue'
import { Boxes } from '@lucide/vue'

const props = withDefaults(
  defineProps<{
    src?: string
    alt: string
    iconClass?: string
  }>(),
  { iconClass: 'size-5' }
)

const hasError = ref(false)
watch(
  () => props.src,
  () => {
    hasError.value = false
  }
)
</script>
