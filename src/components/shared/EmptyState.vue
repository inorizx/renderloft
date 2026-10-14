<script setup lang="ts">
interface Props {
  title: string
  description?: string
}

defineProps<Props>()
</script>

<template>
  <section
    class="glass-panel-inset glass-enter flex min-h-64 flex-col items-center justify-center rounded-lg p-6 text-center"
    style="--glass-enter-delay: 60ms"
  >
    <div
      v-if="$slots.icon"
      class="glass-panel grid size-12 place-items-center rounded-md text-muted-foreground"
    >
      <slot name="icon" />
    </div>
    <h2 class="mt-4 text-lg font-semibold">{{ title }}</h2>
    <p v-if="description" class="mt-2 max-w-md text-sm leading-6 text-muted-foreground">
      {{ description }}
    </p>
    <div v-if="$slots.actions" class="mt-5 flex flex-wrap justify-center gap-2">
      <slot name="actions" />
    </div>
  </section>
</template>
