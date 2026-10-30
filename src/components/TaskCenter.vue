<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import {
  CircleAlert,
  Clock3,
  Download,
  FileText,
  LoaderCircle,
  PanelRightClose,
  PanelRightOpen,
  Play,
  RefreshCw,
  RotateCcw,
  Send,
  TriangleAlert,
  Wand2,
  X
} from '@lucide/vue'
import { useRouter } from 'vue-router'
import { useRunnerSessionsStore } from '@/features/v2/stores/runner-sessions'
import Card from '@/components/ui/Card.vue'
import Button from '@/components/ui/Button.vue'
import Dialog from '@/components/ui/Dialog.vue'
import IconButton from '@/components/ui/IconButton.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import EmptyState from '@/components/shared/EmptyState.vue'
import SectionHeader from '@/components/shared/SectionHeader.vue'
import StatusIndicator from '@/components/shared/StatusIndicator.vue'
import ResultPreviewDialog from '@/components/shared/ResultPreviewDialog.vue'
import { isTerminalStatus, v2, type OperationDetail, type RunRef } from '@/shared/api/v2'
import { downloadOperationArtifacts as downloadAllArtifacts } from '@/shared/lib/downloadArtifacts'
import { runGroupLabel } from '@/shared/lib/runLabel'
import type { TaskResult } from '@/shared/types/runninghub'
import { useV2WorkspaceStore } from '@/features/v2/stores/workspace'
import { useAppStore } from '@/shared/stores/app'
import { useTaskCenterStore } from '@/shared/stores/task-center'
import { TASKCENTER_OVERLAY_MAX_WIDTH } from '@/shared/config/layout'
import { useLocale } from '@/shared/composables/useLocale'
import { taskStatusLabel, taskStatusTone } from '@/shared/lib/taskStatus'
import { attentionExplanation, attentionReasonLabel, attentionTone } from '@/shared/lib/attention'
import { formatSeconds } from '@/shared/lib/duration'
import { formatRelativeTime, timestampOf } from '@/shared/lib/relativeTime'

const router = useRouter()
const workspace = useV2WorkspaceStore()
const taskCenter = useTaskCenterStore()
const runnerSessions = useRunnerSessionsStore()
const appStore = useAppStore()
const { t, dateLocale } = useLocale()

const isRefreshing = ref(false)
const lastRefreshedAt = ref<Date>()
const now = ref(Date.now())
/**
 * 订阅 10s 时钟。下面几个时间文案函数都在模板里被调用，读一次 `now` 才能让渲染 effect
 * 建立依赖，时钟 tick 时才重算。
 *
 * 之所以包成函数而不是裸写 `now.value`：裸表达式会被 `no-unused-expressions` 判为 error，
 * 而这点噪声会让**真正的**缺陷（漏掉的函数调用）藏在里面看不见。
 */
const trackClock = () => now.value
const drawerId = 'renderloft-task-center'
/**
 * 右侧任务中心有三种形态，由视口宽度决定：
 *
 *   < 1024         底部抽屉（移动端）：遮罩 + 自下而上滑出
 *   1024 – 1199    右侧浮层：遮罩 + 自右向左浮出，不占主区宽度
 *   ≥ 1200         常驻侧栏：主区让出 `--task-rail-width`（规则在 AppShell.vue）
 *
 * 阈值 1200 = 桌面外壳的默认窗口宽度（1200×800），所以**默认打开就是常驻侧栏**——
 * 任务中心是这个产品的主画面之一，不该在默认尺寸下被藏进浮层（理由见 layout.ts）。
 *
 * `isOverlay`（浮层行为：遮罩 / dialog 语义 / Esc 关闭 / 缩窗自动收起）覆盖前两档；
 * 形态差异（底部抽屉 vs 右侧浮层）由 CSS 媒体查询承担，不需要额外的 JS 状态。
 *
 * 阈值的**权威定义**在 `shared/config/layout.ts`（同一数字还决定 AppShell 的让位
 * 媒体查询与 store 的默认展开状态）；本文件与那个常量必须同源，不要另写一份。
 */
const viewportWidth = ref(
  typeof window === 'undefined' ? TASKCENTER_OVERLAY_MAX_WIDTH : window.innerWidth
)
const isOverlay = computed(() => viewportWidth.value < TASKCENTER_OVERLAY_MAX_WIDTH)
const taskToggle = ref<InstanceType<typeof IconButton> | null>(null)
const drawerPanel = ref<HTMLElement | null>(null)
let refreshTimer: number | undefined
let clockTimer: number | undefined
// v1 视图切换：任务 / 批次。
const taskView = ref<'tasks' | 'batches'>('tasks')

interface ArtifactPreviewItem {
  artifactId: number
  runId: number
  filename: string
  mimeType: string
  byteSize: number
  checksum: string
}
/** 每个任务的产物请求版本：只作废「同一任务」更早的在途请求，互不影响其它任务。 */
const artifactRequestVersions: Record<number, number> = {}
/** 已尝试加载产物的任务（无论有无结果）：避免空结果（失败/取消）任务被轮询反复重查。 */
const artifactAttempted = new Set<number>()
const artifactItems = ref<Record<number, ArtifactPreviewItem[]>>({})
const artifactPromises = new Map<number, Promise<void>>()
const artifactUrls = ref<Record<number, string>>({})
const artifactLoading = ref<Record<number, boolean>>({})
const artifactErrors = ref<Record<number, string>>({})
const previewTask = ref<OperationDetail>()
const previewDownloadingIndex = ref(-1)
const detailTask = ref<OperationDetail>()
const detailReasons = ref<RunFailure[]>([])
const batchDetail = ref<OperationDetail>()
const cancellingId = ref<number | null>(null)

const isPreviewableArtifact = (artifact: { mimeType: string; filename: string }) =>
  artifact.mimeType.startsWith('image/') ||
  artifact.mimeType.startsWith('video/') ||
  /\.(png|jpe?g|webp|gif|avif|mp4|webm|mov|m4v)$/i.test(artifact.filename)
const artifactKind = (artifact: { mimeType: string; filename: string }) =>
  artifact.mimeType.startsWith('video/') || /\.(mp4|webm|mov|m4v)$/i.test(artifact.filename)
    ? 'video'
    : 'image'
/** 该任务**全部**可预览产物。结果数徽章与预览框都用它，**别在这里截断**：
 *  截断会让「3 项」的批量任务在预览框里只剩 1 张可看。 */
const previewableArtifacts = (operationId: number) =>
  (artifactItems.value[operationId] ?? []).filter(isPreviewableArtifact)
/** 首屏只预取前几张（省流量），其余等用户点开预览再补，见 `ensureArtifactsLoaded`。 */
const EAGER_ARTIFACT_LIMIT = 3
const eagerArtifacts = (operationId: number) =>
  previewableArtifacts(operationId).slice(0, EAGER_ARTIFACT_LIMIT)
/**
 * 预览项：结果 + **它来自哪条输入**。
 *
 * 愿景里那句"结果按输入分组查看"：产物本来就按 run 取（`listArtifacts(runId)`），摊平的顺序
 * 也已经是 run 顺序 —— 分组一直在，缺的只是标签（此前每项都贴任务名，于是"12 张里哪 3 张来自
 * 第 2 条提示词"没法回答）。标签的判据在 `shared/lib/runLabel.ts`（两个预览框共用一份）。
 *
 * ⚠️ `previewResults` 从这里派生，别各写一份：两者一旦分叉，就会出现"数出来 3 项、点开只有 2 项"。
 */
const previewItems = (
  operation: OperationDetail
): { result: TaskResult; label: string; runId: number }[] =>
  previewableArtifacts(operation.operationId)
    .filter((artifact) => Boolean(artifactUrls.value[artifact.artifactId]))
    .map((artifact) => {
      const run = operation.runs.find((candidate) => candidate.runId === artifact.runId)
      return {
        result: {
          url: artifactUrls.value[artifact.artifactId],
          outputType: artifact.mimeType,
          nodeId: String(artifact.artifactId)
        } as TaskResult,
        label: run
          ? `${operationLabel(operation)} · ${runGroupLabel(run)}`
          : operationLabel(operation),
        runId: artifact.runId
      }
    })
const previewResults = (operation: OperationDetail): TaskResult[] =>
  previewItems(operation).map((item) => item.result)
const galleryPreview = (operation: OperationDetail) => {
  const first = previewableArtifacts(operation.operationId).find(
    (artifact) => artifactUrls.value[artifact.artifactId]
  )
  if (!first) return undefined
  return { kind: artifactKind(first), url: artifactUrls.value[first.artifactId] }
}
const galleryResultCount = (operation: OperationDetail) =>
  previewableArtifacts(operation.operationId).length
const isGalleryFailure = (operation: OperationDetail) =>
  operation.status === 'FAILED' ||
  operation.status === 'CANCELLED' ||
  operation.status === 'PARTIAL'
const openPreview = (operation: OperationDetail) => {
  previewTask.value = operation
  const first = previewResults(operation)[0]
  const index = previewGallery.value.findIndex((item) => item.result.nodeId === first?.nodeId)
  previewIndex.value = index >= 0 ? index : 0
  // 列表只预取了前几张，点开预览时把剩下的补上——用户点开就是要看全部产出。
  void ensureArtifactsLoaded(operation)
}

/**
 * 跨任务画廊：右侧「最近任务」里有结果的终态任务，供预览框左右切换。
 *
 * ⚠️ 每个任务的**全部**结果都要摊平进来。预览框在 gallery 非空时**只认 gallery**
 * （见 ResultPreviewDialog 的 previewItems），每个任务只放第 1 张的话，
 * 一次出 3 张的批量任务在预览框里就只剩 1 张，另外 2 张永远点不到。
 *
 * 标签用**每条 run 自己的**（而不是任务名）：预览框把它显示成标题，左右翻的时候就能看出
 * "这张是哪条输入出的"——这就是"结果按输入分组查看"。
 */
const previewGallery = computed(() =>
  galleryOperations.value.flatMap((operation) =>
    previewItems(operation).map((item) => ({ result: item.result, label: item.label }))
  )
)
const previewIndex = ref(0)
const playVideoPreview = (event: Event) => {
  const video = event.currentTarget as HTMLVideoElement
  void video.play().catch(() => undefined)
}
const resetVideoPreview = (event: Event) => {
  const video = event.currentTarget as HTMLVideoElement
  video.pause()
  video.currentTime = 0
}
const downloadPreviewResult = async (result: TaskResult, index: number) => {
  const artifactId = Number(result.nodeId)
  if (!Number.isInteger(artifactId)) return
  previewDownloadingIndex.value = index
  try {
    await downloadArtifact(artifactId)
  } catch (error) {
    appStore.showNotification(
      'error',
      error instanceof Error ? error.message : t('task.downloadFailed')
    )
  } finally {
    previewDownloadingIndex.value = -1
  }
}
/**
 * 下载**一个操作产出的全部产物**（实现见 `shared/lib/downloadArtifacts.ts`）。
 *
 * ⚠️ 这里原来是一个说谎的版本：只取 `previewableArtifacts(id)[0]` —— **只下第一张**，
 * 对其余的只字不提。一批 100 张图，用户点一下就以为拿到了全部。任务历史页那份也没跟上；
 * 抽到 shared 之后两个入口共用同一套"逐个下、汇总报"的判据。
 */
const downloadOperationArtifacts = (operation: OperationDetail) =>
  downloadAllArtifacts(operation.runs.map((run) => run.runId))
/**
 * 下载一个产物。
 *
 * ⚠️ 这里原来把**成功和失败都吞了**（`catch {}` + 一句"不阻断刷新"），结果是用户报的那个现象：
 * 点下载"没反应"。而真相有两层——① 桌面外壳（WebView2）**不弹任何下载提示**，文件其实已经
 * 落进系统下载目录了；② 真失败时也一个字都不说。所以现在两条都报：成功给"已下载"，失败给原因。
 */
/**
 * 下载一个产物：**由 daemon 落盘**（见 `v2.saveArtifact` 的说明），成功后报出**完整路径**，
 * 并给一个"在文件夹中显示"的动作——主流下载提示就是这么收尾的。
 *
 * ⚠️ 这里原来把成功和失败**一起吞了**（`catch {}` + 一句"不阻断刷新"），于是用户看到的现象是
 * "点了没反应"：桌面外壳不弹下载提示，而我们也一个字都不说。
 */
const downloadArtifact = async (artifactId: number) => {
  try {
    const { filename, path } = await v2.saveArtifact(artifactId)
    appStore.showNotification('success', t('task.downloaded', { name: filename, path }), {
      label: t('task.showInFolder'),
      run: () => void v2.openDownloadDir(path).catch(() => undefined)
    })
  } catch (error) {
    appStore.showNotification(
      'error',
      error instanceof Error ? error.message : t('task.downloadFailed')
    )
  }
}

const isActiveStatus = (status: string) => !isTerminalStatus(status)
const isActive = (operation: OperationDetail) => isActiveStatus(operation.status)
const activeSummaries = computed(() =>
  workspace.operationSummaries
    .filter((operation) => isActiveStatus(operation.status))
    .sort((a, b) => timestampOf(b.createdAt) - timestampOf(a.createdAt))
)
const recentSummaries = computed(() =>
  workspace.operationSummaries
    .filter((operation) => !isActiveStatus(operation.status))
    .sort((a, b) => timestampOf(b.createdAt) - timestampOf(a.createdAt))
)
const activeCount = computed(() => activeSummaries.value.length)
/**
 * 元素类型必须显式标注：未加载详情的兜底对象没有 `recipeName`，
 * 不标注会推出「有 recipeName | 没有 recipeName」的联合，模板里读它就报 TS2339。
 */
const taskOperations = computed<OperationDetail[]>(() => {
  const visibleSummaries = [
    ...activeSummaries.value.slice(0, 12),
    ...recentSummaries.value.slice(0, 12)
  ]
  return visibleSummaries.map((summary) => {
    const detail = workspace.operations.find(
      (operation) => operation.operationId === summary.operationId
    )
    if (detail) {
      // 以每次轮询刷新的 summary 状态为准，避免缓存 detail 的过期状态把
      // 已终态的任务继续误判为进行中（状态卡死/从最近完成里消失）。
      return { ...detail, status: summary.status }
    }
    return {
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
    }
  })
})
const hasError = computed(() => Boolean(workspace.operationsError))
const activeOperations = computed(() =>
  taskOperations.value.filter((operation) => isActive(operation))
)
const activeBatchOperations = computed(() =>
  activeOperations.value.filter((operation) => operation.runs.length > 1)
)
const activeSingleOperations = computed(() =>
  activeOperations.value.filter((operation) => operation.runs.length <= 1)
)
const galleryOperations = computed(() =>
  taskOperations.value.filter((operation) => !isActive(operation)).slice(0, 6)
)

// ---------- 批次（多 Run 的 Operation） ----------
const batchOperations = computed(() =>
  taskOperations.value.filter((operation) => operation.runs.length > 1)
)
/** 批次聚合状态（用状态码而非文案做逻辑键，文案只用于展示）。 */
const batchRunState = (
  operation: OperationDetail
): 'running' | 'partial' | 'succeeded' | 'failed' => {
  const realRuns = operation.runs.filter((run) => run.runId > 0)
  if (realRuns.length) {
    if (realRuns.some((run) => !isTerminalStatus(run.status))) return 'running'
    const failed = realRuns.some((run) => run.status === 'FAILED' || run.status === 'CANCELLED')
    return failed ? 'partial' : 'succeeded'
  }
  // 尚无真实 per-run 状态（详情未加载）时，用操作聚合状态兜底。
  if (operation.status === 'SUCCEEDED') return 'succeeded'
  if (operation.status === 'PARTIAL') return 'partial'
  if (operation.status === 'FAILED') return 'failed'
  return 'running'
}
const batchRunLabel = (operation: OperationDetail) => {
  const state = batchRunState(operation)
  if (state === 'partial') return t('task.batchPartial')
  if (state === 'succeeded') return t('task.batchSucceeded')
  if (state === 'failed') return t('task.batchFailed')
  return t('task.batchRunning')
}
/** 批次聚合状态 → 徽章语义。`as const` 是必需的：否则字面量被推成 string，返回值类型对不上。 */
const BATCH_BADGE_STATUS = {
  running: 'info',
  partial: 'warning',
  succeeded: 'success',
  failed: 'error'
} as const
const batchRunBadgeStatus = (
  operation: OperationDetail
): 'info' | 'warning' | 'success' | 'error' =>
  BATCH_BADGE_STATUS[batchRunState(operation)] ?? 'info'
const batchRunSummary = (operation: OperationDetail) => {
  const realRuns = operation.runs.filter((run) => run.runId > 0)
  if (realRuns.length) {
    const completed = realRuns.filter((run) => run.status === 'SUCCEEDED').length
    const failed = realRuns.filter(
      (run) => run.status === 'FAILED' || run.status === 'CANCELLED'
    ).length
    return t('task.batchSummaryRuns', {
      completed,
      running: realRuns.length - completed - failed,
      failed
    })
  }
  if (operation.status === 'SUCCEEDED')
    return t('task.batchSummaryDone', { count: operation.runs.length })
  if (operation.status === 'PARTIAL')
    return t('task.batchSummaryPartial', { count: operation.runs.length })
  if (operation.status === 'FAILED')
    return t('task.batchSummaryFailed', { count: operation.runs.length })
  return t('task.batchSummaryRunning', { count: operation.runs.length })
}

// ---------- 状态与文案 ----------
// 显示名与色调的**唯一权威**在 shared/lib/taskStatus（V2Tasks 同用一份）。
const statusLabel = taskStatusLabel
const statusTone = taskStatusTone
const statusBadgeStatus = (status: string): 'info' | 'warning' =>
  status === 'RUNNING' ? 'info' : 'warning'

const operationLabel = (operation: OperationDetail) => {
  if (operation.recipeName) return operation.recipeName
  const firstRun = operation.runs[0]
  if (firstRun?.clientRunId) return firstRun.clientRunId
  return t('task.operationLabel', { id: operation.operationId })
}
const operationSummary = (operation: OperationDetail) => {
  const completed = operation.runs.filter((run) => run.status === 'SUCCEEDED').length
  return `${completed}/${operation.runs.length} ${t('task.done')}`
}
const taskAppIdentity = (operation: OperationDetail) =>
  operation.recipeName
    ? t('task.appIdentity', { name: operation.recipeName })
    : t('task.operationLabel', { id: operation.operationId })
const shortTaskId = (operation: OperationDetail) => {
  const taskId = operation.runs[0]?.clientRunId
  const short = taskId
    ? taskId.length > 14
      ? `${taskId.slice(0, 6)}...${taskId.slice(-4)}`
      : taskId
    : ''
  return short ? `#${operation.operationId} · ${short}` : `#${operation.operationId}`
}
const activeMediaLabel = (status: string) => {
  const labels: Record<string, string> = {
    RUNNING: t('task.mediaGenerating'),
    QUEUED: t('task.mediaWaitingResource'),
    RETRY_WAIT: t('task.mediaWaitingRetry'),
    PREPARING: t('task.mediaSubmitting'),
    SUBMITTING: t('task.mediaSubmitting'),
    SUBMISSION_UNCERTAIN: t('task.mediaVerifying')
  }
  return labels[status] ?? t('task.mediaTracking')
}
const mediaTone = (status: string) =>
  status === 'RUNNING'
    ? 'running'
    : status === 'QUEUED' || status === 'RETRY_WAIT'
      ? 'queued'
      : 'submitting'
const mediaKind = (status: string) =>
  status === 'RUNNING'
    ? 'loading'
    : status === 'QUEUED' || status === 'RETRY_WAIT'
      ? 'queued'
      : 'submit'
const activeTrackingLabel = (status: string) => {
  const labels: Record<string, string> = {
    QUEUED: t('task.trackQueued'),
    PREPARING: t('task.trackSubmitted'),
    SUBMITTING: t('task.trackSubmitted'),
    SUBMISSION_UNCERTAIN: t('task.trackUncertain'),
    RUNNING: t('task.trackGenerating'),
    RETRY_WAIT: t('task.trackRetryWait')
  }
  return labels[status] ?? t('task.trackAuto')
}
const activeDuration = (operation: OperationDetail) => {
  trackClock()
  const timestamp = timestampOf(operation.createdAt)
  if (Number.isNaN(timestamp)) return t('task.unknownTime')
  return formatSeconds((Date.now() - timestamp) / 1000)
}
const activityAt = (operation: OperationDetail) =>
  isActive(operation) ? operation.createdAt : operation.updatedAt || operation.createdAt
const operationTime = (operation: OperationDetail) => {
  const timestamp = timestampOf(activityAt(operation))
  if (Number.isNaN(timestamp)) return t('task.unknownTime')
  return new Intl.DateTimeFormat(dateLocale.value, { hour: '2-digit', minute: '2-digit' }).format(
    new Date(timestamp)
  )
}
const operationDatetime = (operation: OperationDetail) => {
  const timestamp = timestampOf(activityAt(operation))
  return Number.isNaN(timestamp) ? undefined : new Date(timestamp).toISOString()
}
const operationDuration = (operation: OperationDetail) => {
  trackClock()
  const started = timestampOf(operation.createdAt)
  const ended = isActive(operation)
    ? Date.now()
    : timestampOf(operation.updatedAt || operation.createdAt)
  if (Number.isNaN(started) || Number.isNaN(ended)) return t('task.unknownTime')
  return formatSeconds((ended - started) / 1000)
}
/** 相对时间文案；`trackClock()` 读一次时钟，让渲染 effect 建立依赖以便随 tick 刷新。 */
const relativeTime = (value: string) => {
  trackClock()
  return formatRelativeTime(value, dateLocale.value, {
    justNow: t('task.justNow'),
    unknownTime: t('task.unknownTime')
  })
}
/** 最近任务卡片的完成时间：<24h 用相对时间（随时钟自动刷新），更久用"月日 时分"。 */
const completionTime = (operation: OperationDetail) => {
  const value = operation.updatedAt || operation.createdAt
  const timestamp = timestampOf(value)
  if (Number.isNaN(timestamp)) return ''
  if (Date.now() - timestamp < 24 * 3600 * 1000) return relativeTime(value)
  return new Intl.DateTimeFormat(dateLocale.value, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit'
  }).format(new Date(timestamp))
}
/** 精确完成时间（悬停提示用）。 */
const completionTimeTitle = (operation: OperationDetail) => {
  const timestamp = timestampOf(operation.updatedAt || operation.createdAt)
  if (Number.isNaN(timestamp)) return ''
  return new Intl.DateTimeFormat(dateLocale.value, {
    dateStyle: 'medium',
    timeStyle: 'short'
  }).format(new Date(timestamp))
}

/** 取一张产物的字节并挂成 blob URL。token 变了（任务被回收/重载）就丢弃结果。 */
const fetchArtifactBlob = async (operationId: number, artifactId: number) => {
  if (artifactUrls.value[artifactId]) return
  const token = artifactRequestVersions[operationId]
  artifactLoading.value[artifactId] = true
  try {
    const { blob } = await v2.fetchArtifactBytes(artifactId)
    if (artifactRequestVersions[operationId] !== token) return
    artifactUrls.value[artifactId] = URL.createObjectURL(blob)
  } catch (error) {
    artifactErrors.value[artifactId] = error instanceof Error ? error.message : String(error)
  } finally {
    artifactLoading.value[artifactId] = false
  }
}

/**
 * 补齐某个任务**全部**可预览产物的字节。
 *
 * 列表加载时只预取前 `EAGER_ARTIFACT_LIMIT` 张（省流量），但用户点开预览就是要看
 * 全部产出——否则 3 张的批量任务只看得到 1 张。
 */
const ensureArtifactsLoaded = async (operation: OperationDetail) => {
  const token = artifactRequestVersions[operation.operationId]
  for (const artifact of previewableArtifacts(operation.operationId)) {
    if (artifactRequestVersions[operation.operationId] !== token) return
    await fetchArtifactBlob(operation.operationId, artifact.artifactId)
  }
}

const loadArtifacts = async (operation: OperationDetail) => {
  if (isActive(operation) || artifactAttempted.has(operation.operationId)) return
  const existing = artifactPromises.get(operation.operationId)
  if (existing) return existing
  const token = (artifactRequestVersions[operation.operationId] ?? 0) + 1
  artifactRequestVersions[operation.operationId] = token
  const request = (async () => {
    try {
      const pages = await Promise.all(
        operation.runs.filter((run) => run.runId > 0).map((run) => v2.listArtifacts(run.runId))
      )
      if (artifactRequestVersions[operation.operationId] !== token) return
      artifactAttempted.add(operation.operationId)
      artifactItems.value[operation.operationId] = pages.flatMap((page) => page.items)
      for (const artifact of eagerArtifacts(operation.operationId)) {
        if (artifactRequestVersions[operation.operationId] !== token) return
        await fetchArtifactBlob(operation.operationId, artifact.artifactId)
      }
    } catch (error) {
      artifactErrors.value[operation.operationId] =
        error instanceof Error ? error.message : String(error)
    }
  })()
  artifactPromises.set(operation.operationId, request)
  try {
    await request
  } finally {
    artifactPromises.delete(operation.operationId)
  }
}

/** 会补拉详情/产物的那批任务：最近完成的前 6 个（画廊缩略图的窗口）。 */
const previewTargets = (operations: OperationDetail[]) =>
  operations.filter((item) => !isActive(item)).slice(0, 6)

/**
 * 释放不在这一批里的产物 URL，并允许它们以后**重新加载**。
 *
 * 这个组件挂在 `AppShell` 上、**几乎不会卸载**，所以只靠 `onBeforeUnmount` 等于不释放：
 * 窗口每滑动一次就多留一批原图（≤18 张、每张约 1 MB），一个长会话里只增不减。
 *
 * 同时清掉 `attempted`/`items`/在途版本号——否则被释放过的任务重新回到窗口时，
 * `loadArtifacts` 会因"已尝试过"直接早退，缩略图**再也加载不出来**。
 */
const releaseArtifactsOutside = (keep: Set<number>) => {
  for (const key of Object.keys(artifactItems.value)) {
    const operationId = Number(key)
    if (keep.has(operationId)) continue
    for (const artifact of artifactItems.value[operationId] ?? []) {
      const url = artifactUrls.value[artifact.artifactId]
      if (url) URL.revokeObjectURL(url)
      delete artifactUrls.value[artifact.artifactId]
      delete artifactLoading.value[artifact.artifactId]
      delete artifactErrors.value[artifact.artifactId]
    }
    delete artifactItems.value[operationId]
    artifactAttempted.delete(operationId)
    artifactRequestVersions[operationId] = (artifactRequestVersions[operationId] ?? 0) + 1
  }
}

watch(
  taskOperations,
  (operations) => {
    // 加载与释放共用同一个集合：两处各写一份是这类 bug 的温床。
    const targets = previewTargets(operations)
    releaseArtifactsOutside(new Set(targets.map((operation) => operation.operationId)))
    // 画廊里最近完成的任务需要详情（runs → artifacts）才能出缩略图；
    // 列表接口只返回 summary（无真实 runId），这里按画廊数量有节制地补拉详情。
    for (const operation of targets) {
      const runsLoaded = operation.runs.some((run) => run.runId > 0)
      const runsSettled =
        runsLoaded && operation.runs.every((run) => run.runId > 0 && isTerminalStatus(run.status))
      if (runsSettled) {
        void loadArtifacts(operation)
      } else {
        // 详情可能缓存于运行期，run 状态停在非终态（操作级状态已由 summary 覆盖，
        // 但 per-run 状态没人刷新）→ 计数会误显示"0/1 已完成"。先刷新详情拿终态
        // run，再据此拉取产物。
        void workspace
          .loadOperationDetail(operation.operationId)
          .catch(() => undefined)
          .then((detail) => {
            void loadArtifacts(detail ? { ...operation, ...detail } : operation)
          })
      }
    }
  },
  { immediate: true }
)

const refresh = async () => {
  if (isRefreshing.value) return
  isRefreshing.value = true
  const succeeded = await workspace.loadOperationSummaries(200)
  if (succeeded) {
    lastRefreshedAt.value = new Date()
    // 只在**真的读到了列表**之后才扫描：首次加载失败时列表是空的，若此时就建立基线，
    // 等下次成功加载会把所有历史终态当成"刚完成"，一次弹一堆通知。
    scanCompletions()
  }
  isRefreshing.value = false
}

const focusDrawer = async () => {
  await nextTick()
  drawerPanel.value?.focus()
}

const focusToggle = () => taskToggle.value?.$el?.focus()

const closeDrawer = async () => {
  taskCenter.isOpen = false
  if (isOverlay.value) await nextTick(focusToggle)
}

const toggleDrawer = async () => {
  if (taskCenter.isOpen) {
    await closeDrawer()
    return
  }
  taskCenter.isOpen = true
  markViewed()
  if (isOverlay.value) await focusDrawer()
}

const onKeydown = async (event: KeyboardEvent) => {
  if (event.key === 'Escape' && taskCenter.isOpen && isOverlay.value) {
    event.preventDefault()
    await closeDrawer()
  }
}

/**
 * 跳到应用库并收起任务中心。
 *
 * 抽成方法而不是写成内联的多语句处理器：内联多语句在格式化时会被拆行、丢掉分号，
 * Vue 编译器随后就解析不了（本仓库踩过：prettier 拆行后构建直接失败）。
 * 模板保持"一次调用"，这类问题就不会再出现。
 */
const goToLibrary = () => {
  void router.push('/library')
  taskCenter.isOpen = false
}

const openTaskDetail = async (operation: OperationDetail) => {
  // 详情对话框必须展示最新 run 状态：缓存的 detail 可能停留在提交时的
  // QUEUED，而操作已转终态（summary 只覆盖操作状态，不刷新子 run）。
  const fresh = await workspace.refreshOperation(operation.operationId).catch(() => operation)
  detailTask.value = fresh
  // 优先用 run 上持久化的结构化错误；历史任务（迁移前无 error 字段）兜底查事件流。
  const fromRuns = failureReasonsFromRuns(fresh)
  detailReasons.value = fromRuns.length
    ? fromRuns
    : await loadFailureReasonsFromEvents(fresh.operationId)
}

interface RunFailure {
  runId: number
  reason: string
  code?: string
}
/** 从 run 终态错误直接提取结构化失败原因（详情接口已返回，无需再拉事件）。 */
const failureReasonsFromRuns = (operation: OperationDetail): RunFailure[] => {
  const failures: RunFailure[] = []
  for (const run of operation.runs) {
    const error = run.error
    if (!error?.message) continue
    failures.push({ runId: run.runId, reason: error.message, code: error.code })
  }
  return failures
}

/** 失败事件类型：远端失败 / 对账期限耗尽 / 提交状态无法确认。 */
const FAILURE_EVENT_TYPES = new Set([
  'run.failed',
  'run.tracking_exhausted',
  'run.tracking_unresolved',
  'run.cancelled'
])
/** 兜底：从 operation 事件流提取失败原因（兼容 error 字段落库前的历史任务）。 */
const loadFailureReasonsFromEvents = async (operationId: number): Promise<RunFailure[]> => {
  try {
    const events = await v2.operationEvents(operationId)
    const failures: RunFailure[] = []
    for (const event of events) {
      if (!FAILURE_EVENT_TYPES.has(event.eventType)) continue
      const payload = event.payload as {
        reason?: unknown
        error?: { message?: string; code?: string }
      } | null
      const reason =
        typeof payload?.reason === 'string'
          ? payload.reason.trim()
          : (payload?.error?.message?.trim() ?? '')
      if (reason) failures.push({ runId: event.runId ?? 0, reason, code: payload?.error?.code })
    }
    return failures
  } catch {
    return []
  }
}

const cancelOperation = async (operation: OperationDetail) => {
  if (cancellingId.value !== null) return
  cancellingId.value = operation.operationId
  try {
    await v2.cancelOperation(operation.operationId)
  } catch {
    // 取消失败时靠下一轮轮询恢复真实状态。
  } finally {
    cancellingId.value = null
  }
  await refresh()
}

// ---- 完成通知 + 未读角标 ----
/**
 * 已经"结过账"的 operation：第一次扫描时就已经是终态的（历史，不补提醒），
 * 或者这一轮已经计过未读 / 发过通知的。
 *
 * ⚠️ 判据必须是**状态迁移**（上次还在跑 → 这次终态），不能是 `createdAt`：
 * 一个跑 20 分钟的任务，创建时间早于"上次打开面板"的时刻，于是它完成时既不计未读、
 * 也不弹通知——而它恰恰是最需要提醒的那一类。`createdAt` 回答的是"什么时候开始的"，
 * 这里问的是"什么时候结束的"，两者本来就不是一回事。
 */
const settledCompletions = new Set<number>()
/** 第一次扫描只建立基线（把当时已经终态的记录当历史），避免一打开就为过去 100 个任务弹通知。 */
let completionScanPrimed = false
const unread = ref(0)
const notifyCompletion = (summary: { operationId: number }) => {
  if (typeof window === 'undefined' || !('Notification' in window)) return
  if (Notification.permission !== 'granted') {
    // 首次惰性请求；被拒则静默降级为角标提示。
    void Notification.requestPermission()
    return
  }
  try {
    const notification = new Notification(t('task.notifyTitle'), {
      body: t('task.operationLabel', { id: summary.operationId })
    })
    notification.onclick = () => window.focus()
  } catch {
    // 某些环境不支持构造通知，忽略。
  }
}
const scanCompletions = () => {
  const firstScan = !completionScanPrimed
  for (const summary of workspace.operationSummaries) {
    if (!isTerminalStatus(summary.status)) continue
    if (settledCompletions.has(summary.operationId)) continue
    settledCompletions.add(summary.operationId)
    if (firstScan) continue
    // 窗口看不见 ⇒ 补一条系统通知；面板关着 ⇒ 记一枚未读角标。
    // 两个条件独立：通知被拒 / 被系统静音时，角标仍然是唯一的线索。
    if (document.hidden) notifyCompletion(summary)
    if (!taskCenter.isOpen) unread.value += 1
  }
  completionScanPrimed = true
}
const markViewed = () => {
  unread.value = 0
}

const retryOperation = async (operation: OperationDetail) => {
  // ⚠️ 先问一句：重试的语义是**新建一个任务**（会再计费一次），不是"把这一次接着跑完"。
  if (!window.confirm(t('task.confirmRetry'))) return
  detailTask.value = undefined
  try {
    await v2.retryOperation(operation.operationId)
  } catch {
    return
  }
  await refresh()
}

const reuseError = ref('')
const reusingRunId = ref<number | null>(null)

/** 这个应用的表单里已经有东西了吗（决定复用参数要不要先问一句"覆盖吗"）。 */
const sessionHasContent = (appId: string) => {
  const session = runnerSessions.get(appId)
  if (!session) return false
  return (
    Object.keys(session.values).length > 0 ||
    Object.keys(session.files).length > 0 ||
    (session.batchRows ?? []).length > 0
  )
}

/**
 * **用这套参数再跑一次（可改）** —— 产品愿景里"保存参数、复用历史"的那一步。
 *
 * 与「重新运行」的区别要说清楚（它们不是一回事，所以不是同一个按钮）：
 * - `operation retry` = **立刻再提交一次**，参数原样、不能改；
 * - 这里 = **打开那个应用的表单、把参数填好**，改不改、跑不跑都由人按 —— **钱永远由人按**。
 *
 * 三处刻意的取舍：
 * 1. **只填当前 schema 认得的字段**：应用重新导入过之后，旧修订留下的键塞进表单没有意义
 *    （而且会被 `create` 的 schema 校验挡下——报错虽清楚，但那是把问题留给用户）；
 * 2. **素材按字段键还原**，`fieldKey` 为空（旧位置编码）的**跳过**：不许猜字段键 ——
 *    猜错就是"把钱花在错素材上"。跳过的那些在表单里是空的，必填的会被提交前的校验拦住；
 * 3. 那个应用的表单里**已经有内容**时先问一句：复用是显式动作，但**不该静默丢掉**用户打了一半的东西。
 */
const reuseRunParams = async (operation: OperationDetail, run: RunRef) => {
  if (reusingRunId.value !== null) return
  const recipeId = run.recipeId
  // 后端查不到这个应用（已被删）时**不做**：宁可没有这个入口，也不要跳到一个 schema 都不一样的表单里。
  if (!recipeId) return
  const appId = String(recipeId)
  if (sessionHasContent(appId) && !window.confirm(t('task.confirmReuseOverwrite'))) return
  reusingRunId.value = run.runId
  reuseError.value = ''
  try {
    const schema = await v2.getRecipeSchema(recipeId)
    // `inputSchema` 在后端是**自由 JSON**（见 `getRecipeSchema` 的类型），这里按表单要用的形状收窄
    // —— 与 `V2Runner` 同一手法，别把它当成强类型契约。
    const properties = (schema.inputSchema as { properties?: Record<string, unknown> } | undefined)
      ?.properties
    const known = new Set(Object.keys(properties ?? {}))
    const values: Record<string, unknown> = {}
    for (const [key, value] of Object.entries(run.inputs ?? {})) {
      if (known.has(key)) values[key] = value
    }
    // 素材：按字段键还原；名字按需取（取不到就用 `素材 #id`，**不编一个文件名**）。
    const files: Record<string, { assetId: number; checksum: string; filename: string }> = {}
    for (const asset of run.assets ?? []) {
      const fieldKey = asset.fieldKey ?? ''
      if (!fieldKey) continue
      let filename = t('task.assetFallbackName', { id: asset.assetId })
      try {
        filename = (await v2.getAsset(asset.assetId)).filename || filename
      } catch {
        // 取名字失败不该让"复用参数"整个失败：提交真正需要的是字段键 + assetId + checksum。
      }
      files[fieldKey] = { assetId: asset.assetId, checksum: asset.checksum, filename }
    }
    // 先写会话再导航：`open` 是所有打开配方的唯一漏斗（它也会记"最近使用"），
    // 而 Runner 是在 `onMounted` 里读会话的，顺序反了就白填。
    runnerSessions.open(appId, operation.recipeName ?? '')
    runnerSessions.update(appId, { values, files })
    detailTask.value = undefined
    batchDetail.value = undefined
    await router.push(`/library/${recipeId}`)
  } catch (error) {
    reuseError.value =
      error instanceof Error
        ? t('task.reuseFailedDetail', { message: error.message })
        : t('task.reuseFailed')
  } finally {
    reusingRunId.value = null
  }
}

/**
 * 这一批里**失败**的那些 run。
 *
 * 只算 `FAILED`：`CANCELLED` 是**人主动停的**，把它一起重跑多半不是本意（而且那是花钱的动作）。
 */
const failedRuns = (operation: OperationDetail) =>
  operation.runs.filter((run) => run.status === 'FAILED')

/**
 * 排队位置：这条 run 前面还有几条也在排。
 *
 * 为什么由界面自己算：一个批次的顺序就是 Operation 里的 `sequence`，数据已经在手上。
 * 让后端多吐一个"队列位置"字段，等于多维护一份迟早会漂的派生值。
 */
const queuedAhead = (operation: OperationDetail, run: RunRef): number =>
  run.status === 'QUEUED'
    ? operation.runs.filter((other) => other.status === 'QUEUED' && other.sequence < run.sequence)
        .length
    : 0

const retryingFailed = ref(false)
const retryFailedError = ref('')

/**
 * **只重试失败的那几条**（愿景把"失败项单独重试"明确列为批量核心能力）。
 *
 * ⚠️ 它和「重新运行」不是一回事：`operation retry` 的语义是**新建任务**，而后端的
 * `.../runs/{runId}/retry` 是**一次调用建一个 operation**（它按那条 run 复用已存的 inputs 与
 * assets）。所以 N 条失败 = **N 个新任务、再计费 N 次** —— 确认框必须这么说，不能含糊。
 *
 * 为什么不在这里自己拼一次"多 run 的提交"：界面**拿不到** run 的 inputs/assets
 * （`RunRef` 里没有这两项，也不该有），拼出来的东西一旦错了就是**把钱花在错参数上**。
 * 那种错不该由界面来犯 —— 让后端用它自己存的那份。
 */
const retryFailedRuns = async (operation: OperationDetail) => {
  if (retryingFailed.value) return
  const failed = failedRuns(operation)
  if (!failed.length) return
  if (!window.confirm(t('task.confirmRetryFailed', { count: failed.length }))) return
  retryingFailed.value = true
  retryFailedError.value = ''
  try {
    // 逐条调用：每一条都返回**一个新的 operationId**（所以这里不需要、也不该合并成一次请求）。
    for (const run of failed) {
      await v2.retryRun(operation.operationId, run.runId)
    }
    batchDetail.value = undefined
  } catch (error) {
    // 不静默：用户点的是一个**会花钱**的动作，失败必须说出来（与取消那条不同——
    // 取消失败可以靠下一轮轮询恢复，重试失败则是"什么也没发生"）。
    retryFailedError.value =
      error instanceof Error
        ? t('task.retryFailedDetail', { message: error.message })
        : t('task.retryFailed')
  } finally {
    retryingFailed.value = false
  }
  await refresh()
}
const detailTime = (value: string) => {
  const timestamp = timestampOf(value)
  if (Number.isNaN(timestamp)) return ''
  return new Intl.DateTimeFormat(dateLocale.value, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit'
  }).format(new Date(timestamp))
}
const runTrackingLabel = (tracking: string) =>
  ({
    inactive: t('task.reconcileInactive'),
    active: t('task.reconcileActive'),
    paused: t('task.reconcilePaused'),
    exhausted: t('task.reconcileExhausted')
  })[tracking] ?? tracking

const closeOnMobile = () => {
  viewportWidth.value = window.innerWidth
  if (isOverlay.value) taskCenter.isOpen = false
}

onMounted(async () => {
  const succeeded = await workspace.loadOperationSummaries(200)
  if (succeeded) {
    lastRefreshedAt.value = new Date()
    // 同上：只有真读到列表才建立"历史基线"。
    scanCompletions()
  }
  refreshTimer = window.setInterval(() => {
    void refresh()
  }, 5000)
  clockTimer = window.setInterval(() => {
    now.value = Date.now()
  }, 10_000)
  window.addEventListener('resize', closeOnMobile)
  window.addEventListener('keydown', onKeydown)
})

onBeforeUnmount(() => {
  if (refreshTimer) window.clearInterval(refreshTimer)
  if (clockTimer) window.clearInterval(clockTimer)
  window.removeEventListener('resize', closeOnMobile)
  window.removeEventListener('keydown', onKeydown)
  // 兜底：正常路径由 `releaseArtifactsOutside` 随窗口滑动释放；这里管卸载那一次。
  for (const url of Object.values(artifactUrls.value)) URL.revokeObjectURL(url)
  artifactUrls.value = {}
})
</script>

<template>
  <div class="task-center-shell" :class="taskCenter.isOpen ? 'is-open' : 'is-closed'">
    <div
      v-if="taskCenter.isOpen && isOverlay"
      class="task-drawer-backdrop"
      aria-hidden="true"
      @click="closeDrawer"
    />
    <IconButton
      ref="taskToggle"
      class="task-toggle rounded-none"
      :label="taskCenter.isOpen ? t('task.collapse') : t('task.expand')"
      variant="outline"
      :aria-expanded="taskCenter.isOpen"
      :aria-controls="drawerId"
      @click="toggleDrawer"
    >
      <span class="task-toggle-icon">
        <PanelRightClose v-if="taskCenter.isOpen" class="size-4" />
        <PanelRightOpen v-else class="size-4" />
        <span
          v-if="unread && !taskCenter.isOpen"
          class="task-badge"
          :aria-label="t('task.unreadCount')"
          >{{ unread }}</span
        >
      </span>
    </IconButton>
    <aside
      :id="drawerId"
      ref="drawerPanel"
      class="task-center glass-hairline-l"
      :class="taskCenter.isOpen ? 'task-center-open' : 'task-center-closed'"
      :aria-label="t('task.liveActivity')"
      :aria-hidden="isOverlay && !taskCenter.isOpen ? 'true' : undefined"
      :inert="isOverlay && !taskCenter.isOpen ? true : undefined"
      :role="isOverlay ? 'dialog' : undefined"
      :tabindex="isOverlay && taskCenter.isOpen ? -1 : undefined"
    >
      <template v-if="taskCenter.isOpen">
        <header class="task-center-header">
          <div class="task-heading-row">
            <SectionHeader
              :eyebrow="t('task.eyebrow')"
              :title="t('task.liveActivity')"
              class="flex-1"
            />
            <div class="task-header-actions">
              <RouterLink to="/tasks" class="task-history-link" @click="closeDrawer">
                {{ t('task.allHistory') }}
              </RouterLink>
              <IconButton
                :label="isRefreshing ? t('task.refreshing') : t('task.refresh')"
                size="sm"
                variant="outline"
                :disabled="isRefreshing"
                @click="refresh"
              >
                <LoaderCircle v-if="isRefreshing" class="size-3.5 animate-spin" />
                <RefreshCw v-else class="size-3.5" />
              </IconButton>
              <IconButton
                class="task-mobile-close"
                :label="t('task.close')"
                size="sm"
                variant="outline"
                @click="closeDrawer"
              >
                <X class="size-4" />
              </IconButton>
            </div>
          </div>
          <div class="sync-summary">
            <StatusIndicator
              :status="hasError ? 'error' : 'success'"
              :label="
                hasError
                  ? t('task.syncError')
                  : isRefreshing || activeCount
                    ? t('task.autoSync')
                    : t('task.synced')
              "
            />
            <span class="active-count">
              <span class="active-count-dot" />{{ t('task.activeCount', { count: activeCount }) }}
            </span>
            <span v-if="lastRefreshedAt" class="sync-time">{{
              t('task.updatedAt', { time: relativeTime(lastRefreshedAt.toISOString()) })
            }}</span>
          </div>
          <div class="task-view-tabs" role="tablist" :aria-label="t('task.viewTabs')">
            <button
              type="button"
              role="tab"
              :aria-selected="taskView === 'tasks'"
              :class="{ active: taskView === 'tasks' }"
              @click="taskView = 'tasks'"
            >
              {{ t('task.tabTasks') }}
            </button>
            <button
              type="button"
              role="tab"
              :aria-selected="taskView === 'batches'"
              :class="{ active: taskView === 'batches' }"
              @click="taskView = 'batches'"
            >
              {{ t('task.tabBatches') }}
            </button>
          </div>
        </header>

        <!-- 任务视图 -->
        <div
          v-if="taskView === 'tasks'"
          class="task-stream"
          aria-live="polite"
          aria-relevant="additions text"
        >
          <div class="sr-only" role="status" aria-live="polite">
            {{ t('task.activeCount', { count: activeCount }) }}
          </div>

          <!-- 进行中的批次 -->
          <Card
            v-for="operation in activeBatchOperations"
            :key="`batch-${operation.operationId}`"
            class="task-flow-card task-active"
          >
            <div class="task-media task-media-active" aria-hidden="true">
              <div class="task-media-grid" />
              <Send class="task-media-icon size-5" /><span class="task-media-label">{{
                t('task.batchInProgress')
              }}</span>
            </div>
            <div class="task-flow-content">
              <div class="task-card-topline">
                <StatusBadge :status="statusTone(operation.status)">{{
                  statusLabel(operation.status)
                }}</StatusBadge>
                <time :datetime="operationDatetime(operation)" class="task-card-time">{{
                  relativeTime(operation.createdAt)
                }}</time>
              </div>
              <p class="task-title">{{ operationLabel(operation) }}</p>
              <p class="task-meta">{{ t('task.batchMeta', { count: operation.runs.length }) }}</p>
              <div class="task-progress">
                <span>{{ batchRunSummary(operation) }}</span
                ><span class="task-progress-track"><i /></span>
              </div>
              <div class="task-actions">
                <button
                  type="button"
                  class="task-retry-link"
                  :disabled="cancellingId !== null"
                  @click="cancelOperation(operation)"
                >
                  {{
                    cancellingId === operation.operationId
                      ? t('task.cancelling')
                      : t('task.cancelBatch')
                  }}
                </button>
                <IconButton
                  :label="t('task.openDetails')"
                  size="sm"
                  @click="openTaskDetail(operation)"
                  ><FileText class="size-3.5"
                /></IconButton>
                <button type="button" class="task-retry-link" @click="taskView = 'batches'">
                  {{ t('task.viewBatch') }}
                </button>
              </div>
            </div>
          </Card>

          <!-- 进行中的单任务 -->
          <Card
            v-for="operation in activeSingleOperations"
            :key="operation.operationId"
            class="task-flow-card task-active"
          >
            <div
              class="task-media task-media-active"
              :class="`task-media-${mediaTone(operation.status)}`"
              aria-hidden="true"
            >
              <div class="task-media-grid" />
              <LoaderCircle
                v-if="mediaKind(operation.status) === 'loading'"
                class="task-media-icon size-5 animate-spin"
              /><Clock3
                v-else-if="mediaKind(operation.status) === 'queued'"
                class="task-media-icon size-5"
              /><Send v-else class="task-media-icon size-5" />
              <span class="task-media-label">{{ activeMediaLabel(operation.status) }}</span>
            </div>
            <div class="task-flow-content">
              <div class="task-card-topline">
                <StatusBadge :status="statusBadgeStatus(operation.status)">{{
                  statusLabel(operation.status)
                }}</StatusBadge
                ><time :datetime="operationDatetime(operation)" class="task-card-time">{{
                  activeDuration(operation)
                }}</time>
              </div>
              <p class="task-title">{{ operationLabel(operation) }}</p>
              <p class="task-meta" :title="operation.recipeName || undefined">
                {{ taskAppIdentity(operation) }}
              </p>
              <p class="task-meta">{{ t('task.taskMeta', { id: shortTaskId(operation) }) }}</p>
              <div class="task-progress">
                <span>{{ activeTrackingLabel(operation.status) }}</span
                ><span class="task-progress-track"><i /></span>
              </div>
              <div class="task-actions">
                <button
                  type="button"
                  class="task-retry-link"
                  :disabled="cancellingId !== null"
                  @click="cancelOperation(operation)"
                >
                  {{
                    cancellingId === operation.operationId ? t('task.cancelling') : t('task.cancel')
                  }}
                </button>
                <IconButton
                  :label="t('task.openDetails')"
                  size="sm"
                  @click="openTaskDetail(operation)"
                  ><FileText class="size-3.5"
                /></IconButton>
              </div>
            </div>
          </Card>

          <p v-if="!activeOperations.length" class="task-realtime-empty">
            {{ t('task.noActive') }}
          </p>

          <!-- 最近任务画廊 -->
          <section v-if="galleryOperations.length" class="task-results-section">
            <div class="task-results-heading">
              <p>{{ t('task.recentTasks') }}</p>
              <RouterLink to="/tasks" @click="closeDrawer">{{ t('task.allHistory') }}</RouterLink>
            </div>
            <div class="task-result-grid">
              <Card
                v-for="operation in galleryOperations"
                :key="operation.operationId"
                class="task-result-card"
                :class="isGalleryFailure(operation) ? 'task-result-card-failed' : undefined"
              >
                <button
                  type="button"
                  class="task-result-media"
                  :title="
                    isGalleryFailure(operation)
                      ? t('task.viewDetail')
                      : t('task.previewNamed', { name: operationLabel(operation) })
                  "
                  :disabled="!isGalleryFailure(operation) && !previewResults(operation).length"
                  @click="
                    isGalleryFailure(operation) ? openTaskDetail(operation) : openPreview(operation)
                  "
                >
                  <div v-if="isGalleryFailure(operation)" class="task-result-failed">
                    <CircleAlert class="size-6" /><span>{{
                      operation.status === 'CANCELLED' ? t('status.cancelled') : t('status.failed')
                    }}</span>
                  </div>
                  <img
                    v-else-if="galleryPreview(operation)?.kind === 'image'"
                    :src="galleryPreview(operation)!.url"
                    :alt="operationLabel(operation)"
                  />
                  <video
                    v-else-if="galleryPreview(operation)?.kind === 'video'"
                    :src="galleryPreview(operation)!.url"
                    :aria-label="t('task.previewNamed', { name: operationLabel(operation) })"
                    muted
                    playsinline
                    preload="metadata"
                    @mouseenter="playVideoPreview"
                    @mouseleave="resetVideoPreview"
                    @focus="playVideoPreview"
                    @blur="resetVideoPreview"
                  />
                  <LoaderCircle v-else-if="galleryPreview(operation)" class="size-5 animate-spin" />
                  <div v-else class="grid size-full place-items-center">
                    <FileText class="size-5" />
                  </div>
                  <span
                    v-if="galleryPreview(operation)?.kind === 'video'"
                    class="video-play-overlay"
                    aria-hidden="true"
                    ><Play class="size-5"
                  /></span>
                  <span v-if="galleryResultCount(operation) > 1" class="result-count">{{
                    t('task.resultCount', { count: galleryResultCount(operation) })
                  }}</span>
                </button>
                <div class="task-result-body">
                  <p class="task-result-title">{{ operationLabel(operation) }}</p>
                  <p class="task-result-meta">
                    #{{ operation.operationId }} · {{ operationSummary(operation) }}
                  </p>
                  <p class="task-result-meta" :title="completionTimeTitle(operation)">
                    {{ completionTime(operation) }}
                  </p>
                  <div class="task-actions">
                    <IconButton
                      :label="t('task.downloadAll')"
                      size="sm"
                      @click="downloadOperationArtifacts(operation)"
                      ><Download class="size-3.5"
                    /></IconButton>
                    <IconButton
                      v-if="isGalleryFailure(operation)"
                      :label="t('task.rerun')"
                      size="sm"
                      @click="retryOperation(operation)"
                      ><RotateCcw class="size-3.5"
                    /></IconButton>
                    <IconButton
                      :label="t('task.details')"
                      size="sm"
                      @click="openTaskDetail(operation)"
                      ><FileText class="size-3.5"
                    /></IconButton>
                  </div>
                </div>
              </Card>
            </div>
          </section>

          <EmptyState
            v-if="!activeOperations.length && !taskOperations.length"
            class="task-empty"
            :title="t('task.empty')"
            :description="''"
          >
            <template #icon><Clock3 class="size-5" /></template>
            <template #actions>
              <Button variant="outline" size="sm" @click="goToLibrary()">
                {{ t('home.goToApps') }}
              </Button>
            </template>
          </EmptyState>
        </div>

        <!-- 批次视图 -->
        <div v-else class="task-stream" aria-live="polite" aria-relevant="additions text">
          <p v-if="!batchOperations.length" class="task-realtime-empty">{{ t('task.noBatch') }}</p>
          <template v-for="(operation, index) in batchOperations" :key="operation.operationId">
            <p v-if="index === 0" class="batch-history-heading">{{ t('task.recentBatches') }}</p>
            <p v-if="index === 5" class="batch-history-heading">{{ t('task.olderBatches') }}</p>
            <Card class="batch-history-card">
              <div class="task-card-topline">
                <StatusBadge :status="batchRunBadgeStatus(operation)">{{
                  batchRunLabel(operation)
                }}</StatusBadge>
                <time :datetime="operationDatetime(operation)" class="task-card-time">{{
                  operationTime(operation)
                }}</time>
              </div>
              <p class="task-title">{{ operationLabel(operation) }}</p>
              <p class="task-meta">{{ t('task.taskCount', { count: operation.runs.length }) }}</p>
              <p class="batch-history-summary">{{ batchRunSummary(operation) }}</p>
              <div class="task-actions">
                <IconButton
                  :label="t('task.viewBatchDetail')"
                  size="sm"
                  @click="batchDetail = operation"
                  ><FileText class="size-3.5"
                /></IconButton>
              </div>
            </Card>
          </template>
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

        <Dialog
          v-if="detailTask"
          :open="true"
          :title="t('task.detailTitle', { name: operationLabel(detailTask) })"
          class="max-w-2xl"
          @update:open="!$event && (detailTask = undefined)"
        >
          <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm">
            <dt class="text-muted-foreground">{{ t('task.fieldApp') }}</dt>
            <dd class="break-all">{{ operationLabel(detailTask) }}</dd>
            <dt class="text-muted-foreground">{{ t('task.fieldStatus') }}</dt>
            <dd>
              <StatusBadge :status="statusTone(detailTask.status)">{{
                statusLabel(detailTask.status)
              }}</StatusBadge>
            </dd>
            <dt class="text-muted-foreground">{{ t('task.fieldTaskId') }}</dt>
            <dd>#{{ detailTask.operationId }}</dd>
            <dt class="text-muted-foreground">{{ t('task.fieldStartedAt') }}</dt>
            <dd>{{ detailTime(detailTask.createdAt) }}</dd>
            <dt class="text-muted-foreground">{{ t('task.fieldDuration') }}</dt>
            <dd>{{ operationDuration(detailTask) }}</dd>
            <dt class="text-muted-foreground">{{ t('task.fieldResults') }}</dt>
            <dd>{{ t('task.resultCount', { count: galleryResultCount(detailTask) }) }}</dd>
          </dl>
          <p class="mt-5 text-xs font-semibold text-muted-foreground">
            {{ detailTask.runs.length }} {{ t('task.runs') }}
          </p>
          <!--
            「它其实没在动」：这一块是**唯一**能把"进行中"与"真的在跑"分开的东西。
            文案由界面按**原因码**取（后端那两句是中文原文，给 CLI/MCP/Agent 用的），
            所以英文界面里不会蹦出中文。
          -->
          <div
            v-if="detailTask.attention"
            class="mt-4 space-y-1.5 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs"
          >
            <p class="flex items-center gap-1.5 font-semibold text-amber-700 dark:text-amber-400">
              <TriangleAlert class="size-3.5 shrink-0" aria-hidden="true" />
              {{ t('attention.title') }} · {{ attentionReasonLabel(detailTask.attention.reason) }}
            </p>
            <p class="text-amber-800/90 dark:text-amber-200/90">
              {{
                attentionExplanation(detailTask.attention.reason, !!detailTask.attention.deadline)
              }}
            </p>
            <p class="text-amber-800/80 dark:text-amber-200/80">
              {{
                detailTask.attention.deadline
                  ? t('attention.deadline', { time: detailTime(detailTask.attention.deadline) })
                  : t('attention.noDeadline')
              }}
            </p>
          </div>
          <div v-if="detailTask.runs.length" class="mt-2 space-y-1.5">
            <div
              v-for="run in detailTask.runs"
              :key="run.runId"
              class="flex items-center justify-between gap-3 rounded-md bg-secondary/60 px-3 py-2 text-xs"
            >
              <span class="min-w-0 truncate font-mono">{{
                run.clientRunId || `run#${run.sequence + 1}`
              }}</span>
              <span
                v-if="run.connectionName"
                class="shrink-0 rounded bg-background/60 px-1.5 py-0.5 text-[11px] text-muted-foreground"
                :title="run.connectionName"
                >{{ run.connectionName }}</span
              >
              <span
                v-if="queuedAhead(detailTask, run)"
                class="shrink-0 text-[11px] text-muted-foreground"
                >{{ t('task.queuedAhead', { count: queuedAhead(detailTask, run) }) }}</span
              >
              <span class="shrink-0 text-muted-foreground">{{
                runTrackingLabel(run.trackingState)
              }}</span>
              <!-- 哪一条 run 在等（批量里可能只有第 3 条不确定，其余都正常）。 -->
              <StatusBadge v-if="run.attention" :status="attentionTone(run.attention.reason)">{{
                attentionReasonLabel(run.attention.reason)
              }}</StatusBadge>
              <StatusBadge :status="statusTone(run.status)">{{
                statusLabel(run.status)
              }}</StatusBadge>
              <!--
                「用这套参数再跑一次（可改）」：打开那个应用的表单并预填这次提交的参数。
                与旁边的「重新运行」**不是一回事**（那个是立刻再投一次、参数不能改），
                所以是两个入口而不是一个开关。
              -->
              <IconButton
                v-if="run.recipeId"
                :label="t('task.reuseParams')"
                size="sm"
                :disabled="reusingRunId === run.runId"
                @click="reuseRunParams(detailTask, run)"
                ><Wand2 class="size-3.5"
              /></IconButton>
            </div>
          </div>
          <p v-else class="mt-2 text-sm text-muted-foreground">{{ t('task.noRunsRecorded') }}</p>
          <p v-if="reuseError" class="mt-3 text-xs text-destructive">{{ reuseError }}</p>
          <div
            v-if="detailReasons.length"
            class="mt-4 space-y-1.5 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs"
          >
            <p class="font-semibold text-destructive">{{ t('task.failureReasons') }}</p>
            <p
              v-for="item in detailReasons"
              :key="`${item.runId}-${item.reason}`"
              class="flex items-baseline gap-2 text-destructive/90"
            >
              <code
                v-if="item.code"
                class="shrink-0 rounded bg-destructive/10 px-1 py-0.5 font-mono text-[10px]"
                >{{ item.code }}</code
              >
              <span>{{ item.reason }}</span>
            </p>
          </div>
          <template #footer>
            <Button type="button" variant="outline" @click="detailTask = undefined">{{
              t('common.close')
            }}</Button>
            <RouterLink to="/tasks"
              ><Button type="button">{{ t('task.allHistory') }}</Button></RouterLink
            >
          </template>
        </Dialog>

        <Dialog
          v-if="batchDetail"
          :open="true"
          :title="t('task.batchDetailTitle', { name: operationLabel(batchDetail) })"
          class="max-w-2xl"
          @update:open="!$event && (batchDetail = undefined)"
        >
          <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm">
            <dt class="text-muted-foreground">{{ t('task.fieldBatchStatus') }}</dt>
            <dd>{{ batchRunLabel(batchDetail) }}</dd>
            <dt class="text-muted-foreground">{{ t('task.fieldStartedAt') }}</dt>
            <dd>{{ detailTime(batchDetail.createdAt) }}</dd>
            <dt class="text-muted-foreground">{{ t('task.fieldTaskCount') }}</dt>
            <dd>{{ batchDetail.runs.length }}</dd>
          </dl>
          <p class="mt-5 text-xs font-semibold text-muted-foreground">
            {{ t('task.fieldRunStatus') }}
          </p>
          <div v-if="batchDetail.runs.length" class="mt-2 space-y-1.5">
            <div
              v-for="run in batchDetail.runs"
              :key="run.runId"
              class="flex items-center justify-between gap-3 rounded-md bg-secondary/60 px-3 py-2 text-xs"
            >
              <span class="min-w-0 truncate font-mono">{{
                run.clientRunId || `run#${run.sequence + 1}`
              }}</span>
              <span
                v-if="run.connectionName"
                class="shrink-0 rounded bg-background/60 px-1.5 py-0.5 text-[11px] text-muted-foreground"
                :title="run.connectionName"
                >{{ run.connectionName }}</span
              >
              <!-- 排队中也得有个位置感：否则用户只看到一排"排队中"，不知道还要等多久。 -->
              <span
                v-if="queuedAhead(batchDetail, run)"
                class="shrink-0 text-[11px] text-muted-foreground"
                >{{ t('task.queuedAhead', { count: queuedAhead(batchDetail, run) }) }}</span
              >
              <StatusBadge :status="statusTone(run.status)">{{
                statusLabel(run.status)
              }}</StatusBadge>
              <!--
                「用这套参数再跑一次（可改）」：打开那个应用的表单并预填这次提交的参数。
                与旁边的「重新运行」**不是一回事**（那个是立刻再投一次、参数不能改），
                所以是两个入口而不是一个开关。
              -->
              <IconButton
                v-if="run.recipeId"
                :label="t('task.reuseParams')"
                size="sm"
                :disabled="reusingRunId === run.runId"
                @click="reuseRunParams(batchDetail, run)"
                ><Wand2 class="size-3.5"
              /></IconButton>
            </div>
          </div>
          <p v-if="retryFailedError" class="mt-3 text-xs text-destructive">
            {{ retryFailedError }}
          </p>
          <p v-if="reuseError" class="mt-3 text-xs text-destructive">{{ reuseError }}</p>
          <template #footer>
            <!--
              「只重试失败的 N 条」：批量里失败了几条时，**别整批重跑**——那会把已经成功的那几条
              也再提交、再计费一遍。这条路径后端与 CLI/MCP 早就有（`operation retry --run`），
              界面此前只有"整批重跑"这一个选择。
            -->
            <Button
              v-if="failedRuns(batchDetail).length"
              type="button"
              variant="outline"
              :disabled="retryingFailed"
              @click="retryFailedRuns(batchDetail)"
              >{{
                retryingFailed
                  ? t('task.retryingFailed')
                  : t('task.retryFailedRuns', { count: failedRuns(batchDetail).length })
              }}</Button
            >
            <Button
              type="button"
              variant="outline"
              @click="batchDetail && downloadOperationArtifacts(batchDetail)"
              >{{ t('task.downloadAll') }}</Button
            >
            <Button type="button" variant="outline" @click="batchDetail = undefined">{{
              t('common.close')
            }}</Button>
          </template>
        </Dialog>
      </template>
    </aside>
  </div>

  <button
    v-if="activeCount && !taskCenter.isOpen"
    class="mobile-task-bar"
    type="button"
    :aria-controls="drawerId"
    :aria-expanded="taskCenter.isOpen"
    :aria-label="t('task.expand')"
    @click="toggleDrawer"
  >
    <span class="mobile-task-pulse" />
    <span class="min-w-0 flex-1 text-left">
      <strong>{{ t('shell.runningTasks', { count: activeCount }) }}</strong>
      <small>{{ t('task.liveActivity') }}</small>
    </span>
    <PanelRightOpen class="size-4 shrink-0" />
  </button>
</template>

<style scoped>
.task-center-shell {
  position: fixed;
  inset: 4rem 0 0 auto;
  width: var(--task-rail-width);
  z-index: 35;
  pointer-events: none;
}
.task-center-shell > * {
  pointer-events: auto;
}
.task-center {
  position: absolute;
  inset: 0;
  display: flex;
  width: var(--task-rail-width);
  flex-direction: column;
  /* 左边分隔线由 .glass-hairline-l 提供；表面实色，不磨砂（见 style.css 的光学预算） */
  background: var(--glass-chrome-bg);
  box-shadow: var(--glass-shadow-drawer);
  transition: transform 180ms ease;
}
/*
 * 收起点：平移量 = 面板宽度 − 留在视口内的窄条宽度。
 * 这两个值都来自 style.css 的 --task-rail-*，与 AppShell 的让位量同源 ——
 * 别再在这里写死 276 / 320（改一处漏一处就是内容被压住的经典事故）。
 */
.task-center-closed {
  transform: translateX(calc(var(--task-rail-width) - var(--task-rail-peek)));
}
.task-toggle {
  position: absolute;
  top: 1rem;
  /* 展开时贴在面板左缘外扩一点（把手宽度 + 1px 缝） */
  left: calc(-1 * (var(--task-rail-handle) + 1px));
  z-index: 1;
  border-right: 0;
}
.task-center-header {
  border-bottom: 1px solid var(--app-border);
  padding: 0.875rem 1rem 0.75rem;
}
.task-heading-row,
.task-header-actions,
.sync-summary,
.task-card-topline,
.task-actions,
.task-results-heading {
  display: flex;
  align-items: center;
}
.task-heading-row {
  justify-content: space-between;
  gap: 0.5rem;
}
.task-header-actions {
  gap: 0.25rem;
}
.task-history-link {
  padding: 0.375rem 0.25rem;
  color: var(--app-muted-foreground);
  font-size: 0.6875rem;
  white-space: nowrap;
}
.task-history-link:hover,
.task-retry-link:hover {
  color: var(--app-foreground);
}
.task-mobile-close {
  display: none;
}
.sync-summary {
  flex-wrap: wrap;
  gap: 0.4rem;
  margin-top: 0.5rem;
  color: var(--app-muted-foreground);
  font-size: 0.6875rem;
}
.sync-summary :deep(.status-indicator) {
  font-size: 0.6875rem;
}
.active-count {
  display: inline-flex;
  align-items: center;
  gap: 0.25rem;
  font-variant-numeric: tabular-nums;
}
.active-count-dot {
  width: 0.375rem;
  height: 0.375rem;
  border-radius: 999px;
  background: var(--app-info);
  box-shadow: 0 0 0 0.2rem color-mix(in srgb, var(--app-info) 13%, transparent);
}
.sync-time {
  margin-left: auto;
}
/*
 * 分段控件：单一选中信号 —— 选中项是"浮起来的一枚玻璃片"，未选中项完全透明。
 * 旧写法同时用了填充色变化 + 分隔线 + 焦点环，三个信号互相打架（见 review 记录）。
 */
.task-view-tabs {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 2px;
  margin-top: 0.75rem;
  border: 1px solid var(--glass-panel-edge);
  border-radius: var(--radius-md);
  background: var(--glass-track-bg);
  padding: 2px;
}
.task-view-tabs button {
  min-height: 1.75rem;
  border-radius: calc(var(--radius-md) - 3px);
  background: transparent;
  color: var(--muted-foreground);
  font-size: 0.75rem;
}
.task-view-tabs button.active {
  background: var(--glass-panel-bg);
  box-shadow: var(--glass-panel-shadow);
  color: var(--foreground);
  font-weight: 600;
}
.task-stream {
  position: relative;
  min-height: 0;
  flex: 1;
  overflow-y: auto;
  padding: 0.75rem 1rem 1.5rem;
}
.task-realtime-empty {
  margin: 0;
  padding: 0.75rem 0;
  color: var(--app-muted-foreground);
  font-size: 0.75rem;
}
.task-flow-card {
  position: relative;
  display: grid;
  grid-template-columns: 5.25rem minmax(0, 1fr);
  gap: 0.75rem;
  margin-bottom: 0.75rem;
  padding: 0.5rem;
  border-radius: var(--radius-md);
  background: color-mix(in srgb, var(--app-card) 92%, var(--app-secondary));
  box-shadow: none;
  transition:
    border-color 180ms ease,
    transform 180ms ease,
    box-shadow 180ms ease;
}
.task-flow-card:hover {
  border-color: color-mix(in srgb, var(--app-primary) 28%, var(--app-border));
  box-shadow: var(--shadow-xs);
}
.task-flow-card:last-of-type {
  margin-bottom: 0;
}
.task-active {
  border-color: color-mix(in srgb, var(--app-info) 32%, var(--app-border));
}
.task-active .task-media {
  border-color: color-mix(in srgb, var(--app-info) 38%, var(--app-border));
}
.task-media {
  position: relative;
  display: grid;
  min-height: 5.25rem;
  width: 5.25rem;
  place-items: center;
  overflow: hidden;
  border: 1px solid var(--app-border);
  background: var(--app-secondary);
  color: var(--app-muted-foreground);
}
.task-media-active {
  isolation: isolate;
  background: color-mix(in srgb, var(--app-info) 7%, var(--app-secondary));
  color: var(--app-info);
}
.task-media-grid {
  position: absolute;
  inset: 0;
  z-index: -1;
  opacity: 0.75;
  background-image:
    linear-gradient(90deg, color-mix(in srgb, currentColor 15%, transparent) 1px, transparent 1px),
    linear-gradient(color-mix(in srgb, currentColor 15%, transparent) 1px, transparent 1px);
  background-size: 12px 12px;
}
.task-media-running .task-media-grid {
  animation: media-grid 2.4s linear infinite;
}
.task-media-label {
  position: absolute;
  right: 0.35rem;
  bottom: 0.3rem;
  left: 0.35rem;
  text-align: center;
  font-size: 0.5625rem;
  font-weight: 600;
  color: var(--app-muted-foreground);
}
.task-flow-content {
  min-width: 0;
  padding: 0.1rem 0;
}
.task-card-topline {
  justify-content: space-between;
  gap: 0.5rem;
}
.task-card-topline :deep(.inline-flex) {
  padding: 0.1875rem 0.375rem;
  font-size: 0.625rem;
}
.task-card-time {
  flex: none;
  color: var(--app-muted-foreground);
  font-size: 0.625rem;
  font-variant-numeric: tabular-nums;
}
.task-title {
  margin: 0.45rem 0 0;
  overflow: hidden;
  font-size: 0.8125rem;
  font-weight: 600;
  line-height: 1.3;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.task-meta {
  margin: 0.25rem 0 0;
  overflow: hidden;
  color: var(--app-muted-foreground);
  font-size: 0.6875rem;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.task-progress {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  margin-top: 0.65rem;
  color: var(--app-muted-foreground);
  font-size: 0.625rem;
}
.task-progress-track {
  display: block;
  height: 2px;
  min-width: 1.5rem;
  flex: 1;
  overflow: hidden;
  background: var(--app-border);
}
.task-progress-track i {
  display: block;
  width: 45%;
  height: 100%;
  background: var(--app-info);
  animation: task-progress 1.4s ease-in-out infinite;
}
.task-actions {
  gap: 0.25rem;
  margin-top: 0.6rem;
}
.task-actions :deep(button:not(.task-retry-link)) {
  flex: none;
  height: 1.75rem;
  width: 1.75rem;
}
.task-retry-link {
  color: var(--app-muted-foreground);
  font-size: 0.6875rem;
  text-decoration: underline;
  text-underline-offset: 2px;
}
.task-retry-link:disabled {
  opacity: 0.6;
  cursor: default;
}
.task-results-section {
  margin-top: 1.25rem;
  border-top: 0;
  border-radius: var(--radius-md);
  background: color-mix(in srgb, var(--app-card) 72%, var(--app-secondary));
  padding: 0.875rem;
}
.task-results-heading {
  justify-content: space-between;
  gap: 0.75rem;
  font-size: 0.75rem;
  font-weight: 600;
}
.task-results-heading p {
  display: inline-flex;
  align-items: center;
  gap: 0.4rem;
}
.task-results-heading p::before {
  width: 0.375rem;
  height: 0.375rem;
  border-radius: 999px;
  background: var(--app-info);
  box-shadow: 0 0 0 0.2rem color-mix(in srgb, var(--app-info) 14%, transparent);
  content: '';
}
.task-results-heading a {
  color: var(--app-muted-foreground);
  font-size: 0.6875rem;
  font-weight: 500;
}
.task-results-heading a:hover {
  color: var(--app-foreground);
}
.task-result-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0.5rem;
  margin-top: 0.75rem;
}
.task-result-card {
  min-width: 0;
  overflow: hidden;
  border-radius: var(--radius-md);
  box-shadow: none;
  transition:
    border-color 150ms ease,
    box-shadow 150ms ease;
}
.task-result-card:hover {
  border-color: color-mix(in srgb, var(--app-info) 45%, var(--app-border));
  box-shadow: var(--shadow-sm);
}
.task-result-card-failed {
  border-color: color-mix(in srgb, var(--app-destructive) 34%, var(--app-border));
}
.task-result-media {
  position: relative;
  display: grid;
  width: 100%;
  aspect-ratio: 3 / 4;
  place-items: center;
  overflow: hidden;
  border-bottom: 1px solid var(--app-border);
  background: var(--app-secondary);
  color: var(--app-muted-foreground);
  padding: 0;
  text-align: inherit;
}
.task-result-media:disabled {
  cursor: default;
}
.task-result-media img,
.task-result-media video {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  object-fit: cover;
  transition: transform 180ms ease;
}
.task-result-media:hover img,
.task-result-media:focus-visible img {
  transform: scale(1.04);
}
.task-result-failed {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 0.375rem;
  width: 100%;
  height: 100%;
  background: color-mix(in srgb, var(--app-destructive) 8%, var(--app-secondary));
  color: var(--app-destructive);
  font-size: 0.6875rem;
  font-weight: 600;
}
.task-result-body {
  padding: 0.5rem;
}
.task-result-title {
  display: -webkit-box;
  overflow: hidden;
  min-height: 2.35em;
  margin: 0;
  font-size: 0.6875rem;
  font-weight: 600;
  line-height: 1.2;
  -webkit-box-orient: vertical;
  -webkit-line-clamp: 2;
}
.task-result-meta {
  margin: 0.25rem 0 0;
  overflow: hidden;
  color: var(--app-muted-foreground);
  font-size: 0.625rem;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.task-result-body .task-actions {
  flex-wrap: wrap;
  margin-top: 0.4rem;
}
.result-count {
  position: absolute;
  top: 0.35rem;
  right: 0.35rem;
  padding: 0.125rem 0.25rem;
  background: color-mix(in srgb, var(--app-background) 83%, transparent);
  color: var(--app-foreground);
  font-size: 0.5625rem;
  backdrop-filter: blur(4px);
}
.video-play-overlay {
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
  color: white;
  pointer-events: none;
  transform: translate(-50%, -50%);
  transition:
    opacity 160ms ease,
    transform 160ms ease;
}
.task-result-media:hover .video-play-overlay,
.task-result-media:focus-visible .video-play-overlay {
  opacity: 0;
  transform: translate(-50%, -50%) scale(0.9);
}
.batch-history-heading {
  margin: 0.875rem 0 0.25rem;
  color: var(--app-muted-foreground);
  font-size: 0.75rem;
  font-weight: 600;
}
.batch-history-card {
  padding: 0.875rem;
}
.batch-history-summary {
  margin-top: 0.625rem;
  color: var(--app-muted-foreground);
  font-size: 0.75rem;
}
.task-toggle-icon {
  position: relative;
  display: inline-grid;
  place-items: center;
}
.task-badge {
  position: absolute;
  top: -0.45rem;
  right: -0.55rem;
  display: grid;
  min-width: 1.05rem;
  height: 1.05rem;
  place-items: center;
  border: 1.5px solid var(--app-card);
  border-radius: 999px;
  background: var(--app-destructive);
  color: white;
  padding: 0 0.2rem;
  font-size: 0.625rem;
  font-weight: 700;
  line-height: 1;
  font-variant-numeric: tabular-nums;
}
.task-empty {
  min-height: 12rem;
  padding: 1rem;
}
.mobile-task-bar {
  position: fixed;
  right: 0.75rem;
  bottom: calc(4.75rem + env(safe-area-inset-bottom));
  left: 0.75rem;
  z-index: 29;
  display: flex;
  align-items: center;
  gap: 0.75rem;
  border: 1px solid color-mix(in srgb, var(--app-info) 30%, var(--app-border));
  border-radius: var(--radius-md);
  background: var(--app-card);
  padding: 0.625rem 0.75rem;
  box-shadow: 0 8px 24px color-mix(in srgb, var(--shadow-ink) 12%, transparent);
  color: var(--app-foreground);
}
.mobile-task-bar strong,
.mobile-task-bar small {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.mobile-task-bar strong {
  font-size: 0.8125rem;
}
.mobile-task-bar small {
  margin-top: 0.125rem;
  color: var(--app-muted-foreground);
  font-size: 0.6875rem;
}
.mobile-task-pulse {
  display: block;
  width: 0.375rem;
  height: 0.375rem;
  flex: none;
  border-radius: 999px;
  background: var(--app-info);
  box-shadow: 0 0 0 0.2rem color-mix(in srgb, var(--app-info) 14%, transparent);
}
@keyframes task-progress {
  0% {
    transform: translateX(-130%);
  }
  100% {
    transform: translateX(320%);
  }
}
@keyframes media-grid {
  to {
    background-position: 12px 12px;
  }
}
/**
 * 浮层遮罩：1024 以下（底部抽屉）与 1024–1199（右侧浮层）共用，
 * 所以定义在全局而不是某个媒体查询里——遮罩的元素由 `isOverlay` 控制挂载，
 * 两档浮层的差异只在面板形态（见各自的 @media 块）。
 *
 * 层级 34 低于 `.task-center-shell` 的 35，保证面板浮在遮罩之上。
 */
.task-drawer-backdrop {
  position: fixed;
  inset: 0;
  /*
   * ⚠️ 这个 z-index 是**外壳内部**的层级，不是全局的——遮罩是 `.task-center-shell`
   * 的子元素，而外壳自己（z-index 35）就是一个层叠上下文，所以它跟 35 没有可比性。
   *
   * 原来写的是 34（作者以为在和外壳的 35 比），实际效果是**把把手压住了**：把手是
   * z-index 1 ⇒ 抽屉打开时点不动它（Playwright 实测报
   * "task-drawer-backdrop intercepts pointer events"，人点也是没反应）。
   *
   * 现在的次序（都在外壳内部）：遮罩 0 ＜ 面板（z-index auto，DOM 在后）＜ 把手 1。
   * 遮罩能盖住页面内容靠的是**外壳**的 35，不是这里。
   */
  z-index: 0;
  /* 遮罩的语义是"压暗背面"，与主题无关，统一走 `--scrim`（见 style.css 里那条注释）。 */
  background: var(--scrim);
}
@media (max-width: 1023px) {
  .task-center-closed {
    visibility: hidden;
  }
  .task-center-shell {
    inset: auto 0 calc(4rem + env(safe-area-inset-bottom));
    width: auto;
    height: min(78dvh, 42rem);
  }
  .task-center {
    width: auto;
    height: 100%;
    border-top: 1px solid var(--app-border);
    border-left: 0;
    border-radius: 1rem 1rem 0 0;
    box-shadow: 0 -12px 28px color-mix(in srgb, var(--shadow-ink) 12%, transparent);
  }
  .task-center-closed {
    transform: translateY(calc(100% + 4rem));
  }
  .task-toggle {
    display: none;
  }
  .task-mobile-close {
    display: inline-flex;
  }
}
@media (min-width: 768px) and (max-width: 1023px) {
  .task-center-shell {
    inset: 4rem 0 0 auto;
    width: var(--task-rail-width);
    height: auto;
  }
  .task-center {
    width: var(--task-rail-width);
    height: 100%;
    border-top: 0;
    border-left: 1px solid var(--app-border);
    border-radius: 0;
  }
  /* 平板区间是浮层：整块移出，不残留窄条 */
  .task-center-closed {
    transform: translateX(var(--task-rail-width));
  }
  .task-toggle {
    display: inline-flex;
  }
  .task-mobile-close {
    display: inline-flex;
  }
}
/**
 * 1024–1199：右侧浮层。
 *
 * 这个区间左栏已常驻（`lg:flex`），但宽度不足以同时容纳「左栏 + 内容 + 右栏」，
 * 所以右栏不再常驻，而是盖在内容上的浮层——主区不让位（AppShell 的让位规则从
 * 1200 才生效），内容因此能吃满剩余宽度。
 *
 * 浮层必须给出明确的「我是浮层」信号，否则就是无声遮挡：
 *   - 遮罩（`.task-drawer-backdrop` 由 isOverlay 渲染，形态见上）
 *   - 收窄 + 圆角 + 加重阴影，与常驻侧栏拉开视觉层级
 * 收起点（`.task-center-closed`）平移量随之改为自身宽度，避免边缘露出。
 */
@media (min-width: 1024px) and (max-width: 1199px) {
  .task-center-shell {
    inset: 4rem 0 0 auto;
    width: var(--task-rail-width);
  }
  .task-center {
    width: var(--task-rail-width);
    border-radius: 0.75rem 0 0 0.75rem;
    box-shadow: -16px 0 40px color-mix(in srgb, var(--shadow-ink) 22%, transparent);
  }
  .task-center-closed {
    transform: translateX(100%);
  }
  /* 浮层靠遮罩点击关闭，轨道上的 toggle 保持可见以便再次收起 */
  .task-toggle {
    display: inline-flex;
  }
  /**
   * ⚠️ 收起态的落点：浮层档**没有"窄条"**（收起时面板整块移出，右缘什么都不留），
   * 所以把手要贴到视口右缘。不写这条的话它沿用基础规则的 `left: -(把手+1px)`——
   * 那是"贴面板左缘"的意思，于是收起后把手钉在**轨道左缘**（距右边 宽度+把手 ≈ 357px），
   * 飘在内容中间像个孤儿按钮（实测报的）。
   *
   * 与 ≥1200 那条是同一个毛病，只是常驻档要减掉 `--task-rail-peek`（那里真的留了一条窄条），
   * 浮层档不减。两处都别再写死数字。
   */
  .task-center-shell.is-closed .task-toggle {
    left: calc(var(--task-rail-width) - var(--task-rail-handle) - 1px);
  }
  .task-mobile-close {
    display: inline-flex;
  }
}
@media (min-width: 1024px) {
  .mobile-task-bar {
    display: none;
  }
}
/**
 * 常驻侧栏（≥1200）收起时，把手必须跟着"留在视口里的窄条"走。
 *
 * 把手是 `.task-center-shell` 的子元素（不是面板的），因为浮层模式下面板会整块滑走，
 * 把手得留在轨道上才能再次打开。但在常驻模式下这就出了个问题：收起时面板右移
 * (宽度 − 窄条)，把手却仍钉在**轨道左缘**（距右边 宽度+把手 ≈ 357px），于是飘在
 * 内容中间 —— 看起来像个孤儿按钮。
 *
 * 收起态的落点 = 轨道宽度 − 窄条 − 把手 − 1px 缝，即窄条左侧紧邻的位置。
 * 全部由 token 推导，别再写死数字。
 */
@media (min-width: 1200px) {
  .task-center-shell.is-closed .task-toggle {
    left: calc(var(--task-rail-width) - var(--task-rail-peek) - var(--task-rail-handle) - 1px);
  }
}
@media (prefers-reduced-motion: reduce) {
  .task-media-running .task-media-grid,
  .task-progress-track i {
    animation: none;
  }
}
</style>
