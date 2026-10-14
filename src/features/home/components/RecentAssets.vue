<script setup lang="ts">
import { ArrowUpRight, Boxes, CircleAlert, LoaderCircle } from '@lucide/vue'
import AppCover from '@/components/shared/AppCover.vue'
import EmptyState from '@/components/shared/EmptyState.vue'
import SectionHeader from '@/components/shared/SectionHeader.vue'
import { Button, Card } from '@/components/ui'
import { stripHtml } from '@/lib/utils'

export interface RecentAsset {
  id: string
  name: string
  alias?: string
  cover?: string
  note?: string
  description?: string
  /** 配方驱动键（`driverKey`）。**退路**用：没有 `kind` 时按它判类型。 */
  driverKey?: string
  /** 配方种类（`kind`）。类型标签的首选判据，见 `shared/lib/recipeType`。 */
  kind?: string
}

defineProps<{
  eyebrow: string
  title: string
  allLabel: string
  emptyTitle: string
  emptyDescription: string
  addLabel: string
  assets: RecentAsset[]
  getLocation: (asset: RecentAsset) => Record<string, unknown>
  typeLabel: (asset: RecentAsset) => string
  connectionLabel: (asset: RecentAsset) => string
  isLoading: boolean
  loadError: string
  retryLabel: string
}>()
defineEmits<{ retry: [] }>()
</script>

<template>
  <section>
    <SectionHeader :eyebrow="eyebrow" :title="title">
      <template #actions
        ><RouterLink to="/library"
          ><Button variant="link" size="sm">{{ allLabel }}</Button></RouterLink
        ></template
      >
    </SectionHeader>
    <div
      v-if="isLoading"
      class="mt-4 grid min-h-40 place-items-center text-sm text-muted-foreground"
    >
      <LoaderCircle class="size-5 animate-spin" />
    </div>
    <EmptyState v-else-if="loadError" class="mt-4 min-h-0" :title="loadError" :description="''">
      <template #icon><CircleAlert class="size-5 text-destructive" /></template>
      <template #actions
        ><Button variant="outline" @click="$emit('retry')">{{ retryLabel }}</Button></template
      >
    </EmptyState>
    <div v-else-if="assets.length" class="mt-4 grid gap-3 md:grid-cols-2">
      <RouterLink v-for="asset in assets" :key="asset.id" :to="getLocation(asset)">
        <Card class="asset-card glass-lift h-full p-4">
          <div class="flex gap-3">
            <AppCover
              class="size-10 shrink-0"
              :src="asset.cover"
              :alt="asset.name"
              icon-class="size-4"
            />
            <div class="min-w-0 flex-1">
              <div class="flex justify-between gap-3">
                <p class="truncate font-semibold">{{ asset.alias || asset.name }}</p>
                <ArrowUpRight class="size-4 shrink-0 text-muted-foreground" />
              </div>
              <p class="mt-1 truncate text-xs text-muted-foreground">
                {{ connectionLabel(asset) }}
              </p>
              <p class="mt-3 line-clamp-2 text-sm leading-5 text-muted-foreground">
                {{ stripHtml(asset.note || asset.description || '') }}
              </p>
              <span class="asset-type mt-3">{{ typeLabel(asset) }}</span>
            </div>
          </div>
        </Card>
      </RouterLink>
    </div>
    <EmptyState v-else class="mt-4 min-h-0" :title="emptyTitle" :description="emptyDescription">
      <template #icon><Boxes class="size-5" /></template>
      <template #actions
        ><RouterLink to="/library"
          ><Button variant="outline">{{ addLabel }}</Button></RouterLink
        ></template
      >
    </EmptyState>
  </section>
</template>

<style scoped>
/* hover 的描边反馈统一由全局 .glass-lift 提供，这里不再各写一份。 */
.asset-type {
  display: inline-flex;
  border: 1px solid var(--glass-panel-edge);
  padding: 0.15rem 0.4rem;
  font-size: 0.6875rem;
  color: var(--app-muted-foreground);
}
</style>
