<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useV2WorkspaceStore } from '@/features/v2/stores/workspace'
import { v2 } from '@/shared/api/v2'
import { useAppStore } from '@/shared/stores/app'
import Card from '@/components/ui/Card.vue'
import Button from '@/components/ui/Button.vue'
import Dialog from '@/components/ui/Dialog.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import Textarea from '@/components/ui/Textarea.vue'
import EmptyState from '@/components/shared/EmptyState.vue'
import ParameterField from '@/components/shared/ParameterField.vue'
import { useRunnerSessionsStore } from '@/features/v2/stores/runner-sessions'
import { useExecutionConnections } from '@/shared/composables/useExecutionConnections'
import { useLocale } from '@/shared/composables/useLocale'
import { looksLikeLongText } from '@/shared/lib/fieldHeuristics'
import { formatUsage, type RunUsage } from '@/shared/lib/runUsage'

const { t } = useLocale()
const route = useRoute()
const router = useRouter()
const workspace = useV2WorkspaceStore()
const runnerSessions = useRunnerSessionsStore()
const appStore = useAppStore()
const session = runnerSessions.open(String(route.params.id), '')

const recipeId = Number(route.params.id)
const validRecipeId = Number.isInteger(recipeId) && recipeId > 0
/** 运行页里这个 recipe 对象（可能还没加载出来）。 */
const recipe = computed(() => workspace.recipes.find((item) => item.recipeId === recipeId))

/**
 * 这一页正在跑的**是什么**：按平台上它自己的身份说，不写死。
 *
 * ⚠️ 以前这行永远写「AI 应用」——对 `rh-app` 是对的，对 `rh-workflow` 是**假话**：
 * 那是用户自己的工作流，没有作者、没有平台给的参数表（字段还是我们从图里发现的）。
 * 「工作流」和「AI 应用」都是 **RunningHub 自己的词**（两种不同的对象），我们只转述；
 * 认不出来时才退回我们自己的伞词「应用」（伞词是我们的，但不能拿它盖掉平台的区别）。
 */
const recipeKindLabel = computed(() => {
  switch (recipe.value?.kind) {
    case 'rh-app':
      return t('common.eyebrowApp')
    case 'rh-workflow':
      return t('library.kindRhWorkflow')
    case 'comfyui-workflow':
      return t('library.kindComfyWorkflow')
    default:
      return t('common.eyebrowRecipe')
  }
})
const recipeLoadFailed = ref(!validRecipeId)

const { loadConnections, connectionName } = useExecutionConnections()

/**
 * 这条配方绑定的连接名。
 *
 * 运行页是**唯一**决定"这一跑打到哪个账号"的地方，而此前它一个字都不提连接：配了国内站 +
 * 国际站两条 RunningHub 连接（两个独立账号，见连接页的端点预设）时，用户在按下提交之前
 * 根本看不出会扣哪一个。
 *
 * 名字取自连接缓存——AppShell 启动时已经拉过，命中缓存**不发请求**（`onMounted` 里那次
 * `loadConnections()` 在已加载时直接返回）。解析不出来（连接被删、缓存没就绪）就**不显示**：
 * `连接 #3` 是噪声，而"这条连接没了"提交时会由 daemon/平台明确报出来。
 */
const boundConnectionName = computed(() => {
  const id = recipe.value?.defaultConnectionId
  return id == null ? '' : connectionName(String(id), '')
})

/**
 * 提交那一行：`将创建 3 项任务 · 连接 RunningHub`。
 *
 * ⚠️ 这里**只报身份，不报余额**（2026-10-13 定，理由在 NEXT.md）：余额是 RunningHub 一家的
 * 面（`ports/provider.rs` 的默认实现是"不支持"），把它搬到这里等于把"哪家才有这个面"的判据
 * 复制到第二个文件，而"余额不够"这件事**平台在同一个位置已经会明确报出来**（提交失败的
 * `submitError`）。身份是协议无关的（谁都有连接名），所以它留在这里。
 */
const decisionLine = computed(() => {
  const count =
    batchMode.value === 'batch'
      ? t('runner.willCreate', { count: batchRows.value.length })
      : t('runner.willCreateOne')
  return boundConnectionName.value
    ? `${count} · ${t('runner.boundConnection', { name: boundConnectionName.value })}`
    : count
})
/** 表单要用的 schema 形状。后端 inputSchema 是自由 JSON（工作流字段千变万化），按用途收窄。 */
interface FormSchema {
  revisionId?: number
  defaults?: Record<string, unknown>
  inputSchema?: {
    properties?: Record<
      string,
      {
        type?: string
        format?: string
        enum?: string[]
        description?: string
        default?: unknown
        minimum?: number
        maximum?: number
        multipleOf?: number
      }
    >
    required?: string[]
  }
}
const schema = ref<FormSchema>({})
/**
 * 这条配方绑定的连接**声明**能跑的模型（后端 `/recipes/{id}/schema` 给的 `modelOptions`）。
 *
 * 空数组 = 连接没声明 ⇒ 不过滤（照旧自由填写）。声明是给人方便，不是闸门：真发了清单外的
 * 模型，由平台去拒——我们不在这里替平台把关。
 */
const modelOptions = ref<string[]>([])
const values = reactive<Record<string, unknown>>({})
const batchMode = ref<'single' | 'batch'>('single')
const submitting = ref(false)
const submitError = ref('')

// ---------- V1 式批量：统一参数 + 变量字段 + 任务行 ----------
type RowStatus = 'pending' | 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled'
interface BatchRow {
  id: number
  values: Record<string, unknown>
  files: Record<string, { assetId: number; checksum: string; filename: string }>
  /** 提交后的行级执行状态（未提交/草稿行为 pending）。 */
  status?: RowStatus
  /** 提交时随 payload 生成的显式 id：行↔run 映射以此为准，不依赖数组下标。 */
  clientRunId?: string
  runId?: number
  taskId?: string
  /** 平台报的用量（耗时/花费）。**只显示平台说的**，没有就不显示。 */
  usage?: RunUsage | null
}
const variableKeys = ref<string[]>([])
const batchRows = ref<BatchRow[]>([])
const batchAddCount = ref('3')
/** 「一次添加多条」弹窗。 */
const showBatchAddDialog = ref(false)
/** 「清空待处理」确认弹窗。 */
const showClearPendingDialog = ref(false)
/** 「单独填写的格子是空的」提交前确认弹窗。 */
const showBlankCellsDialog = ref(false)
const blankCells = ref<Array<{ label: string; rows: number[] }>>([])
/** 用户在弹窗里点过"仍要提交"后放行一次；每次提交前重新问（非响应式，进 run() 即消费）。 */
let blankCellsConfirmed = false
let rowSeq = 1
const nextRowId = () => rowSeq++

/** 提交后轮询该批次操作，把每个 run 的状态映射回对应任务行。 */
let batchPollTimer: number | undefined
const mapRunStatus = (status: string): RowStatus => {
  if (status === 'SUCCEEDED') return 'succeeded'
  if (status === 'FAILED') return 'failed'
  if (status === 'CANCELLED') return 'cancelled'
  if (status === 'RUNNING' || status === 'ACCEPTED') return 'running'
  return 'queued'
}
const stopBatchPolling = () => {
  if (batchPollTimer) window.clearInterval(batchPollTimer)
  batchPollTimer = undefined
}
const startBatchPolling = (operationId: number) => {
  stopBatchPolling()
  batchPollTimer = window.setInterval(async () => {
    try {
      // 走 workspace store 刷新（详情+摘要同步落库），Runner 与任务中心共享同一份数据源。
      const detail = await workspace.refreshOperation(operationId)
      const byClientRunId = new Map(detail.runs.map((run) => [run.clientRunId, run]))
      const byRunId = new Map(detail.runs.map((run) => [run.runId, run]))
      let active = 0
      batchRows.value = batchRows.value.map((row) => {
        const run =
          (row.clientRunId ? byClientRunId.get(row.clientRunId) : undefined) ??
          (row.runId !== undefined ? byRunId.get(row.runId) : undefined)
        if (!run) return row
        const status = mapRunStatus(run.status)
        if (status === 'queued' || status === 'running') active += 1
        return {
          ...row,
          status,
          runId: run.runId,
          taskId: run.clientRunId || String(run.runId),
          usage: run.usage ?? null
        }
      })
      if (active === 0) stopBatchPolling()
    } catch {
      // 轮询失败保持现状，下次继续。
    }
  }, 2500)
}
/** 行级状态徽章的展示（颜色/文案）。 */
const rowStatusBadge = (
  status?: RowStatus
): { tone: 'info' | 'success' | 'error' | 'neutral' | 'warning'; label: string } => {
  const labels: Record<RowStatus, string> = {
    pending: t('status.pending'),
    queued: t('status.queued'),
    running: t('status.running'),
    succeeded: t('status.success'),
    failed: t('status.failed'),
    cancelled: t('status.cancelled')
  }
  const tones: Record<RowStatus, 'info' | 'success' | 'error' | 'neutral' | 'warning'> = {
    pending: 'neutral',
    queued: 'info',
    running: 'info',
    succeeded: 'success',
    failed: 'error',
    cancelled: 'neutral'
  }
  const key: RowStatus = status ?? 'pending'
  return { tone: tones[key], label: labels[key] }
}
/** 批次摘要：待提交 / 执行中 / 已完成 / 失败 计数（措辞与 status.* 对齐）。 */
const batchSummary = computed(() => {
  const count = (predicate: (status?: RowStatus) => boolean) =>
    batchRows.value.filter((row) => predicate(row.status)).length
  return {
    total: batchRows.value.length,
    pending: count((s) => !s || s === 'pending'),
    active: count((s) => s === 'queued' || s === 'running'),
    succeeded: count((s) => s === 'succeeded'),
    failed: count((s) => s === 'failed' || s === 'cancelled')
  }
})
const variableFields = computed(() =>
  fields.value.filter((field) => variableKeys.value.includes(field.key))
)

/**
 * 新行以当前统一参数为**起点快照**（不是继承）。
 *
 * 快照之后这一行就是独立数据：格子里显示什么，提交的就是什么；删空就是真的空。
 * 反过来（行值留空 = 动态继承统一参数）会让"清空"被统一值立刻补回来，
 * 用户看到的是"我刚删完它又给我填上了"——隐藏状态换来的是不可预期。
 */
const makeRow = (): BatchRow => {
  const rowValues: Record<string, unknown> = {}
  const rowFiles: Record<string, { assetId: number; checksum: string; filename: string }> = {}
  for (const key of variableKeys.value) {
    if (values[key] !== undefined) rowValues[key] = values[key]
    if (fileRefs.value[key]) rowFiles[key] = fileRefs.value[key]
  }
  return { id: nextRowId(), values: rowValues, files: rowFiles }
}

const ensureBatchRows = () => {
  if (!batchRows.value.length) batchRows.value = [makeRow()]
}

const toggleVariableField = (key: string) => {
  if (variableKeys.value.includes(key)) {
    // 取消「单独填写」：这一列回到统一参数，行内覆盖不再有意义，一并清掉。
    variableKeys.value = variableKeys.value.filter((item) => item !== key)
    for (const row of batchRows.value) {
      delete row.values[key]
      delete row.files[key]
    }
    return
  }
  variableKeys.value = [...variableKeys.value, key]
  // 勾上时把当前统一值快照进**已有**的行：否则这些格子会空着，
  // 而它们本该以"勾选那一刻的统一参数"为起点（与新行的规则一致）。
  for (const row of batchRows.value) {
    if (row.values[key] === undefined && values[key] !== undefined) row.values[key] = values[key]
    if (row.files[key] === undefined && fileRefs.value[key]) row.files[key] = fileRefs.value[key]
  }
}

const addBatchRow = () => {
  batchRows.value = [...batchRows.value, makeRow()]
}

const normalizedAddCount = computed(() => {
  const count = Number(batchAddCount.value)
  return Number.isFinite(count) ? Math.min(50, Math.max(1, Math.round(count))) : 1
})

const addMultipleRows = () => {
  const rows = Array.from({ length: normalizedAddCount.value }, () => makeRow())
  batchRows.value = [...batchRows.value, ...rows]
}

/** 「一次添加多条」弹窗确认。 */
const addMultipleRowsAndClose = () => {
  addMultipleRows()
  showBatchAddDialog.value = false
}

const duplicateBatchRow = (row: BatchRow) => {
  const index = batchRows.value.findIndex((item) => item.id === row.id)
  if (index === -1) return
  const copy: BatchRow = {
    id: nextRowId(),
    values: { ...row.values },
    files: { ...row.files }
  }
  batchRows.value = [
    ...batchRows.value.slice(0, index + 1),
    copy,
    ...batchRows.value.slice(index + 1)
  ]
}

const deleteBatchRow = (rowId: number) => {
  batchRows.value = batchRows.value.filter((item) => item.id !== rowId)
  ensureBatchRows()
}

const clearBatchRows = () => {
  batchRows.value = [makeRow()]
}

/** 「清空待处理」确认后执行。 */
const confirmClearBatchRows = () => {
  clearBatchRows()
  showClearPendingDialog.value = false
}

const setRowValue = (row: BatchRow, key: string, raw: string | boolean, type: string) => {
  if (type === 'boolean') {
    row.values[key] = raw === true || raw === 'true'
    return
  }
  // 数值字段的空值：与统一参数一致地视为"没填"（删掉这个键），
  // 否则提交时 `Number('')` 会把它悄悄变成 0。
  if ((type === 'integer' || type === 'number') && (raw === '' || raw == null)) {
    delete row.values[key]
    return
  }
  // 文本字段的空串就是空串：行里是看得见的真实值，不再有"空 = 继承统一参数"这回事。
  row.values[key] = raw
}

/** 表格单元格是否用多行文本域：先排除不可能多行的类型，再信 schema 的显式声明
 * （format=textarea），最后走关键词/长度启发式。
 * 启发式与 ParameterField 共用一份：shared/lib/fieldHeuristics。 */
const isMultilineField = (field: (typeof fields)['value'][number]) => {
  if (field.format === 'file') return false
  // 平台显式声明为多行文本（format=textarea）时直接采用。
  if (field.format === 'textarea') return true
  if (
    field.enum?.length ||
    field.type === 'boolean' ||
    field.type === 'integer' ||
    field.type === 'number'
  )
    return false
  return looksLikeLongText(field)
}

/** 行内文件选择：动态创建一次性 input，避免表格里挂一堆隐藏元素。 */
const openRowFilePicker = (row: BatchRow, key: string) => {
  if (rowUploadingKey.value) return
  const input = document.createElement('input')
  input.type = 'file'
  input.addEventListener('change', () => {
    void onRowFileSelected(row, key, input.files?.[0] ?? null)
  })
  input.click()
}

const fields = computed(() => {
  const properties = schema.value.inputSchema?.properties ?? {}
  const required = new Set(schema.value.inputSchema?.required ?? [])
  return Object.entries(properties).map(([key, value]) => ({
    key,
    type: value.type ?? 'string',
    format: value.format,
    // 模型字段：连接声明了清单就变成下拉（`ParameterField` 见到 enum 就渲染 select）。
    // 判据取**字段名** `model` —— 这是通用协议层的契约名（驱动的 ALLOWED_FIELDS 与
    // binding_schema 都写 `model`），不是猜的。
    // ⚠️ 当前值若不在清单里，**要保留它**：否则表单会把用户/配方已经定好的模型悄悄换成
    // 清单里的第一个（那是"改数据"，不是"给建议"）。
    enum:
      key === 'model' && modelOptions.value.length
        ? modelOptions.value.includes(String(values[key] ?? '')) || values[key] === undefined
          ? modelOptions.value
          : [String(values[key]), ...modelOptions.value]
        : value.enum,
    description: value.description,
    defaultValue: value.default,
    // 数字约束（JSON Schema 的关键字）要一路带到控件：RH 应用那条导入路一直在写
    // `minimum`/`maximum`，ComfyUI 探到节点定义时也会写上 min/max/step。
    minimum: value.minimum,
    maximum: value.maximum,
    multipleOf: value.multipleOf,
    required: required.has(key)
  }))
})

/**
 * 这表单还没被人整理过吗？—— 判据只用**数据里的事实**，不猜：
 * ① 有字段撞名（同一个描述出现在多个字段上：实测一条应用里有 5 个 `value`、5 个 `save_output`）；
 * ② 一个备注名都没写过（`fieldLabels` 为空）。
 * 满足任一条 ⇒ 在运行页说一句"名字是平台给的，可以改成好认的"；整理过的应用不唠叨。
 */
const fieldNamingHint = computed(() => {
  if (!fields.value.length) return false
  const labels = fields.value.map((field) => field.description || field.key)
  const hasDuplicate = new Set(labels).size !== labels.length
  const hasNoNote = Object.keys(recipe.value?.fieldLabels ?? {}).length === 0
  return hasDuplicate || hasNoNote
})

const fileRefs = ref<Record<string, { assetId: number; checksum: string; filename: string }>>({})
const uploadingKey = ref('')
const uploadError = ref('')
/** 行内文件上传态，键为 `${rowId}:${key}`。 */
const rowUploadingKey = ref('')
/** 行内文件拖拽态，键为 `${rowId}:${key}`；用深度计数避免子元素触发误清除。 */
const rowDraggingKey = ref('')
let rowDragDepth = 0
const onRowDragEnter = (row: BatchRow, key: string) => {
  rowDragDepth += 1
  rowDraggingKey.value = `${row.id}:${key}`
}
const onRowDragLeave = () => {
  rowDragDepth = Math.max(0, rowDragDepth - 1)
  if (rowDragDepth === 0) rowDraggingKey.value = ''
}
const onRowDragDrop = (row: BatchRow, key: string, event: DragEvent) => {
  rowDragDepth = 0
  rowDraggingKey.value = ''
  void onRowFileSelected(row, key, event.dataTransfer?.files?.[0] ?? null)
}

watch(
  [batchMode, values, fileRefs, variableKeys, batchRows],
  () => {
    runnerSessions.update(String(route.params.id), {
      mode: batchMode.value,
      values,
      files: fileRefs.value,
      variableKeys: variableKeys.value,
      batchRows: batchRows.value
    })
  },
  { deep: true }
)

watch(recipe, (current) => {
  if (current) runnerSessions.update(String(route.params.id), {})
})

watch(batchMode, (mode) => {
  if (mode === 'batch') ensureBatchRows()
})

/** 上传文件的公共底层：返回 asset 引用。 */
const uploadAssetFile = async (file: File) => {
  const bytes = new Uint8Array(await file.arrayBuffer())
  const uploaded = await v2.uploadAsset(file.name, file.type || 'application/octet-stream', bytes)
  return { assetId: uploaded.assetId, checksum: uploaded.checksum, filename: file.name }
}

const onFileSelected = async (key: string, file: File | null) => {
  if (!file) return
  if (file.size > 100 * 1024 * 1024) {
    uploadError.value = t('runner.uploadTooLarge')
    return
  }
  uploadingKey.value = key
  uploadError.value = ''
  try {
    fileRefs.value = { ...fileRefs.value, [key]: await uploadAssetFile(file) }
    values[key] = file.name
  } catch (caught) {
    uploadError.value = caught instanceof Error ? caught.message : String(caught)
  } finally {
    uploadingKey.value = ''
  }
}

const clearFile = (key: string) => {
  const next = { ...fileRefs.value }
  delete next[key]
  fileRefs.value = next
  delete values[key]
}

/** 任务行内选择文件：上传到该行的变量文件槽位。 */
const onRowFileSelected = async (row: BatchRow, key: string, file: File | null) => {
  if (!file) return
  if (file.size > 100 * 1024 * 1024) {
    uploadError.value = t('runner.uploadTooLarge')
    return
  }
  const stateKey = `${row.id}:${key}`
  rowUploadingKey.value = stateKey
  uploadError.value = ''
  try {
    row.files = { ...row.files, [key]: await uploadAssetFile(file) }
    row.values[key] = file.name
  } catch (caught) {
    uploadError.value = caught instanceof Error ? caught.message : String(caught)
  } finally {
    rowUploadingKey.value = ''
  }
}

const clearRowFile = (row: BatchRow, key: string) => {
  const next = { ...row.files }
  delete next[key]
  row.files = next
  delete row.values[key]
}

onMounted(async () => {
  if (!validRecipeId) return
  // 连接缓存没就绪时补一次（已加载则直接返回、不发请求）：否则上面那行连接名是空的。
  void loadConnections().catch(() => undefined)
  try {
    await workspace.loadRecipes()
    const loaded = await v2.getRecipeSchema(recipeId)
    // inputSchema 在后端是自由 JSON，这里按表单要用的形状收窄 ——
    // 与 workspace.fetchRecipeSchema 同一手法，别把它当成强类型契约。
    schema.value = {
      revisionId: loaded.revisionId,
      defaults: loaded.defaults,
      inputSchema: loaded.inputSchema as FormSchema['inputSchema']
    }
    modelOptions.value = loaded.modelOptions ?? []
    recipeLoadFailed.value = false
    batchMode.value = session.mode
    Object.assign(values, session.values)
    fileRefs.value = { ...session.files }
    variableKeys.value = [...(session.variableKeys ?? [])]
    batchRows.value = (session.batchRows ?? []).map((row) => ({
      ...row,
      values: { ...row.values },
      files: { ...row.files }
    }))
    for (const row of batchRows.value) rowSeq = Math.max(rowSeq, row.id + 1)
    if (batchMode.value === 'batch') ensureBatchRows()
    const revisionDefaults = schema.value.defaults ?? {}
    for (const field of fields.value) {
      if (values[field.key] !== undefined) continue
      if (revisionDefaults[field.key] !== undefined) values[field.key] = revisionDefaults[field.key]
      else if (field.defaultValue !== undefined) values[field.key] = field.defaultValue
      else if (field.type === 'boolean') values[field.key] = false
      else if (field.enum?.length) values[field.key] = field.enum[0]
    }
    runnerSessions.open(String(route.params.id), recipe.value?.name ?? `Recipe #${recipeId}`)
  } catch (caught) {
    recipeLoadFailed.value = true
    submitError.value = caught instanceof Error ? caught.message : String(caught)
  }
})

// 离开 Runner 页面时停止批次轮询，避免定时器空转。
onBeforeUnmount(() => {
  stopBatchPolling()
})

const setValue = (key: string, raw: string | boolean, type: string) => {
  if (type === 'boolean') {
    values[key] = raw === true || raw === 'true'
    return
  }
  // 数值字段保留原始字符串中间态（含 “12.”、“0.5” 等），提交时统一校验/转换，
  // 避免输入过程中值被清空导致的“输入不了”体验。
  if ((type === 'integer' || type === 'number') && (raw === '' || raw == null)) {
    delete values[key]
    return
  }
  values[key] = raw
}

/** 把一组原始值标准化为提交载荷：数值字符串转 Number，布尔转 boolean。 */
const normalizeInputs = (raw: Record<string, unknown>) => {
  const inputs: Record<string, unknown> = {}
  for (const field of fields.value) {
    const value = raw[field.key]
    if (value === undefined) continue
    if (field.type === 'integer' || field.type === 'number') inputs[field.key] = Number(value)
    else if (field.type === 'boolean') inputs[field.key] = value === true || value === 'true'
    else inputs[field.key] = value
  }
  return inputs
}

const isInvalidNumber = (field: (typeof fields)['value'][number], value: unknown) => {
  if (field.type !== 'integer' && field.type !== 'number') return false
  if (value === undefined || value === null || value === '') return false
  const raw = String(value)
  return field.type === 'integer' ? !/^-?\d+$/.test(raw) : !Number.isFinite(Number(raw))
}

const isMissingValue = (field: (typeof fields)['value'][number], value: unknown) => {
  if (field.type === 'boolean') return value === undefined
  return (
    value === undefined ||
    value === null ||
    value === '' ||
    (typeof value === 'string' && !value.trim())
  )
}

/**
 * 某一行最终的参数值：统一参数 + 行内的「单独填写」值。
 *
 * 单独填写的字段**只认行内值**（行里没有就是没填），不再回退统一参数：
 * 既然格子是快照出来的真实值，"空着"就该如实报"缺少"，而不是被统一值悄悄补上。
 */
const rowEffectiveValues = (row: BatchRow) => {
  const merged: Record<string, unknown> = { ...values }
  for (const key of variableKeys.value) {
    if (row.values[key] === undefined) delete merged[key]
    else merged[key] = row.values[key]
  }
  return merged
}

/**
 * 「单独填写」的字段里，哪些行是空的。
 *
 * 快照语义下这些格子没有统一值兜底（不会回退统一参数），空着就会按空提交，
 * 所以在花钱之前把行号列出来确认一次。布尔/文件不在此列：它们不是"没填"就是"没选"。
 */
const blankVariableCells = () => {
  const blanks: Array<{ label: string; rows: number[] }> = []
  for (const key of variableKeys.value) {
    const field = fields.value.find((item) => item.key === key)
    if (!field || field.type === 'boolean' || field.format === 'file') continue
    const rows = batchRows.value
      .map((row, index) => (isMissingValue(field, row.values[key]) ? index + 1 : 0))
      .filter((n) => n > 0)
    if (rows.length) blanks.push({ label: field.description || field.key, rows })
  }
  return blanks
}

/** 弹窗里点"仍要提交"：带一次放行标记重跑提交。 */
const confirmBlankCellsAndRun = () => {
  showBlankCellsDialog.value = false
  blankCellsConfirmed = true
  void run()
}

const run = async () => {
  // 双击/重复点击时 disabled 还没渲染生效，这里同步挡一次重复提交。
  if (submitting.value) return
  submitting.value = true
  submitError.value = ''
  // 消费掉"已确认空值"这一次放行：改完再提交要重新问。
  const blankConfirmed = blankCellsConfirmed
  blankCellsConfirmed = false
  try {
    const revisionId = schema.value.revisionId
    if (!revisionId) throw new Error(t('runner.revisionExpired'))

    // 待提交的行：单次模式即当前统一参数一行。
    const rows: BatchRow[] =
      batchMode.value === 'batch' ? batchRows.value : [{ id: 0, values: {}, files: {} }]

    // 数值/必填校验：按每行最终生效值检查，报出第一个问题行。
    for (const [index, row] of rows.entries()) {
      const effective = rowEffectiveValues(row)
      const rowTag = batchMode.value === 'batch' ? t('runner.rowTag', { n: index + 1 }) : ''
      const invalidNumbers = fields.value
        .filter((field) => isInvalidNumber(field, effective[field.key]))
        .map((field) => field.description || field.key)
      if (invalidNumbers.length) {
        throw new Error(
          t('runner.invalidNumbers', {
            row: rowTag,
            fields: invalidNumbers.join(t('common.listSeparator'))
          })
        )
      }
      const missing = fields.value
        .filter((field) => field.required)
        .filter((field) => isMissingValue(field, effective[field.key]))
        .map((field) => field.description || field.key)
      if (missing.length) {
        throw new Error(
          t('runner.missingRequired', {
            row: rowTag,
            fields: missing.join(t('common.listSeparator'))
          })
        )
      }
    }

    // 硬校验都过了，再确认一次"空的单独填写项"：这类格子没有统一值兜底，
    // 空着就会按空值提交——花的是真钱，值得多问一句。
    if (batchMode.value === 'batch' && !blankConfirmed) {
      const blanks = blankVariableCells()
      if (blanks.length) {
        blankCells.value = blanks
        showBlankCellsDialog.value = true
        return
      }
    }

    const fileFields = fields.value.filter((field) => field.format === 'file')
    // 每行显式生成 clientRunId：行↔run 映射用它对齐，不依赖返回数组顺序。
    const submitToken = Date.now().toString(36)
    const clientRunIds = rows.map((row) => `row-${row.id}-${submitToken}`)
    const runsPayload = rows.map((row, index) => {
      // 资产按**字段键**定位（field.key，如 "475::image"）：位置数组的语义由上游节点
      // 顺序决定，把它当语义正是图片错位的来源。没选文件的字段直接不进对象（跳过中间
      // 槽位是合法的——不必从第一个文件字段开始连续填）。
      const assets: Record<string, { assetId: number; checksum: string }> = {}
      for (const field of fileFields) {
        const ref = row.files[field.key] ?? fileRefs.value[field.key]
        if (ref) assets[field.key] = { assetId: ref.assetId, checksum: ref.checksum }
      }
      return {
        clientRunId: clientRunIds[index],
        inputs: normalizeInputs(rowEffectiveValues(row)),
        assets
      }
    })
    const result = await workspace.runRecipes({
      recipeRevisionId: revisionId,
      runs: runsPayload
    })
    // 轻量漂浮提醒；进度与详情由右侧「实时任务」面板统一展示。
    appStore.showNotification('success', t('runner.submitted', { id: result.operationId }))
    // 批量模式：按 clientRunId 把 run 映射回任务行，并轮询每行状态。
    if (batchMode.value === 'batch') {
      batchRows.value = batchRows.value.map((row, index) => ({
        ...row,
        clientRunId: clientRunIds[index]
      }))
      const detail =
        workspace.operations.find((item) => item.operationId === result.operationId) ??
        (await workspace.refreshOperation(result.operationId).catch(() => undefined))
      const byClientRunId = new Map((detail?.runs ?? []).map((run) => [run.clientRunId, run]))
      const runIds = result.runIds ?? []
      batchRows.value = batchRows.value.map((row, index) => {
        const run = byClientRunId.get(row.clientRunId ?? '')
        const runId = run?.runId ?? runIds[index]
        return {
          ...row,
          status: 'queued',
          runId,
          taskId: run?.clientRunId ?? (runId !== undefined ? String(runId) : undefined)
        }
      })
      startBatchPolling(result.operationId)
    }
  } catch (caught) {
    submitError.value = caught instanceof Error ? caught.message : String(caught)
  } finally {
    submitting.value = false
  }
}

const goBackToLibrary = () => {
  void router.push({ name: 'Library' })
}

/**
 * 去「编辑」改字段名与控件。
 *
 * 为什么要从**运行页**也能进：用户是在这里发现问题的（一条应用 31 个字段、里面 5 个
 * 一模一样的 `value`），而改名字与控件的对话框长在应用库那一页——发现问题的地方没有入口，
 * 等于让人先记住"这事要去别处改"。带着 `?edit=<id>` 过去，那一页会把编辑框直接打开。
 */
const goEditFields = () => {
  void router.push({ path: '/library', query: { edit: String(recipeId) } })
}
</script>

<template>
  <div class="mx-auto max-w-4xl space-y-6 px-4 py-6 pb-24 lg:pb-8">
    <header class="flex items-center gap-3">
      <Button variant="ghost" size="sm" @click="goBackToLibrary">{{
        t('runner.backToLibrary')
      }}</Button>
      <h1 class="text-xl font-semibold">{{ recipe?.name ?? `Recipe #${recipeId}` }}</h1>
    </header>

    <EmptyState
      v-if="recipeLoadFailed"
      :title="t('runner.openFailedTitle')"
      :description="t('runner.openFailedDescription')"
    >
      <template #actions>
        <Button variant="outline" @click="goBackToLibrary">{{
          t('runner.backToLibraryPlain')
        }}</Button>
      </template>
    </EmptyState>

    <p
      v-if="submitError && !recipeLoadFailed"
      class="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
    >
      {{ submitError }}
    </p>

    <p
      v-if="uploadError"
      class="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
    >
      {{ uploadError }}
    </p>

    <Card v-if="!recipeLoadFailed" class="space-y-5 p-5">
      <div class="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <p class="text-xs font-semibold uppercase tracking-[0.08em] text-info">
            {{ recipeKindLabel }}
          </p>
          <h2 class="mt-1 text-base font-semibold">{{ t('runner.paramsTitle') }}</h2>
          <p class="mt-1 text-sm text-muted-foreground">
            {{ t('runner.paramsHint') }}
          </p>
          <!--
            名字是平台给的这事儿，只在**看得出还没整理过**的时候说一句（见 `fieldNamingHint`），
            并且把入口放在这里 —— 用户正是在这一步发现"两个 value 分不清"的。
          -->
          <p v-if="fieldNamingHint" class="mt-1 text-xs text-muted-foreground">
            {{ t('runner.fieldNamingHint') }}
            <button
              type="button"
              class="font-medium text-primary underline-offset-4 hover:underline"
              @click="goEditFields"
            >
              {{ t('runner.editFields') }}
            </button>
          </p>
        </div>
        <div
          class="flex rounded-md border border-border bg-secondary/60 p-1"
          role="radiogroup"
          :aria-label="t('runner.modeGroup')"
        >
          <button
            type="button"
            class="run-mode-tab"
            :class="batchMode === 'single' && 'is-active'"
            role="radio"
            :aria-checked="batchMode === 'single'"
            @click="batchMode = 'single'"
          >
            {{ t('runner.modeSingle') }}
          </button>
          <button
            type="button"
            class="run-mode-tab"
            :class="batchMode === 'batch' && 'is-active'"
            role="radio"
            :aria-checked="batchMode === 'batch'"
            @click="batchMode = 'batch'"
          >
            {{ t('runner.modeBatch') }}
          </button>
        </div>
      </div>
      <div
        v-if="batchMode === 'batch'"
        class="rounded-md border border-info/25 bg-info/5 p-3 text-sm"
      >
        <p class="font-medium">{{ t('runner.batchUniformTitle') }}</p>
        <p class="mt-0.5 text-xs text-muted-foreground">
          {{ t('runner.batchUniformHint') }}
        </p>
      </div>
      <div v-if="fields.length" class="grid gap-4 sm:grid-cols-2">
        <ParameterField
          v-for="field in fields"
          :key="field.key"
          :field="field"
          :model-value="values[field.key]"
          :disabled="batchMode === 'batch' && variableKeys.includes(field.key)"
          :file="
            fileRefs[field.key] || uploadingKey === field.key
              ? { filename: fileRefs[field.key]?.filename, uploading: uploadingKey === field.key }
              : null
          "
          :id-prefix="`field`"
          @update:model-value="(value) => setValue(field.key, value, field.type ?? 'string')"
          @file-change="(key, file) => onFileSelected(key, file)"
          @clear-file="clearFile"
        >
          <template v-if="batchMode === 'batch'" #actions>
            <label class="batch-variable-toggle">
              <input
                type="checkbox"
                :checked="variableKeys.includes(field.key)"
                @change="toggleVariableField(field.key)"
              />
              {{ t('runner.fieldToggle') }}
            </label>
          </template>
        </ParameterField>
      </div>
      <EmptyState
        v-else
        :title="t('runner.noParamsTitle')"
        :description="t('runner.noParamsDescription')"
      />

      <!-- 批量任务行（V1 式逐条填写） -->
      <div v-if="batchMode === 'batch'" class="batch-rows-panel">
        <div class="batch-summary-bar" role="status" :aria-label="t('runner.batchSummary')">
          <span class="batch-summary-item is-neutral"
            ><span class="batch-summary-dot" aria-hidden="true" />{{ t('status.pending') }}
            <strong>{{ batchSummary.pending }}</strong></span
          >
          <span class="batch-summary-item is-info"
            ><span class="batch-summary-dot" aria-hidden="true" />{{ t('status.running') }}
            <strong>{{ batchSummary.active }}</strong></span
          >
          <span class="batch-summary-item is-success"
            ><span class="batch-summary-dot" aria-hidden="true" />{{ t('status.success') }}
            <strong>{{ batchSummary.succeeded }}</strong></span
          >
          <span class="batch-summary-item is-error"
            ><span class="batch-summary-dot" aria-hidden="true" />{{ t('status.failed') }}
            <strong>{{ batchSummary.failed }}</strong></span
          >
          <span class="batch-summary-total">{{
            t('runner.totalRows', { count: batchSummary.total })
          }}</span>
        </div>
        <div class="flex flex-wrap items-center justify-between gap-3">
          <div>
            <p class="text-sm font-semibold">{{ t('runner.rowsPanelTitle') }}</p>
            <p class="mt-0.5 text-xs text-muted-foreground">
              {{
                variableFields.length
                  ? t('runner.rowsPanelHint')
                  : t('runner.rowsPanelHintNoVariable')
              }}
            </p>
          </div>
          <div class="flex flex-wrap items-center gap-2">
            <Button type="button" variant="outline" size="sm" @click="addBatchRow">{{
              t('runner.addRow')
            }}</Button>
            <Button type="button" variant="outline" size="sm" @click="showBatchAddDialog = true">{{
              t('runner.addRows')
            }}</Button>
            <Button
              type="button"
              variant="outline"
              size="sm"
              class="text-destructive"
              :disabled="batchRows.length <= 1"
              @click="showClearPendingDialog = true"
              >{{ t('runner.clearPending') }}</Button
            >
          </div>
        </div>
        <div v-if="variableFields.length" class="batch-table-scroll">
          <table class="batch-table">
            <thead>
              <tr>
                <th class="batch-index-column">#</th>
                <th class="batch-status-column">{{ t('runner.colStatus') }}</th>
                <th v-for="field in variableFields" :key="field.key">
                  {{ field.description || field.key }}
                </th>
                <th class="batch-actions-column">{{ t('runner.colActions') }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="(row, index) in batchRows" :key="row.id">
                <td class="batch-index-column">{{ index + 1 }}</td>
                <td class="batch-status-column">
                  <StatusBadge :status="rowStatusBadge(row.status).tone">{{
                    rowStatusBadge(row.status).label
                  }}</StatusBadge>
                  <span
                    v-if="row.taskId"
                    class="mt-1 block max-w-28 truncate text-xs text-muted-foreground"
                    >{{ row.taskId }}</span
                  >
                  <!-- 平台报的耗时/花费：与任务页同一个格式化（只转述，没有就不显示）。 -->
                  <span
                    v-if="formatUsage([row])"
                    class="mt-1 block text-xs tabular-nums text-muted-foreground"
                    >{{ formatUsage([row]) }}</span
                  >
                </td>
                <td v-for="field in variableFields" :key="field.key">
                  <Textarea
                    v-if="isMultilineField(field)"
                    :model-value="String(row.values[field.key] ?? '')"
                    :rows="3"
                    class="min-w-56"
                    :placeholder="field.description"
                    @update:model-value="
                      (value) => setRowValue(row, field.key, value, field.type ?? 'string')
                    "
                  />
                  <select
                    v-else-if="field.enum?.length || field.type === 'boolean'"
                    class="batch-cell-select"
                    :value="
                      String(
                        row.values[field.key] ??
                          (field.type === 'boolean' ? 'false' : (field.enum?.[0] ?? ''))
                      )
                    "
                    @change="
                      setRowValue(
                        row,
                        field.key,
                        ($event.target as HTMLSelectElement).value,
                        field.type ?? 'string'
                      )
                    "
                  >
                    <template v-if="field.type === 'boolean'">
                      <option value="true">{{ t('common.yes') }}</option>
                      <option value="false">{{ t('common.no') }}</option>
                    </template>
                    <template v-else>
                      <option v-for="option in field.enum" :key="option" :value="option">
                        {{ option }}
                      </option>
                    </template>
                  </select>
                  <template v-else-if="field.format === 'file'">
                    <div
                      class="batch-cell-file"
                      :class="{ 'is-dragging': rowDraggingKey === `${row.id}:${field.key}` }"
                      @dragenter.prevent="onRowDragEnter(row, field.key)"
                      @dragover.prevent
                      @dragleave="onRowDragLeave"
                      @drop.prevent="onRowDragDrop(row, field.key, $event)"
                    >
                      <div class="flex flex-wrap items-center gap-1">
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          :disabled="rowUploadingKey === `${row.id}:${field.key}`"
                          @click="openRowFilePicker(row, field.key)"
                          >{{
                            row.files[field.key] ? t('runner.replace') : t('runner.choose')
                          }}</Button
                        >
                        <Button
                          v-if="row.files[field.key]"
                          type="button"
                          variant="ghost"
                          size="sm"
                          class="h-7 px-2 text-xs text-destructive"
                          @click="clearRowFile(row, field.key)"
                          >{{ t('runner.remove') }}</Button
                        >
                      </div>
                      <p
                        v-if="rowUploadingKey === `${row.id}:${field.key}`"
                        class="text-xs text-muted-foreground"
                      >
                        {{ t('runner.uploading') }}
                      </p>
                      <p
                        v-else-if="row.files[field.key]"
                        class="max-w-48 truncate text-xs text-muted-foreground"
                      >
                        {{ row.files[field.key].filename }}
                      </p>
                      <p v-else class="text-[11px] text-muted-foreground">
                        {{ t('runner.dropHint') }}
                      </p>
                    </div>
                  </template>
                  <input
                    v-else
                    class="batch-cell-input"
                    :type="field.type === 'integer' || field.type === 'number' ? 'number' : 'text'"
                    :value="row.values[field.key] ?? ''"
                    :placeholder="field.description"
                    @input="
                      setRowValue(
                        row,
                        field.key,
                        ($event.target as HTMLInputElement).value,
                        field.type ?? 'string'
                      )
                    "
                  />
                </td>
                <td class="batch-actions-column">
                  <div class="flex gap-1">
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      @click="duplicateBatchRow(row)"
                      >{{ t('runner.duplicate') }}</Button
                    >
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      class="h-7 px-2 text-xs text-destructive"
                      :disabled="batchRows.length === 1"
                      @click="deleteBatchRow(row.id)"
                      >{{ t('runner.delete') }}</Button
                    >
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      <div
        class="flex flex-col gap-3 border-t border-border pt-4 sm:flex-row sm:items-center sm:justify-between"
      >
        <p class="text-xs text-muted-foreground">{{ decisionLine }}</p>
        <Button :disabled="submitting" size="lg" @click="run">
          {{
            submitting
              ? t('runner.submitting')
              : batchMode === 'batch'
                ? t('runner.startBatch')
                : t('runner.start')
          }}
        </Button>
      </div>
    </Card>

    <Dialog
      :open="showBatchAddDialog"
      :title="t('runner.dialogAddTitle')"
      class="max-w-sm"
      @update:open="showBatchAddDialog = $event"
    >
      <div class="space-y-3">
        <p class="text-sm text-muted-foreground">
          {{ t('runner.dialogAddDescription') }}
        </p>
        <label class="block">
          <span class="mb-1 block text-xs font-medium text-muted-foreground">{{
            t('runner.countLabel')
          }}</span>
          <input
            v-model="batchAddCount"
            type="number"
            min="1"
            max="50"
            class="batch-dialog-input"
            @keyup.enter="addMultipleRowsAndClose"
          />
        </label>
      </div>
      <template #footer>
        <Button type="button" variant="outline" @click="showBatchAddDialog = false">{{
          t('common.cancel')
        }}</Button>
        <Button type="button" @click="addMultipleRowsAndClose">{{
          t('runner.addCount', { count: normalizedAddCount })
        }}</Button>
      </template>
    </Dialog>

    <Dialog
      :open="showClearPendingDialog"
      :title="t('runner.dialogClearTitle')"
      class="max-w-sm"
      @update:open="showClearPendingDialog = $event"
    >
      <p class="text-sm leading-6 text-muted-foreground">
        {{ t('runner.dialogClearDescription', { count: batchRows.length }) }}
      </p>
      <template #footer>
        <Button type="button" variant="outline" @click="showClearPendingDialog = false">{{
          t('common.cancel')
        }}</Button>
        <Button
          type="button"
          variant="outline"
          class="text-destructive"
          @click="confirmClearBatchRows"
          >{{ t('runner.clearPending') }}</Button
        >
      </template>
    </Dialog>

    <Dialog
      :open="showBlankCellsDialog"
      :title="t('runner.blankCellsTitle')"
      class="max-w-sm"
      @update:open="showBlankCellsDialog = $event"
    >
      <p class="text-sm leading-6 text-muted-foreground">
        {{ t('runner.blankCellsDescription') }}
      </p>
      <ul class="mt-3 space-y-1 text-sm">
        <li v-for="item in blankCells" :key="item.label">
          {{
            t('runner.blankCellsItem', {
              field: item.label,
              rows: item.rows.join(t('common.listSeparator'))
            })
          }}
        </li>
      </ul>
      <template #footer>
        <Button type="button" variant="outline" @click="showBlankCellsDialog = false">{{
          t('common.cancel')
        }}</Button>
        <Button type="button" variant="outline" @click="confirmBlankCellsAndRun">{{
          t('runner.blankCellsConfirm')
        }}</Button>
      </template>
    </Dialog>
  </div>
</template>

<style scoped>
.run-mode-tab {
  border-radius: 0.25rem;
  padding: 0.4rem 0.7rem;
  color: var(--app-muted-foreground);
  font-size: 0.75rem;
  font-weight: 600;
  transition:
    background-color 150ms ease,
    color 150ms ease;
}
.run-mode-tab:hover {
  color: var(--app-foreground);
}
.run-mode-tab.is-active {
  background: var(--app-card);
  color: var(--app-foreground);
  box-shadow: var(--shadow-xs);
}
.batch-variable-toggle {
  display: inline-flex;
  align-items: center;
  gap: 0.3rem;
  color: var(--app-muted-foreground);
  font-size: 0.6875rem;
  white-space: nowrap;
  cursor: pointer;
}
.batch-variable-toggle:hover {
  color: var(--app-foreground);
}
.batch-variable-toggle input {
  width: 0.875rem;
  height: 0.875rem;
  accent-color: var(--app-primary);
}
.batch-rows-panel {
  display: grid;
  gap: 0.875rem;
  border: 1px solid var(--app-border);
  border-radius: var(--radius-md);
  background: color-mix(in srgb, var(--app-card) 72%, var(--app-secondary));
  padding: 0.875rem;
}
.batch-summary-bar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.375rem 1rem;
  border: 1px solid var(--app-border);
  border-radius: var(--radius-md);
  background: var(--app-card);
  padding: 0.5rem 0.75rem;
}
.batch-summary-item {
  display: inline-flex;
  align-items: center;
  gap: 0.3rem;
  color: var(--app-muted-foreground);
  font-size: 0.75rem;
}
.batch-summary-item strong {
  color: var(--app-foreground);
  font-variant-numeric: tabular-nums;
}
.batch-summary-dot {
  width: 0.5rem;
  height: 0.5rem;
  border-radius: 9999px;
  background: currentColor;
}
.batch-summary-item.is-neutral {
  color: var(--app-muted-foreground);
}
.batch-summary-item.is-info {
  color: var(--app-info);
}
.batch-summary-item.is-success {
  color: var(--app-success);
}
.batch-summary-item.is-error {
  color: var(--app-destructive);
}
.batch-summary-total {
  margin-left: auto;
  color: var(--app-muted-foreground);
  font-size: 0.6875rem;
}
.batch-dialog-input {
  width: 100%;
  height: 2.25rem;
  border: 1px solid var(--app-border);
  border-radius: var(--radius-sm);
  background: var(--app-secondary);
  padding: 0 0.625rem;
  font-size: 0.8125rem;
  outline: none;
}
.batch-dialog-input:focus-visible {
  border-color: var(--app-ring, var(--app-primary));
}
.batch-table-scroll {
  overflow-x: auto;
}
.batch-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 0.8125rem;
}
.batch-table th,
.batch-table td {
  border: 1px solid var(--app-border);
  padding: 0.5rem 0.625rem;
  text-align: left;
  vertical-align: top;
}
.batch-table th {
  background: color-mix(in srgb, var(--app-secondary) 70%, transparent);
  color: var(--app-muted-foreground);
  font-size: 0.6875rem;
  font-weight: 600;
  white-space: nowrap;
}
.batch-index-column {
  width: 3rem;
  color: var(--app-muted-foreground);
  font-variant-numeric: tabular-nums;
}
.batch-status-column {
  width: 6.5rem;
  white-space: nowrap;
}
.batch-actions-column {
  width: 7.5rem;
  white-space: nowrap;
}
.batch-cell-input {
  width: 100%;
  min-width: 8rem;
  height: 2rem;
  border: 1px solid var(--app-border);
  border-radius: var(--radius-sm);
  background: var(--app-secondary);
  padding: 0 0.5rem;
  font-size: 0.8125rem;
  outline: none;
}
.batch-cell-input:focus-visible {
  border-color: var(--app-ring, var(--app-primary));
}
.batch-cell-select {
  width: 100%;
  min-width: 6rem;
  height: 2rem;
  border: 1px solid var(--app-border);
  border-radius: var(--radius-sm);
  background: var(--app-secondary);
  padding: 0 0.5rem;
  font-size: 0.8125rem;
  outline: none;
}
.batch-cell-file {
  display: grid;
  gap: 0.25rem;
  min-width: 9rem;
  padding: 0.375rem;
  margin: -0.375rem;
  border-radius: var(--radius-sm);
  transition:
    background-color 120ms ease,
    outline-color 120ms ease;
}
.batch-cell-file.is-dragging {
  outline: 2px dashed var(--app-primary);
  outline-offset: 2px;
  background: color-mix(in srgb, var(--app-primary) 7%, transparent);
}
</style>
