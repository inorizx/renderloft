<script setup lang="ts">
import type { Component } from 'vue'
import { ArrowRight, Settings2 } from '@lucide/vue'
import SectionHeader from '@/components/shared/SectionHeader.vue'
import { Button, Card } from '@/components/ui'
import type { ConnectionStatus } from '@/shared/composables/useExecutionConnections'

defineProps<{
  eyebrow: string
  title: string
  items: Array<{
    id: string
    title: string
    description: string
    status: ConnectionStatus
    statusLabel: string
    actionLabel: string
    to: string
    icon: Component
  }>
}>()
</script>

<template>
  <section>
    <SectionHeader :eyebrow="eyebrow" :title="title" />
    <div class="quick-start-grid mt-4">
      <Card v-for="item in items" :key="item.id" class="quick-start-card glass-lift p-4">
        <component :is="item.icon" class="size-5 text-primary" />
        <div class="mt-4 flex min-w-0 items-start justify-between gap-3">
          <div class="min-w-0">
            <h2 class="truncate font-semibold">{{ item.title }}</h2>
            <p class="mt-1 text-sm leading-5 text-muted-foreground">{{ item.description }}</p>
          </div>
          <span class="connection-status shrink-0" :class="`is-${item.status}`">{{
            item.statusLabel
          }}</span>
        </div>
        <RouterLink :to="item.to" class="mt-4 inline-flex">
          <Button size="sm" :variant="item.status === 'connected' ? 'outline' : 'default'">
            <Settings2 v-if="item.status !== 'connected'" class="size-3.5" />
            <ArrowRight v-else class="size-3.5" />
            {{ item.actionLabel }}
          </Button>
        </RouterLink>
      </Card>
    </div>
  </section>
</template>

<style scoped>
.quick-start-grid {
  display: grid;
  gap: 0.75rem;
}
.quick-start-card {
  min-width: 0;
}
/*
 * 卡片图标：淡色方块 + 主色字形。用 content-box 让 padding 把图标"撑大"，
 * 而不是把 1.25rem 的字形压小 —— 这是图标块与图标本身最容易搞错的一处。
 * 不再发光：图标不需要当灯（见 style.css 的光学预算）。
 */
.quick-start-card > svg {
  box-sizing: content-box;
  width: 1.125rem;
  height: 1.125rem;
  padding: 0.55rem;
  border-radius: var(--radius-md);
  background: color-mix(in srgb, var(--glass-accent) 14%, transparent);
  color: var(--glass-accent);
}
.connection-status {
  border: 1px solid var(--glass-panel-edge);
  padding: 0.2rem 0.45rem;
  font-size: 0.6875rem;
  line-height: 1rem;
  color: var(--app-muted-foreground);
}
.connection-status.is-connected {
  border-color: color-mix(in srgb, var(--app-success) 52%, var(--app-border));
  color: var(--app-success);
}
.connection-status.is-not-configured {
  border-color: color-mix(in srgb, var(--app-warning) 52%, var(--app-border));
  color: var(--app-warning);
}
.connection-status.is-unknown {
  border-color: color-mix(in srgb, var(--app-muted-foreground) 55%, var(--app-border));
}
/*
 * 列数断点按**容器实际能有多少宽度**定，不能只看视口：
 * 右栏从 1200 起常驻、主区让出 320px（见 shared/config/layout.ts），容器宽度在 1200
 * 处会跳变 320px。原先 768 就上三列 ⇒ 1200 视口下容器只有 561px，每张卡只剩 178px、
 * 文字挤成窄条。现在 768–1279 两列（各 264–520px），≥1280 三列（各 ≥240px）。
 */
@media (min-width: 768px) {
  .quick-start-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
@media (min-width: 1280px) {
  .quick-start-grid {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}
</style>
