<template>
  <PageContainer>
    <PageHeader
      :eyebrow="t('history.eyebrow')"
      :title="t('history.title')"
      :description="t('history.description')"
    >
      <template #actions>
        <Button variant="outline" size="sm" :disabled="clearing" @click="clearFinished"
          ><Trash2 class="size-3.5" />{{
            clearing ? t('task.clearing') : t('task.clearFinished')
          }}</Button
        >
        <Button variant="outline" size="sm" @click="workspace.loadOperations(100)">{{
          t('task.reload')
        }}</Button>
      </template>
    </PageHeader>

    <div class="history-layout">
      <section class="stats-strip" :aria-label="t('history.stats')">
        <Button
          v-for="filter in filters"
          :key="filter.value"
          variant="ghost"
          type="button"
          class="stat-item"
          :class="statusFilter === filter.value ? 'stat-active' : ''"
          @click="statusFilter = filter.value"
          ><span>{{ filter.label }}</span
          ><strong>{{ filter.count }}</strong></Button
        >
      </section>

      <section class="archive-toolbar">
        <div class="relative w-full sm:w-[min(100%,32rem)]">
          <Search
            class="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
          /><Input
            v-model="keyword"
            type="search"
            class="pl-9"
            :placeholder="t('history.search')"
          />
        </div>
        <StatusIndicator
          status="success"
          :label="t('history.archivedCount', { count: filteredTasks.length })"
        />
      </section>

      <p v-if="downloadError" class="mt-3 text-xs text-destructive">{{ downloadError }}</p>

      <section v-if="filteredTasks.length" class="mt-6">
        <SectionHeader
          :title="t('history.results')"
          :description="t('history.sorted')"
          class="mb-4"
        />
        <div class="archive-grid">
          <Card
            v-for="task in filteredTasks"
            :key="task.operationId"
            class="archive-card overflow-hidden shadow-none"
          >
            <button
              class="preview-area"
              :class="artifactLoading[task.operationId] && 'is-loading'"
              type="button"
              :disabled="!previewResults(task).length"
              @click="openPreview(task)"
            >
              <img
                v-if="galleryPreview(task)?.kind === 'image'"
                :src="galleryPreview(task)!.url"
                :alt="operationLabel(task)"
                class="size-full object-cover"
              />
              <video
                v-else-if="galleryPreview(task)?.kind === 'video'"
                :src="galleryPreview(task)!.url"
                class="size-full object-cover"
                muted
                playsinline
                preload="metadata"
                @mouseenter="playVideoPreview"
                @mouseleave="resetVideoPreview"
                @focus="playVideoPreview"
                @blur="resetVideoPreview"
              />
              <span
                v-if="galleryPreview(task)?.kind === 'video'"
                class="archive-video-play"
                aria-hidden="true"
                ><Play class="size-5"
              /></span>
              <div v-else class="grid size-full place-items-center">
                <LoaderCircle
                  v-if="isActiveStatus(task.status) || artifactLoading[task.operationId]"
                  class="size-6 animate-spin text-primary"
                /><CircleAlert
                  v-else-if="isGalleryFailure(task.status)"
                  class="size-6 text-destructive"
                /><FileText v-else class="size-6 text-muted-foreground" />
              </div>
              <StatusBadge class="absolute left-2.5 top-2.5" :status="statusTone(task.status)">{{
                statusLabel(task.status)
              }}</StatusBadge
              ><StatusBadge
                v-if="galleryResultCount(task)"
                class="absolute bottom-2.5 right-2.5"
                status="neutral"
                >{{ t('history.resultCount', { count: galleryResultCount(task) }) }}</StatusBadge
              >
            </button>
            <div class="p-3.5">
              <p class="truncate text-sm font-semibold">{{ operationLabel(task) }}</p>
              <p class="mt-1 truncate text-xs text-muted-foreground">
                #{{ task.operationId }} · {{ task.runs.length }} {{ t('task.runs') }}
              </p>
              <!--
                「它其实没在动」：排队 / 退避重试 / 等确认这几种，状态**都显示「进行中」**，
                所以这里必须自己说一句——否则用户看到的就是一个永远转圈的任务
                （实测吃过一次 40 分钟误报：对外报"预计 10 分钟出片"）。
              -->
              <p
                v-if="task.attention"
                class="mt-1 flex items-center gap-1.5 text-xs text-amber-600 dark:text-amber-500"
              >
                <TriangleAlert class="size-3.5 shrink-0" aria-hidden="true" />
                <span class="truncate">{{ attentionLine(task.attention) }}</span>
              </p>
              <div class="mt-4 flex items-center justify-between">
                <time class="text-xs text-muted-foreground">{{ formatDate(task.createdAt) }}</time>
                <!--
                  耗时与花费：**平台报什么就显示什么**（RunningHub 按时长计费，两个数在同一行）。
                  没有就不显示——本地 ComfyUI 没有计费概念，凑一个「0 币」是编造。
                -->
                <span
                  v-if="formatUsage(task.runs)"
                  class="text-xs tabular-nums text-muted-foreground"
                  >{{ formatUsage(task.runs) }}</span
                >
                <div class="flex items-center gap-1">
                  <IconButton
                    v-if="previewResults(task).length"
                    :label="t('history.download')"
                    size="sm"
                    :disabled="downloadingId === task.operationId"
                    @click="downloadTask(task)"
                  >
                    <LoaderCircle
                      v-if="downloadingId === task.operationId"
                      class="size-3.5 animate-spin"
                    /><Download v-else class="size-3.5" />
                  </IconButton>
                  <IconButton
                    :label="t('history.results')"
                    size="sm"
                    :disabled="!previewResults(task).length"
                    @click="openPreview(task)"
                    ><Eye class="size-3.5"
                  /></IconButton>
                  <IconButton
                    v-if="isGalleryFailure(task.status)"
                    :label="t('task.rerun')"
                    size="sm"
                    :disabled="retryingId === task.operationId"
                    @click="retry(task)"
                    ><LoaderCircle
                      v-if="retryingId === task.operationId"
                      class="size-3.5 animate-spin" /><RotateCcw v-else class="size-3.5"
                  /></IconButton>
                  <IconButton
                    :label="t('task.deleteHistory')"
                    size="sm"
                    :disabled="removingId === task.operationId"
                    @click="remove(task)"
                    ><Trash2 class="size-3.5"
                  /></IconButton>
                </div>
              </div>
            </div>
          </Card>
        </div>
      </section>

      <EmptyState
        v-else
        :title="hasTasks ? t('history.noMatches') : t('history.empty')"
        :description="hasTasks ? t('history.noMatchesHint') : t('history.emptyHint')"
        class="mt-6 sm:min-h-[380px]"
        ><template #icon><ArchiveX class="size-6" /></template
        ><template v-if="!hasTasks" #actions
          ><RouterLink to="/library"
            ><Button>{{ t('library.goToLibrary') }}</Button></RouterLink
          ></template
        ></EmptyState
      >
    </div>

    <ResultPreviewDialog
      v-if="previewTask"
      :open="true"
      :title="previewGallery[previewIndex]?.label ?? operationLabel(previewTask)"
      :results="previewResults(previewTask)"
      :gallery="previewGallery"
      :initial-index="previewIndex"
      :downloading-index="previewDownloadingIndex"
      @update:open="!$event && (previewTask = undefined)"
      @update:index="(index) => (previewIndex = index)"
      @download="(result, index) => downloadPreviewResult(result, index)"
    />
  </PageContainer>
</template>
<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import {
  ArchiveX,
  CircleAlert,
  Download,
  Eye,
  FileText,
  LoaderCircle,
  Play,
  RotateCcw,
  Search,
  Trash2,
  TriangleAlert
} from '@lucide/vue'
import PageContainer from '@/components/layout/PageContainer.vue'
import PageHeader from '@/components/PageHeader.vue'
import SectionHeader from '@/components/shared/SectionHeader.vue'
import StatusIndicator from '@/components/shared/StatusIndicator.vue'
import EmptyState from '@/components/shared/EmptyState.vue'
import ResultPreviewDialog from '@/components/shared/ResultPreviewDialog.vue'
import { Button, Card, IconButton, Input, StatusBadge } from '@/components/ui'
import { isTerminalStatus, v2, type OperationDetail, type OperationSummary } from '@/shared/api/v2'
import { useV2WorkspaceStore } from '@/features/v2/stores/workspace'
import { useAppStore } from '@/shared/stores/app'
import { useLocale } from '@/shared/composables/useLocale'
import { taskStatusLabel, taskStatusTone } from '@/shared/lib/taskStatus'
import { attentionReasonLabel } from '@/shared/lib/attention'
import { formatUsage } from '@/shared/lib/runUsage'
import { runGroupLabel } from '@/shared/lib/runLabel'
import type { TaskResult } from '@/shared/types/runninghub'

const workspace = useV2WorkspaceStore()
const appStore = useAppStore()
const { t, dateLocale } = useLocale()

/** 画廊单屏最多展示的任务数；也是产物预取的并行上限来源。 */
const MAX_TASKS = 20

const keyword = ref('')
type TaskStatusFilter = 'all' | 'active' | 'SUCCEEDED' | 'FAILED' | 'CANCELLED'
const filterStorageKey = 'renderloft-tasks-status-filter'
const savedFilter = localStorage.getItem(filterStorageKey)
const statusFilter = ref<TaskStatusFilter>(
  savedFilter === 'active' ||
    savedFilter === 'SUCCEEDED' ||
    savedFilter === 'FAILED' ||
    savedFilter === 'CANCELLED'
    ? savedFilter
    : 'all'
)
watch(statusFilter, (value) => localStorage.setItem(filterStorageKey, value))

const downloadingId = ref<number>()
const downloadError = ref('')
const previewTask = ref<OperationDetail>()
const previewDownloadingIndex = ref(-1)

// operation 详情缓存：summaries 只带状态/数量，产物预览需要 runIds。
const details = ref<Record<number, OperationDetail>>({})
const detailPromises = new Map<number, Promise<OperationDetail | undefined>>()
const artifactItems = ref<
  Record<
    number,
    Array<{
      artifactId: number
      runId: number
      filename: string
      mimeType: string
      byteSize: number
      checksum: string
    }>
  >
>({})
const artifactUrls = ref<Record<number, string>>({})
const artifactLoading = ref<Record<number, boolean>>({})
/** 已尝试加载产物的任务（无论有无结果）：避免空结果（失败/取消）任务被轮询反复重查。 */
const artifactAttempted = new Set<number>()
/** 每个任务的产物请求版本：只作废「同一任务」更早的在途请求，互不影响其它任务。
 * （不能用全局计数器：批量 forEach 同步入队会互相覆盖，导致仅最后一个任务的结果被保留。） */
const artifactRequestVersions: Record<number, number> = {}

// 产物/缩略图拉取并发受限队列：避免一次画廊里同时发起几十个 HTTP 请求造成卡顿。
const ARTIFACT_CONCURRENCY = 3
const artifactQueue: Array<() => Promise<void>> = []
let activeArtifactLoads = 0
const drainArtifactQueue = () => {
  while (activeArtifactLoads < ARTIFACT_CONCURRENCY && artifactQueue.length) {
    const job = artifactQueue.shift()!
    activeArtifactLoads += 1
    void job().finally(() => {
      activeArtifactLoads -= 1
      drainArtifactQueue()
    })
  }
}
const enqueueArtifactLoad = (job: () => Promise<void>) => {
  artifactQueue.push(job)
  drainArtifactQueue()
}

const isActiveStatus = (status: string) => !isTerminalStatus(status)
const isActive = (task: OperationDetail) => isActiveStatus(task.status)
const isGalleryFailure = (status: string) =>
  status === 'FAILED' || status === 'CANCELLED' || status === 'PARTIAL'

const fallbackDetail = (summary: OperationSummary): OperationDetail => ({
  operationId: summary.operationId,
  status: summary.status,
  actorId: '',
  origin: '',
  idempotencyKey: '',
  requestHash: '',
  metadata: {},
  createdAt: summary.createdAt,
  updatedAt: summary.createdAt,
  runs: Array.from({ length: summary.runCount }, (_, index) => ({
    runId: 0,
    operationId: summary.operationId,
    clientRunId: '',
    sequence: index,
    recipeRevisionId: 0,
    connectionRevisionId: 0,
    status: summary.status,
    trackingState: '',
    createdAt: summary.createdAt,
    updatedAt: summary.createdAt
  }))
})

const tasks = computed(() => {
  const sorted = [...workspace.operationSummaries].sort(
    (a, b) => (Number(b.createdAt) || 0) - (Number(a.createdAt) || 0)
  )
  return sorted.map((summary) => {
    const fromStore = workspace.operations.find(
      (operation) => operation.operationId === summary.operationId
    )
    const fromCache = details.value[summary.operationId]
    return fromStore ?? fromCache ?? fallbackDetail(summary)
  })
})

// 显示名与色调的**唯一权威**在 shared/lib/taskStatus（TaskCenter 同用一份）。
const statusLabel = taskStatusLabel
const statusTone = taskStatusTone

const operationLabel = (task: OperationDetail) => {
  if (task.recipeName) return task.recipeName
  return t('task.operationLabel', { id: task.operationId })
}

const filters = computed<Array<{ value: TaskStatusFilter; label: string; count: number }>>(() => [
  { value: 'all', label: t('history.all'), count: tasks.value.length },
  {
    value: 'active',
    label: t('history.active'),
    count: tasks.value.filter((task) => isActive(task)).length
  },
  {
    value: 'SUCCEEDED',
    label: t('status.success'),
    count: tasks.value.filter((task) => task.status === 'SUCCEEDED').length
  },
  {
    value: 'FAILED',
    label: t('status.failed'),
    count: tasks.value.filter((task) => task.status === 'FAILED').length
  },
  {
    value: 'CANCELLED',
    label: t('status.cancelled'),
    count: tasks.value.filter((task) => task.status === 'CANCELLED').length
  }
])

const filteredTasks = computed(() => {
  const query = keyword.value.trim().toLowerCase()
  return (
    tasks.value
      .filter((task) => {
        const matchesStatus =
          statusFilter.value === 'all' ||
          (statusFilter.value === 'active' ? isActive(task) : task.status === statusFilter.value)
        return (
          matchesStatus &&
          (!query ||
            operationLabel(task).toLowerCase().includes(query) ||
            String(task.operationId).includes(query))
        )
      })
      // 画廊最多展示最近 20 个任务，避免海量历史一次渲染/预取造成卡顿。
      .slice(0, MAX_TASKS)
  )
})
const hasTasks = computed(() => tasks.value.length > 0)

// ---- 产物预览加载 ----
const ensureDetail = async (operationId: number) => {
  if (workspace.operations.some((item) => item.operationId === operationId)) return
  if (details.value[operationId]) return
  const existing = detailPromises.get(operationId)
  if (existing) return existing
  const promise = workspace
    .loadOperationDetail(operationId)
    .then((detail) => {
      details.value = { ...details.value, [operationId]: detail }
      return detail
    })
    .catch(() => undefined)
  detailPromises.set(operationId, promise)
  try {
    return await promise
  } finally {
    detailPromises.delete(operationId)
  }
}

const isPreviewableArtifact = (artifact: { mimeType: string; filename: string }) =>
  artifact.mimeType.startsWith('image/') ||
  artifact.mimeType.startsWith('video/') ||
  /\.(png|jpe?g|webp|gif|avif|mp4|webm|mov|m4v)$/i.test(artifact.filename)
const artifactKind = (artifact: { mimeType: string; filename: string }) =>
  artifact.mimeType.startsWith('video/') || /\.(mp4|webm|mov|m4v)$/i.test(artifact.filename)
    ? 'video'
    : 'image'
const previewableArtifacts = (operationId: number) =>
  (artifactItems.value[operationId] ?? []).filter(isPreviewableArtifact)

/**
 * 预览项：结果 + **它来自哪条输入**（愿景里"结果按输入分组查看"）。
 *
 * 标签判据在 `shared/lib/runLabel.ts` —— 与任务中心共用**同一份**，不在这里再写一遍：
 * 两处各写一份的下场就是同一批任务在两个界面上有两种说法（本仓库真实发生过）。
 *
 * ⚠️ `previewResults` 从这里派生，别各写一份：两者一旦分叉，就会"数出来 3 项、点开只有 2 项"。
 */
const previewItems = (task: OperationDetail): { result: TaskResult; label: string }[] =>
  previewableArtifacts(task.operationId)
    .filter((artifact) => Boolean(artifactUrls.value[artifact.artifactId]))
    .map((artifact) => {
      const run = task.runs.find((candidate) => candidate.runId === artifact.runId)
      return {
        result: {
          url: artifactUrls.value[artifact.artifactId],
          outputType: artifact.mimeType,
          nodeId: String(artifact.artifactId)
        } as TaskResult,
        label: run ? `${operationLabel(task)} · ${runGroupLabel(run)}` : operationLabel(task)
      }
    })
const previewResults = (task: OperationDetail): TaskResult[] =>
  previewItems(task).map((item) => item.result)
const galleryPreview = (task: OperationDetail) => {
  const first = previewableArtifacts(task.operationId).find(
    (artifact) => artifactUrls.value[artifact.artifactId]
  )
  if (!first) return undefined
  return { kind: artifactKind(first), url: artifactUrls.value[first.artifactId] }
}
const galleryResultCount = (task: OperationDetail) => previewableArtifacts(task.operationId).length

const loadArtifacts = (task: OperationDetail) => {
  if (isActive(task) || artifactAttempted.has(task.operationId)) return
  const resolved =
    workspace.operations.find((item) => item.operationId === task.operationId) ??
    details.value[task.operationId] ??
    task
  const runIds = resolved.runs.filter((run) => run.runId > 0).map((run) => run.runId)
  if (!runIds.length) return
  const token = (artifactRequestVersions[task.operationId] ?? 0) + 1
  artifactRequestVersions[task.operationId] = token
  artifactLoading.value[task.operationId] = true
  // 并发受限：同一时间最多加载 ARTIFACT_CONCURRENCY 个任务，避免大量缩略图并行拉取造成卡顿。
  enqueueArtifactLoad(async () => {
    try {
      const pages = await Promise.all(runIds.map((runId) => v2.listArtifacts(runId)))
      if (artifactRequestVersions[task.operationId] !== token) return
      artifactAttempted.add(task.operationId)
      artifactItems.value[task.operationId] = pages.flatMap((page) => page.items)
      for (const artifact of previewableArtifacts(task.operationId)) {
        if (artifactUrls.value[artifact.artifactId]) continue
        artifactLoading.value[artifact.artifactId] = true
        try {
          const { blob } = await v2.fetchArtifactBytes(artifact.artifactId)
          if (artifactRequestVersions[task.operationId] !== token) return
          artifactUrls.value[artifact.artifactId] = URL.createObjectURL(blob)
        } catch {
          // 预览失败不阻断列表。
        } finally {
          artifactLoading.value[artifact.artifactId] = false
        }
      }
    } catch {
      // 产物读取失败保持列表可见。
    } finally {
      if (artifactRequestVersions[task.operationId] === token)
        artifactLoading.value[task.operationId] = false
    }
  })
}

const playVideoPreview = (event: Event) => {
  const video = event.currentTarget as HTMLVideoElement
  void video.play().catch(() => undefined)
}
const resetVideoPreview = (event: Event) => {
  const video = event.currentTarget as HTMLVideoElement
  video.pause()
  video.currentTime = 0
}

const openPreview = (task: OperationDetail) => {
  previewTask.value = task
  const first = previewResults(task)[0]
  const index = previewGallery.value.findIndex((item) => item.result.nodeId === first?.nodeId)
  previewIndex.value = index >= 0 ? index : 0
}

/**
 * 跨任务画廊：历史里所有有结果的终态任务，按列表顺序排列，供预览框左右切换。
 *
 * ⚠️ 每个任务的**全部**结果都要摊平进来。预览框在 gallery 非空时**只认 gallery**
 * （见 ResultPreviewDialog 的 previewItems），每个任务只放第 1 张的话，
 * 一次出 3 张的批量任务在预览框里就只剩 1 张，另外 2 张永远点不到。
 */
const previewGallery = computed(() =>
  filteredTasks.value
    .filter((task) => !isActive(task))
    .flatMap((task) =>
      previewItems(task).map((item) => ({ result: item.result, label: item.label }))
    )
)
const previewIndex = ref(0)
const retryingId = ref<number>()
/**
 * 「它其实没在动 · 等确认 · 14:32 判失败」——一句话说清"在等什么、等到什么时候"。
 *
 * 没有死线的档位（排队、退避重试）**只说原因**：说"到点判失败"会让人去等一个不会发生的事件
 * （后端为这句话专门改过一次 `deadlineEffect`）。
 */
const attentionLine = (attention: { reason: string; deadline?: string | null }) => {
  const reason = attentionReasonLabel(attention.reason)
  const deadline = attention.deadline ? formatDate(attention.deadline) : ''
  return deadline ? `${reason} · ${t('attention.deadline', { time: deadline })}` : reason
}
const retry = async (task: OperationDetail) => {
  if (retryingId.value !== undefined) return
  // ⚠️ 先问一句：重试的语义是**新建一个任务**（会再计费一次），不是"把这一次接着跑完"。
  if (!window.confirm(t('task.confirmRetry'))) return
  retryingId.value = task.operationId
  try {
    await v2.retryOperation(task.operationId)
    await workspace.loadOperationSummaries(100)
  } catch (error) {
    downloadError.value =
      error instanceof Error
        ? t('task.rerunFailedDetail', { message: error.message })
        : t('task.rerunFailed')
  } finally {
    retryingId.value = undefined
  }
}
const removingId = ref<number>()
const remove = async (task: OperationDetail) => {
  if (removingId.value !== undefined) return
  if (!window.confirm(t('task.confirmDelete', { id: task.operationId }))) return
  removingId.value = task.operationId
  try {
    await v2.deleteOperation(task.operationId)
    await workspace.loadOperationSummaries(100)
  } catch (error) {
    downloadError.value =
      error instanceof Error
        ? t('task.deleteFailedDetail', { message: error.message })
        : t('task.deleteFailed')
  } finally {
    removingId.value = undefined
  }
}
const clearing = ref(false)
const clearFinished = async () => {
  if (clearing.value) return
  const terminal = tasks.value.filter((task) => isTerminalStatus(task.status))
  if (!terminal.length) return
  if (!window.confirm(t('task.confirmClear', { count: terminal.length }))) return
  clearing.value = true
  try {
    for (const task of terminal) {
      await v2.deleteOperation(task.operationId)
    }
    await workspace.loadOperationSummaries(100)
  } catch (error) {
    downloadError.value =
      error instanceof Error
        ? t('task.clearFailedDetail', { message: error.message })
        : t('task.clearFailed')
  } finally {
    clearing.value = false
  }
}
const downloadTask = async (task: OperationDetail) => {
  const first = previewableArtifacts(task.operationId)[0]
  if (!first || downloadingId.value !== undefined) return
  downloadError.value = ''
  downloadingId.value = task.operationId
  try {
    const { filename, path } = await v2.saveArtifact(first.artifactId)
    // 成功也要说一句，并报出**完整路径**：桌面外壳不弹下载提示，不说用户就以为"点了没反应"。
    appStore.showNotification('success', t('task.downloaded', { name: filename, path }), {
      label: t('task.showInFolder'),
      run: () => void v2.openDownloadDir(path).catch(() => undefined)
    })
  } catch (error) {
    downloadError.value =
      error instanceof Error
        ? t('task.downloadFailedDetail', { message: error.message })
        : t('task.downloadFailed')
  } finally {
    downloadingId.value = undefined
  }
}
const downloadPreviewResult = async (result: TaskResult, index: number) => {
  const artifactId = Number(result.nodeId)
  if (!Number.isInteger(artifactId)) return
  previewDownloadingIndex.value = index
  try {
    const { filename, path } = await v2.saveArtifact(artifactId)
    appStore.showNotification('success', t('task.downloaded', { name: filename, path }), {
      label: t('task.showInFolder'),
      run: () => void v2.openDownloadDir(path).catch(() => undefined)
    })
  } catch (error) {
    downloadError.value =
      error instanceof Error
        ? t('task.downloadFailedDetail', { message: error.message })
        : t('task.downloadFailed')
  } finally {
    previewDownloadingIndex.value = -1
  }
}

const formatDate = (value: string) => {
  const date = new Date(Number(value) || value)
  if (Number.isNaN(date.getTime())) return ''
  return new Intl.DateTimeFormat(dateLocale.value, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit'
  }).format(date)
}

// 可见运行任务：先确保 detail，再加载产物预览。
// 范围与渲染上限**同一个常量**：此前这里是写死的 40，而画廊只渲染 20 张——多预取的一倍
// 完全是白拉的（每张都是原图，见下）。
watch(
  filteredTasks,
  (list) => {
    list.slice(0, MAX_TASKS).forEach((task) => {
      void (async () => {
        await ensureDetail(task.operationId)
        void loadArtifacts(task)
      })()
    })
  },
  { immediate: true }
)

let pollTimer: number | undefined
const refresh = async () => {
  const succeeded = await workspace.loadOperationSummaries(100)
  if (succeeded) {
    tasks.value.forEach((task) => {
      if (!isActive(task)) void loadArtifacts(task)
    })
  }
}
onMounted(async () => {
  const succeeded = await workspace.loadOperationSummaries(100)
  if (succeeded) {
    pollTimer = window.setInterval(() => {
      if (workspace.operationSummaries.some((item) => !isTerminalStatus(item.status))) {
        void refresh()
      }
    }, 5000)
  }
})
onBeforeUnmount(() => {
  if (pollTimer) window.clearInterval(pollTimer)
  // 作废在途的产物请求：它们会在 `createObjectURL` 之前因版本号对不上而提前退出。
  for (const key of Object.keys(artifactRequestVersions)) {
    const operationId = Number(key)
    artifactRequestVersions[operationId] = (artifactRequestVersions[operationId] ?? 0) + 1
  }
  // 释放缩略图。**object URL 不 revoke，整张原图就一直留在文档里**（每张约 1 MB）：
  // 此前这里只清定时器，所以每次进这个页面都会再留一份，只有整页刷新才还回来。
  for (const url of Object.values(artifactUrls.value)) URL.revokeObjectURL(url)
  artifactUrls.value = {}
})
</script>
<style scoped>
.history-layout {
  margin-top: 1.5rem;
}
/*
 * 状态统计条：从"表格栅格"（每格带边框 + 选中项同时加底色和下划线）改成
 * 分段轨道 —— 选中项是一枚浮起的玻璃片，只有一个选中信号。
 * 数字用显示字体 + tabular-nums：这是全站少见的"大号数字"位置，值得给尺度。
 */
.stats-strip {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 4px;
  border: 1px solid var(--glass-panel-edge);
  border-radius: var(--radius-md);
  background: var(--glass-track-bg);
  padding: 4px;
}
.stat-item {
  height: 3.5rem;
  justify-content: space-between;
  border: 0;
  border-radius: calc(var(--radius-md) - 3px);
  padding: 0 0.85rem;
  text-align: left;
  font-size: 0.8125rem;
  color: var(--app-muted-foreground);
}
.stat-item strong {
  font-family: var(--font-display);
  font-size: 1.375rem;
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  color: var(--app-foreground);
}
.stat-active {
  background: var(--glass-panel-bg);
  box-shadow: var(--glass-panel-shadow);
  color: var(--app-foreground);
}
.archive-toolbar {
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
  border-bottom: 1px solid var(--app-border);
  padding: 1rem 0;
}
.archive-grid {
  display: grid;
  grid-template-columns: repeat(1, minmax(0, 1fr));
  gap: 1rem;
}
.archive-card:hover {
  border-color: color-mix(in srgb, var(--app-primary) 55%, var(--app-border));
}
.preview-area {
  position: relative;
  display: block;
  width: 100%;
  aspect-ratio: 3 / 4;
  overflow: hidden;
  border-bottom: 1px solid var(--app-border);
  background: var(--app-secondary);
  padding: 0;
  text-align: left;
}
.preview-area:disabled {
  cursor: default;
}
.preview-area.is-loading::after {
  position: absolute;
  inset: 0;
  background: linear-gradient(
    100deg,
    transparent 20%,
    color-mix(in srgb, var(--app-foreground) 6%, transparent) 50%,
    transparent 80%
  );
  background-size: 200% 100%;
  animation: skeleton-shimmer 1.4s ease-in-out infinite;
  content: '';
}
@keyframes skeleton-shimmer {
  0% {
    background-position: 100% 0;
  }
  100% {
    background-position: -100% 0;
  }
}
.archive-video-play {
  position: absolute;
  top: 50%;
  left: 50%;
  display: grid;
  width: 2.25rem;
  height: 2.25rem;
  place-items: center;
  border: 1px solid color-mix(in srgb, white 44%, transparent);
  border-radius: 999px;
  background: color-mix(in srgb, black 45%, transparent);
  padding: 0.25rem 0.375rem;
  color: white;
  pointer-events: none;
  transform: translate(-50%, -50%);
  transition:
    opacity 160ms ease,
    transform 160ms ease;
}
.preview-area:hover .archive-video-play,
.preview-area:focus-visible .archive-video-play {
  opacity: 0;
  transform: translate(-50%, -50%) scale(0.9);
}
@media (min-width: 640px) {
  .stats-strip {
    grid-template-columns: repeat(5, minmax(0, 1fr));
  }
  .archive-toolbar {
    flex-direction: row;
    align-items: center;
    justify-content: space-between;
  }
  /*
   * ⚠️ 用 `auto-fill`，**不要** `auto-fit`、也不要写死列数：
   *   - `auto-fit` 会**折叠空轨道** ⇒ 任务少时卡片被拉大（实测 1920 窗口 + 3 个任务：
   *     6 条轨道折成 3 条，每张 502px 宽、预览图 502×628 —— 用户报"图有点大"）；
   *   - 写死 2 列 ⇒ 1279 视口下每张 511px，同一个毛病。
   * `auto-fill` 保留空轨道，卡片尺寸因此只由**窗口宽度**决定，与任务数量无关；
   * 一屏几列随窗口自适应（约 200–290px 一张，回到缩略图尺度）。
   */
  .archive-grid {
    grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
  }
}
@media (min-width: 1280px) {
  /* 列数已在 ≥640 那条里交给 `auto-fill` 自适应，这里不再覆盖（覆盖一次就多一处会打架的规则）。
     宽屏下唯一该变的是预览比例：宽屏时卡片更宽，4/5 比 3/4 矮一些、一屏能多看半张。 */
  .preview-area {
    aspect-ratio: 4 / 5;
  }
}
</style>
