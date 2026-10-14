<script setup lang="ts">
import { computed } from 'vue'
import { ArrowRight, Check, LibraryBig, Link2, Play } from '@lucide/vue'
import { Button, Card } from '@/components/ui'
import type { ConnectionStatus } from '@/shared/composables/useExecutionConnections'
import { useLocale } from '@/shared/composables/useLocale'

interface Props {
  runningHubStatus: ConnectionStatus
  executionReady: boolean
  hasApps: boolean
  hasTasks: boolean
}

const props = defineProps<Props>()
const emit = defineEmits<{ skip: [] }>()
const { t } = useLocale()

const steps = computed(() => [
  {
    id: 'connection',
    title: t('home.setupConnectionTitle'),
    description: t('home.setupConnectionDescription'),
    done: props.executionReady,
    to: '/connections',
    action: props.executionReady ? t('home.setupViewConnection') : t('home.setupConfigure'),
    icon: Link2
  },
  {
    id: 'app',
    title: t('home.setupAppTitle'),
    description: t('home.setupAppDescription'),
    done: props.hasApps,
    // 无应用时：RunningHub 已连接才去导入；否则引导先去完成配置，避免进入死胡同导入弹窗。
    to: props.hasApps
      ? '/library'
      : props.runningHubStatus === 'connected'
        ? '/library?import=runninghub'
        : '/connections',
    action: props.hasApps
      ? t('home.setupViewApps')
      : props.runningHubStatus === 'connected'
        ? t('home.setupImport')
        : t('home.setupConfigure'),
    icon: LibraryBig
  },
  {
    id: 'run',
    title: t('home.setupRunTitle'),
    description: t('home.setupRunDescription'),
    done: props.hasTasks,
    to: props.hasTasks ? '/tasks' : '/library',
    action: props.hasTasks ? t('home.setupViewTasks') : t('home.setupOpenApp'),
    icon: Play
  }
])
</script>

<template>
  <Card class="border-primary/20 bg-primary/[0.035] p-5 sm:p-6">
    <div class="flex flex-col gap-5 lg:flex-row lg:items-start lg:justify-between">
      <div class="max-w-xl">
        <div class="flex items-center gap-2">
          <span class="rounded-full bg-primary/10 px-2 py-1 text-[11px] font-semibold text-primary">
            {{ t('home.setupEyebrow') }}
          </span>
          <span class="text-xs text-muted-foreground">{{ t('home.setupHint') }}</span>
        </div>
        <h2 class="mt-3 text-lg font-semibold tracking-tight">{{ t('home.setupTitle') }}</h2>
        <p class="mt-1.5 text-sm leading-6 text-muted-foreground">
          {{ t('home.setupDescription') }}
        </p>
      </div>
      <Button variant="outline" size="sm" class="shrink-0" @click="emit('skip')">
        {{ t('home.setupSkip') }}
        <ArrowRight class="size-3.5" />
      </Button>
    </div>

    <!--
      断点用 `xl`（1280）而不是 `lg`（1024）：任务中心的右栏从 1200 起**常驻**，
      主区因此让出 320px（见 shared/config/layout.ts）——容器宽度不再是视口宽度的
      简单函数，在 1200 处会跳变 320px。用视口断点写 `lg:grid-cols-3` 的话，
      1200 视口下容器只有 640px 却硬撑三列，每张卡片只剩 ~190px、文字挤成窄条。
      `xl` 下 1200 视口回落到两列（各 ~310px），与改动前 960px 宽三列（各 ~300px）观感一致。
      （更彻底的做法是容器查询 `@container`，那要动 `<main>` 的 containment，先不动。）
    -->
    <ol class="mt-6 grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
      <li
        v-for="(step, index) in steps"
        :key="step.id"
        class="setup-step flex min-w-0 gap-3 rounded-lg border border-border/70 bg-card/80 p-4"
        :class="step.done && 'is-done'"
      >
        <div class="setup-step-number">
          <Check v-if="step.done" class="size-4" />
          <span v-else>{{ index + 1 }}</span>
        </div>
        <div class="min-w-0 flex-1">
          <div class="flex items-start justify-between gap-2">
            <h3 class="text-sm font-semibold">{{ step.title }}</h3>
            <component :is="step.icon" class="size-4 shrink-0 text-muted-foreground" />
          </div>
          <p class="mt-1 text-xs leading-5 text-muted-foreground">{{ step.description }}</p>
          <RouterLink
            :to="step.to"
            class="mt-3 inline-flex items-center gap-1.5 text-xs font-medium text-primary underline-offset-4 transition-colors hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
          >
            {{ step.action }}
            <ArrowRight class="size-3" />
          </RouterLink>
        </div>
      </li>
    </ol>
  </Card>
</template>

<style scoped>
.setup-step-number {
  display: grid;
  width: 1.75rem;
  height: 1.75rem;
  flex: none;
  place-items: center;
  border: 1px solid var(--app-border);
  border-radius: 999px;
  color: var(--app-muted-foreground);
  font-size: 0.75rem;
  font-weight: 600;
}
.setup-step.is-done .setup-step-number {
  border-color: color-mix(in srgb, var(--app-success) 50%, var(--app-border));
  background: color-mix(in srgb, var(--app-success) 12%, transparent);
  color: var(--app-success);
}
</style>
