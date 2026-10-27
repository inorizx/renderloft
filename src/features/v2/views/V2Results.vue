<template>
  <PageContainer>
    <PageHeader
      :eyebrow="t('results.eyebrow')"
      :title="t('results.title')"
      :description="t('results.description')"
    >
      <template #actions>
        <Button v-if="selecting" variant="ghost" size="sm" @click="cancelSelection">{{
          t('results.cancelSelect')
        }}</Button>
        <Button
          v-else
          variant="outline"
          size="sm"
          :disabled="!items.length"
          @click="selecting = true"
          >{{ t('results.select') }}</Button
        >
        <Button variant="outline" size="sm" :disabled="loading" @click="reload">{{
          t('results.reload')
        }}</Button>
      </template>
    </PageHeader>

    <p v-if="error" class="mb-3 text-sm text-destructive">{{ error }}</p>

    <EmptyState
      v-if="!items.length && !loading"
      :title="t('results.emptyTitle')"
      :description="t('results.emptyDescription')"
    >
      <template #icon><Images class="size-5" /></template>
    </EmptyState>

    <!--
      「挑」这一步此前只能靠人肉：20 张里要 5 张，就得下载 20 次、再删 15 个文件。
      选择模式把"挑选"变成一次点击；导出沿用逐张保存（文件名规则不变）。
    -->
    <div
      v-if="selecting"
      class="mb-3 flex flex-wrap items-center gap-2 rounded-lg border border-border bg-secondary/50 px-3 py-2 text-sm"
    >
      <span class="font-medium">{{
        t('results.selectedCount', { selected: selected.size, total: items.length })
      }}</span>
      <Button variant="ghost" size="sm" @click="selectAll">{{ t('results.selectAll') }}</Button>
      <Button variant="ghost" size="sm" :disabled="!selected.size" @click="clearSelection">{{
        t('results.clearSelect')
      }}</Button>
      <span class="flex-1"></span>
      <Button size="sm" :disabled="!selected.size || exporting" @click="exportSelected">{{
        exporting ? t('results.exporting') : t('results.exportSelected')
      }}</Button>
    </div>

    <div v-if="items.length" class="results-grid" aria-live="polite">
      <button
        v-for="(item, index) in items"
        :key="item.artifactId"
        type="button"
        class="result-card"
        :class="{ 'result-card-selected': selecting && selected.has(item.artifactId) }"
        :aria-label="item.label"
        @click="selecting ? toggleSelect(item.artifactId) : openPreview(index)"
      >
        <span v-if="selecting" class="result-check" aria-hidden="true">
          <Check v-if="selected.has(item.artifactId)" class="size-3.5" />
        </span>
        <span class="result-thumb">
          <img
            v-if="item.kind === 'image' && item.url"
            :src="item.url"
            :alt="item.label"
            loading="lazy"
          />
          <video v-else-if="item.kind === 'video' && item.url" :src="item.url" muted playsinline />
          <span v-else class="result-thumb-placeholder"
            ><LoaderCircle v-if="!item.url" class="size-4 animate-spin" /><FileQuestion
              v-else
              class="size-4"
          /></span>
        </span>
        <span class="result-meta">
          <span class="result-name">{{ item.filename }}</span>
          <!--
            来源只在**已知**时显示：产物列表本身不带"哪个应用"（那要按 run 反查），所以这里只用
            工作区已经缓存过的任务来解析。解析不到就**什么都不说**，不编一个来源。
          -->
          <span v-if="item.source" class="result-source">{{ item.source }}</span>
          <span class="result-size">{{ humanSize(item.byteSize) }}</span>
        </span>
      </button>
    </div>

    <div v-if="hasMore" class="mt-5 flex justify-center">
      <Button variant="outline" :disabled="loading" @click="loadMore">
        {{ loading ? t('results.loading') : t('results.loadMore') }}
      </Button>
    </div>

    <ResultPreviewDialog
      v-if="previewIndex !== null"
      :open="true"
      :title="previewTitle"
      :results="previewable"
      :gallery="gallery"
      :initial-index="previewIndex"
      :downloading-index="downloadingIndex"
      @update:open="!$event && (previewIndex = null)"
      @update:index="(index) => (previewIndex = index)"
      @download="(result) => download(result)"
    />
  </PageContainer>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { Check, FileQuestion, Images, LoaderCircle } from '@lucide/vue'
import PageContainer from '@/components/layout/PageContainer.vue'
import PageHeader from '@/components/PageHeader.vue'
import Button from '@/components/ui/Button.vue'
import EmptyState from '@/components/shared/EmptyState.vue'
import ResultPreviewDialog from '@/components/shared/ResultPreviewDialog.vue'
import { v2, type OperationDetail } from '@/shared/api/v2'
import { useLocale } from '@/shared/composables/useLocale'
import { useV2WorkspaceStore } from '@/features/v2/stores/workspace'
import { useAppStore } from '@/shared/stores/app'
import { runGroupLabel } from '@/shared/lib/runLabel'
import type { TaskResult } from '@/shared/types/runninghub'

const { t } = useLocale()
const workspace = useV2WorkspaceStore()
const appStore = useAppStore()

interface ResultItem {
  artifactId: number
  runId: number
  filename: string
  mimeType: string
  byteSize: number
  kind: 'image' | 'video' | 'other'
  url?: string
  label: string
  /** 已知来源（`应用名 · 那条 run 的标签`）；解析不到就是空串，**不编**。 */
  source: string
  result: TaskResult
}

/** 一页多少条。产物可能很多，所以是"加载更多"而不是一次拉完。 */
const PAGE_SIZE = 60

const items = ref<ResultItem[]>([])
const loading = ref(false)
const hasMore = ref(false)
const error = ref('')

/**
 * 选择模式：点卡片不是预览，而是勾选。
 *
 * 为什么需要它：20 张里只要 5 张时，旧路径是"全下 20 个再删 15 个"。
 * 这里只做"挑 + 导出选中"，命名规则沿用后端（`saveArtifact` 自己定名），
 * 不再前端另起一套命名——两套命名迟早对不上。
 */
const selecting = ref(false)
const selected = ref(new Set<number>())
const exporting = ref(false)

const toggleSelect = (artifactId: number) => {
  if (selected.value.has(artifactId)) selected.value.delete(artifactId)
  else selected.value.add(artifactId)
}

const selectAll = () => {
  for (const item of items.value) selected.value.add(item.artifactId)
}

const clearSelection = () => selected.value.clear()

const cancelSelection = () => {
  selecting.value = false
  selected.value.clear()
}

async function exportSelected() {
  if (!selected.value.size || exporting.value) return
  exporting.value = true
  const ids = [...selected.value]
  let done = 0
  let directory = ''
  let firstError = ''
  for (const artifactId of ids) {
    try {
      const saved = await v2.saveArtifact(artifactId)
      done += 1
      if (!directory) directory = saved.directory
    } catch (error) {
      if (!firstError) firstError = error instanceof Error ? error.message : String(error)
    }
  }
  exporting.value = false
  if (firstError) {
    appStore.showNotification(
      'error',
      t('results.exportPartial', { done, total: ids.length, message: firstError })
    )
    return
  }
  appStore.showNotification('success', t('results.exported', { count: done, directory }))
  cancelSelection()
}
const previewIndex = ref<number | null>(null)
const downloadingIndex = ref(-1)

const kindOf = (mimeType: string, filename: string): ResultItem['kind'] => {
  if (mimeType.startsWith('image/')) return 'image'
  if (mimeType.startsWith('video/')) return 'video'
  if (/\.(png|jpe?g|webp|gif|avif)$/i.test(filename)) return 'image'
  if (/\.(mp4|webm|mov|m4v)$/i.test(filename)) return 'video'
  return 'other'
}

const humanSize = (bytes: number) => {
  if (!bytes) return t('results.sizeUnknown')
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`
}

/**
 * 这条产物来自哪个应用/哪条输入 —— **只在已经知道时**给。
 *
 * 产物列表本身不带"哪个应用"（那要按 run 反查），所以这里只查工作区**已经缓存过**的任务详情，
 * 不为每一张产物再发一次请求（一页 60 张就是 60 次，而用户多数时候只是要看图）。
 * 查不到就返回空串：界面**什么都不显示**，而不是编一个来源。
 */
const sourceOf = (runId: number): string => {
  const operation: OperationDetail | undefined = workspace.operations.find((candidate) =>
    candidate.runs.some((run) => run.runId === runId)
  )
  if (!operation) return ''
  const run = operation.runs.find((candidate) => candidate.runId === runId)
  const app = operation.recipeName ?? ''
  if (!run) return app
  return app ? `${app} · ${runGroupLabel(run)}` : runGroupLabel(run)
}

/** 把一页产物变成可显示的东西：缩略图按需取字节，URL 用完要回收。 */
const absorb = async (
  page: {
    artifactId: number
    runId: number
    filename: string
    mimeType: string
    byteSize: number
  }[]
) => {
  const next: ResultItem[] = page.map((artifact) => {
    const kind = kindOf(artifact.mimeType, artifact.filename)
    const source = sourceOf(artifact.runId)
    return {
      artifactId: artifact.artifactId,
      runId: artifact.runId,
      filename: artifact.filename,
      mimeType: artifact.mimeType,
      byteSize: artifact.byteSize,
      kind,
      label: source ? `${source} · ${artifact.filename}` : artifact.filename,
      source,
      result: {
        url: null,
        outputType: artifact.mimeType,
        nodeId: String(artifact.artifactId)
      }
    }
  })
  items.value = [...items.value, ...next]
  // 缩略图逐张补：失败的那张就留占位符（**不静默消失**），其余照常显示。
  for (const item of next) {
    if (item.kind === 'other') continue
    try {
      const { blob } = await v2.fetchArtifactBytes(item.artifactId)
      const url = URL.createObjectURL(blob)
      item.url = url
      item.result = { ...item.result, url }
      // 数组里的对象是同一批引用，但 Vue 的响应式要靠一次触发（缩略图 src 才会更新）。
      items.value = [...items.value]
    } catch {
      item.url = undefined
    }
  }
}

const load = async (before?: number) => {
  if (loading.value) return
  loading.value = true
  error.value = ''
  try {
    const page = await v2.listArtifactsNewest(PAGE_SIZE, before)
    hasMore.value = page.hasMore ?? false
    await absorb(page.items)
  } catch (caught) {
    error.value = caught instanceof Error ? caught.message : String(caught)
  } finally {
    loading.value = false
  }
}

const loadMore = () => load(items.value[items.value.length - 1]?.artifactId)

/**
 * 重新加载：**先把这一页的 object URL 回收**再重来。
 *
 * 不回收的话，每点一次"重新加载"就多留一批原图在内存里（这个页面不会卸载）。
 */
const releaseUrls = () => {
  for (const item of items.value) {
    if (item.url) URL.revokeObjectURL(item.url)
  }
}

const reload = async () => {
  releaseUrls()
  items.value = []
  hasMore.value = false
  await load()
}

const gallery = computed(() =>
  items.value
    .filter((item) => Boolean(item.result.url))
    .map((item) => ({ result: item.result, label: item.label }))
)

/** 预览框要的那一份（与 `gallery` 同源，避免"数出来 N 项、点开只有 M 项"）。 */
const previewable = computed(() => gallery.value.map((item) => item.result))

const previewTitle = computed(() => {
  const item = previewIndex.value === null ? undefined : items.value[previewIndex.value]
  return item?.label ?? ''
})

const openPreview = (index: number) => {
  previewIndex.value = index
}

const download = async (result: TaskResult) => {
  const artifactId = Number(result.nodeId)
  if (!Number.isInteger(artifactId)) return
  downloadingIndex.value = items.value.findIndex((item) => item.artifactId === artifactId)
  try {
    const { filename, path } = await v2.saveArtifact(artifactId)
    appStore.showNotification('success', t('results.downloaded', { filename, path }))
  } catch (caught) {
    appStore.showNotification(
      'error',
      t('results.downloadFailed', {
        message: caught instanceof Error ? caught.message : String(caught)
      })
    )
  } finally {
    downloadingIndex.value = -1
  }
}

onMounted(async () => {
  // 任务列表先加载：来源标签只从**已缓存**的任务里解析，不额外发请求。
  void workspace.loadOperations(100).catch(() => undefined)
  await load()
})

onBeforeUnmount(releaseUrls)
</script>

<style scoped>
.results-grid {
  display: grid;
  gap: 0.75rem;
  grid-template-columns: repeat(auto-fill, minmax(9.5rem, 1fr));
}
.result-card {
  position: relative;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  border: 1px solid var(--border);
  border-radius: 0.6rem;
  background: var(--secondary);
  text-align: left;
  transition: border-color 0.15s ease;
}
.result-card:hover {
  border-color: color-mix(in srgb, var(--primary) 40%, var(--border));
}
.result-card-selected {
  border-color: var(--primary);
  box-shadow: 0 0 0 2px color-mix(in srgb, var(--primary) 30%, transparent);
}
.result-check {
  position: absolute;
  top: 0.4rem;
  right: 0.4rem;
  z-index: 1;
  display: grid;
  place-items: center;
  width: 1.15rem;
  height: 1.15rem;
  border: 1px solid var(--border);
  border-radius: 999px;
  background: color-mix(in srgb, var(--background) 85%, transparent);
  color: transparent;
}
.result-card-selected .result-check {
  border-color: var(--primary);
  background: var(--primary);
  color: var(--primary-foreground);
}
.result-thumb {
  display: grid;
  place-items: center;
  aspect-ratio: 1 / 1;
  background: var(--muted);
}
.result-thumb img,
.result-thumb video {
  width: 100%;
  height: 100%;
  object-fit: cover;
}
.result-thumb-placeholder {
  color: var(--muted-foreground);
}
.result-meta {
  display: flex;
  flex-direction: column;
  gap: 0.15rem;
  padding: 0.5rem 0.6rem 0.6rem;
}
.result-name {
  font-size: 0.75rem;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.result-source {
  font-size: 0.6875rem;
  color: var(--muted-foreground);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.result-size {
  font-size: 0.6875rem;
  color: var(--muted-foreground);
}
</style>
