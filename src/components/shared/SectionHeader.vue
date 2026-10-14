<script setup lang="ts">
interface Props {
  eyebrow?: string
  title: string
  description?: string
}

defineProps<Props>()
</script>

<template>
  <header class="flex flex-col justify-between gap-4 sm:flex-row sm:items-end">
    <div class="min-w-0">
      <p v-if="eyebrow" class="glass-eyebrow">{{ eyebrow }}</p>
      <h2 class="section-title mt-1">{{ title }}</h2>
      <p v-if="description" class="mt-1 max-w-2xl text-sm leading-5 text-muted-foreground">
        {{ description }}
      </p>
    </div>
    <div v-if="$slots.actions" class="flex shrink-0 flex-wrap gap-2">
      <slot name="actions" />
    </div>
  </header>
</template>

<style scoped>
.section-title {
  font-family: var(--font-display);
  font-size: 1.125rem;
  font-weight: 600;
  letter-spacing: 0.01em;
}
</style>
