<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Boxes, Image, LoaderCircle, Workflow } from '@lucide/vue'
import PageContainer from '@/components/layout/PageContainer.vue'
import PageHeader from '@/components/PageHeader.vue'
import NoticePanel from '@/components/shared/NoticePanel.vue'
import { Button } from '@/components/ui'
import FirstRunGuide from '@/features/home/components/FirstRunGuide.vue'
import QuickStart from '@/features/home/components/QuickStart.vue'
import RecentAssets, { type RecentAsset } from '@/features/home/components/RecentAssets.vue'
import { useV2WorkspaceStore } from '@/features/v2/stores/workspace'
import { useSettingsStore } from '@/features/settings/stores'
import {
  isConnectionReady,
  useExecutionConnections,
  type ConnectionStatus
} from '@/shared/composables/useExecutionConnections'
import { useLocale } from '@/shared/composables/useLocale'
import { recipeTypeLabel } from '@/shared/lib/recipeType'

const workspace = useV2WorkspaceStore()
const settingsStore = useSettingsStore()
const hasInitialized = ref(false)
const { t } = useLocale()
const {
  connectionStatus,
  connections,
  isLoading: isLoadingConnections,
  loadError: connectionsLoadError,
  loadConnections
} = useExecutionConnections()
const isInitializing = computed(
  () => settingsStore.isLoading || isLoadingConnections.value || workspace.isLoading
)
const initializationError = computed(
  () => settingsStore.loadError || connectionsLoadError.value || workspace.error
)
const hasExecutionConnection = computed(() =>
  connections.value.some((connection) =>
    isConnectionReady(connection, connection.providerId !== 'comfyui')
  )
)
// 引导关闭只作用于当前浏览器会话：本地应用无跨工作区/actor 的稳定身份，
// 用 localStorage 的单一全局键会在共享浏览器或切换 daemon 会话时误压其他工作区的引导。
const GUIDE_SKIP_KEY = 'renderloft-first-run-guide-skipped'
const firstRunGuideSkipped = ref(
  typeof window !== 'undefined' && sessionStorage.getItem(GUIDE_SKIP_KEY) === '1'
)
const showFirstRunGuide = computed(
  () =>
    hasInitialized.value &&
    !isInitializing.value &&
    !initializationError.value &&
    !firstRunGuideSkipped.value &&
    (!workspace.recipes.length || !hasExecutionConnection.value)
)
const skipFirstRunGuide = () => {
  firstRunGuideSkipped.value = true
  if (typeof window !== 'undefined') sessionStorage.setItem(GUIDE_SKIP_KEY, '1')
}

const recentAssets = computed<RecentAsset[]>(() =>
  workspace.recipes.slice(0, 4).map((recipe) => ({
    id: String(recipe.recipeId),
    name: recipe.name,
    alias: recipe.name,
    description: recipe.description,
    note: recipe.description,
    driverKey: recipe.driverKey,
    kind: recipe.kind
  }))
)
const hasSuccessfulRun = computed(() =>
  workspace.operationSummaries.some(
    (operation) => operation.status === 'SUCCEEDED' || operation.status === 'PARTIAL'
  )
)
const runnerLocation = (asset: RecentAsset) => ({ path: `/library/${asset.id}` })
// 配方类型标签：唯一权威在 shared/lib/recipeType（资源库同用一份）。
// 原先这里写死了 `library.assetTypeRhWorkflow`、还丢掉了入参，
// 于是 ComfyUI / OpenAI 的配方在首页一律显示成「RH 工作流」。
const assetTypeLabel = (asset: RecentAsset) => recipeTypeLabel(asset)
const assetConnectionLabel = (asset: RecentAsset) => asset.name

const statusLabel = (status: ConnectionStatus) =>
  t(`connection.${status === 'not-configured' ? 'notConfigured' : status}`)
const quickStartItems = computed(() => [
  {
    id: 'runninghub',
    title: t('home.quickStartRunningHub'),
    description: t('home.quickStartRunningHubHint'),
    status: connectionStatus('runninghub'),
    statusLabel: statusLabel(connectionStatus('runninghub')),
    actionLabel:
      connectionStatus('runninghub') === 'connected'
        ? t('home.openLibrary')
        : t('connection.configure'),
    to: connectionStatus('runninghub') === 'connected' ? '/library' : '/connections',
    icon: Boxes
  },
  {
    id: 'comfyui',
    title: t('home.quickStartComfyui'),
    description: t('home.quickStartComfyuiHint'),
    status: connectionStatus('comfyui'),
    statusLabel: statusLabel(connectionStatus('comfyui')),
    actionLabel:
      connectionStatus('comfyui') === 'connected'
        ? t('home.openLibrary')
        : t('connection.configure'),
    to: connectionStatus('comfyui') === 'connected' ? '/library' : '/connections',
    icon: Workflow
  },
  {
    id: 'openai-image',
    title: t('home.quickStartOpenAiImage'),
    description: t('home.quickStartOpenAiImageHint'),
    status: connectionStatus('openai-image'),
    statusLabel: statusLabel(connectionStatus('openai-image')),
    actionLabel:
      connectionStatus('openai-image') === 'connected'
        ? t('home.openLibrary')
        : t('connection.configure'),
    to: connectionStatus('openai-image') === 'connected' ? '/library' : '/connections',
    icon: Image
  }
])
// 状态标签与色调的唯一权威在 shared/lib/taskStatus（任务中心同用一份）。
// 首页原先各写了一份：标签那份对 6 个**可达**值其实与权威完全相同，
// 而它多出的 ACCEPTED / SUBMISSION_UNCERTAIN 两项**根本不可达**——
const initialize = async (force = false) => {
  hasInitialized.value = false
  await Promise.allSettled([
    workspace.loadRecipes(force),
    workspace.loadOperations(50),
    settingsStore.loadSettings(force),
    loadConnections(force)
  ])
  hasInitialized.value = true
}

onMounted(() => initialize())
</script>

<template>
  <PageContainer>
    <PageHeader
      :eyebrow="t('home.eyebrow')"
      :title="t('home.title')"
      :description="t('home.description')"
    />
    <NoticePanel v-if="isInitializing" class="mt-6" variant="default">
      <LoaderCircle class="size-4 animate-spin" /> {{ t('home.loadingWorkspace') }}
    </NoticePanel>
    <NoticePanel v-else-if="initializationError" class="mt-6" variant="destructive">
      {{ t('home.loadWorkspaceFailed') }}
      <template #actions
        ><Button variant="outline" size="sm" @click="initialize(true)">{{
          t('common.retry')
        }}</Button></template
      >
    </NoticePanel>
    <FirstRunGuide
      v-if="showFirstRunGuide"
      :running-hub-status="connectionStatus('runninghub')"
      :execution-ready="hasExecutionConnection"
      :has-apps="workspace.recipes.length > 0"
      :has-tasks="hasSuccessfulRun"
      class="mt-6"
      @skip="skipFirstRunGuide"
    />
    <QuickStart
      v-else
      :eyebrow="t('home.quickStartEyebrow')"
      :title="t('home.quickStart')"
      :items="quickStartItems"
      class="mt-6"
    />
    <section class="workspace-grid mt-6">
      <RecentAssets
        :eyebrow="t('home.recentAppsEyebrow')"
        :title="t('home.recentAssets')"
        :all-label="t('home.allApps')"
        :empty-title="t('home.startWithApp')"
        :empty-description="t('home.startWithAppHint')"
        :add-label="t('home.addApp')"
        :assets="recentAssets"
        :get-location="runnerLocation"
        :type-label="assetTypeLabel"
        :connection-label="assetConnectionLabel"
        :is-loading="workspace.isLoading"
        :load-error="workspace.error"
        :retry-label="t('common.retry')"
        @retry="workspace.loadRecipes(true)"
      />
      <!--
        首页**不放**任务动态：`operation summary` 只拿得到 `{id, status}`，渲染出来每行都是
        「任务 / 未填写提示词」——没名字、没图、没信息。右侧栏那份「最近任务」有名字、有缩略图、
        有每轮状态，是同一件事的更好版本；同一份信息在一屏里出现两次、其中一次还是残的，
        不如只留好的那份。
        （真要在首页放，正路是让 operation summary 带上 `recipeName`。）
      -->
    </section>
  </PageContainer>
</template>

<style scoped>
.workspace-grid {
  display: grid;
  gap: 1.75rem;
}
/*
 * 断点要按**容器实际能有多少宽度**定，而容器宽度不是视口宽度的简单函数：
 * 右栏从 1200 起常驻、主区让出 320px（见 shared/config/layout.ts）⇒ 容器在 1200 处
 * 跳变 320px。两列的账：主列 ≥420 + 侧列 300 + 间距 28 ⇒ 容器 ≥748。
 *
 *   - 浮层档（1024–1199）：容器 = 视口 − 240（左导航）⇒ 视口 ≥ 988，取 1100（原值，本来就对）
 *   - 常驻档（≥1200）    ：容器 = 视口 − 560（左导航 + 右栏）⇒ 视口 ≥ 1308，取 1320
 *
 * ⚠️ 只写一个 1100 的话，1200 视口下容器只有 561px，主列被挤到 **232px**（实测）。
 * 视口断点表达不了这个跳变——真要彻底解决得用容器查询（`@container`），
 * 那需要给 `<main>` 加 containment，先不动。
 */
@media (min-width: 1100px) and (max-width: 1199px) {
  .workspace-grid {
    grid-template-columns: minmax(0, 1fr) 300px;
  }
}
@media (min-width: 1320px) {
  .workspace-grid {
    grid-template-columns: minmax(0, 1fr) 300px;
  }
}
</style>
