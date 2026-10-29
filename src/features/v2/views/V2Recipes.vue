<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  useV2WorkspaceStore,
  type ImportedRecipeResult,
  type V2ComfyFieldPreview,
  type V2Recipe
} from '@/features/v2/stores/workspace'
import Card from '@/components/ui/Card.vue'
import Button from '@/components/ui/Button.vue'
import Dialog from '@/components/ui/Dialog.vue'
import Input from '@/components/ui/Input.vue'
import Textarea from '@/components/ui/Textarea.vue'
import Badge from '@/components/ui/Badge.vue'
import IconButton from '@/components/ui/IconButton.vue'
import AppCover from '@/components/shared/AppCover.vue'
import ConfirmDialog from '@/components/shared/ConfirmDialog.vue'
import EmptyState from '@/components/shared/EmptyState.vue'
import FormField from '@/components/shared/FormField.vue'
import PageContainer from '@/components/layout/PageContainer.vue'
import PageHeader from '@/components/PageHeader.vue'
import NoticePanel from '@/components/shared/NoticePanel.vue'
import { stripHtml } from '@/lib/utils'
import { V2ApiError } from '@/shared/api/v2'
import { recipeTypeKey, recipeTypeLabel, type RecipeTypeKey } from '@/shared/lib/recipeType'
import { timestampOf } from '@/shared/lib/relativeTime'
import { useLocale } from '@/shared/composables/useLocale'
import {
  ArchiveRestore,
  ArrowRight,
  Boxes,
  Download,
  FolderOpen,
  Search,
  Star,
  Pencil,
  Trash2,
  Upload
} from '@lucide/vue'

const { t, dateLocale } = useLocale()
const route = useRoute()
const router = useRouter()
const workspace = useV2WorkspaceStore()

// ---------- 资源库展示：搜索 / 筛选 / 排序 / 分组 ----------
const query = ref('')
const appScope = ref<'all' | 'favorites'>('all')
const assetType = ref<'all' | RecipeTypeKey>('all')
const sortBy = ref<'recent' | 'added' | 'name'>('recent')
const selectedGroup = ref('')

// 类型判别与显示名的**唯一权威**在 shared/lib/recipeType（首页也用它，别再各写一份）。
const typeLabel = recipeTypeLabel
const connectionLabel = (recipe: V2Recipe) => {
  const connection = workspace.connections.find(
    (item) => item.connectionId === recipe.defaultConnectionId
  )
  return connection?.displayName ?? t('library.noConnection')
}
/**
 * 卡片上真正给人看的标签。
 *
 * 导入路径会把**驱动名**塞进 `labels`（`"runninghub"` / `"comfyui"`）当溯源标记——那是给我们
 * 自己看的内部值，直接渲染出来就是一行小写 key，而且和左边的类型标签说的是同一件事。
 * 这里只在**显示**时滤掉，数据不动（搜索仍能命中它，见上）。
 */
const visibleLabels = (recipe: V2Recipe) => {
  const driverSegments = (recipe.driverKey ?? '').split('/')
  return (recipe.labels ?? []).filter((label) => !driverSegments.includes(label))
}
const availableGroups = computed(() => {
  const groups = new Set<string>()
  workspace.recipes.forEach((recipe) => {
    if (recipe.group) groups.add(recipe.group)
  })
  return [...groups]
})
const hasFilters = computed(
  () =>
    Boolean(query.value.trim()) ||
    appScope.value === 'favorites' ||
    assetType.value !== 'all' ||
    Boolean(selectedGroup.value)
)
const filteredRecipes = computed(() => {
  const term = query.value.trim().toLowerCase()
  const list = workspace.recipes
    .filter((recipe) => appScope.value !== 'favorites' || recipe.favorite)
    .filter((recipe) => assetType.value === 'all' || recipeTypeKey(recipe) === assetType.value)
    .filter((recipe) => !selectedGroup.value || recipe.group === selectedGroup.value)
    .filter(
      (recipe) =>
        !term ||
        [
          recipe.name,
          recipe.description,
          recipe.note,
          recipe.group,
          // 类型标签也进搜索：用户会搜「工作流」「RH 应用」，那是他眼睛看到的字。
          typeLabel(recipe),
          ...(recipe.labels ?? [])
        ]
          .filter(Boolean)
          .join(' ')
          .toLowerCase()
          .includes(term)
    )
  return [...list].sort((a, b) => {
    if (sortBy.value === 'name') return a.name.localeCompare(b.name, 'zh')
    const added = (value: V2Recipe) => value.createdAt ?? value.lastOpenedAt ?? ''
    if (sortBy.value === 'added') return added(b).localeCompare(added(a))
    const recent = (value: V2Recipe) => value.lastOpenedAt ?? value.createdAt ?? ''
    return recent(b).localeCompare(recent(a))
  })
})
const clearFilters = () => {
  query.value = ''
  appScope.value = 'all'
  assetType.value = 'all'
  sortBy.value = 'recent'
  selectedGroup.value = ''
}

const toggleFavorite = (recipe: V2Recipe) => {
  void workspace.toggleFavorite(recipe.recipeId).catch(() => {})
}

// ---------- 编辑 / 删除 ----------
const editingRecipe = ref<V2Recipe>()
/**
 * 这个编辑框是从**运行页**过来的吗（带着 `?edit=<id>`）？
 * 记住是谁，关掉时才能"从哪来、回哪去"（见 `closeEdit`）。
 */
const editFromRunnerId = ref<number | null>(null)
const editForm = reactive({
  note: '',
  group: '',
  /**
   * 字段备注：一行一个输入框。
   *
   * ⚠️ 为什么需要它：RunningHub 的 schema 里 `description` **就等于字段名**
   * （实测 `"874::text": {"description": "text"}`）——平台没给语义，Agent 和用户
   * 看到的都是读不出意思的键。语义只能由人补一次。
   *
   * 预填用 **recipe 上存的原始备注**，不是 schema 里合并后的 description——
   * 后者含平台给的字段名，拿它当"用户填过的值"会让用户以为自己写过。
   */
  fields: [] as Array<{
    key: string
    label: string
    /**
     * 这一列是**用户自己的决定**，默认空 = 保持现状（我们建议的 / 引擎给的）。
     *
     * ⚠️ 刻意不把"现在的控件"预填进这个下拉：那样用户没动它、一保存就会把它变成
     * `source = user`——**把我们/引擎定的东西冒充成他定的**。现状用只读文字标在旁边。
     */
    widget: string
    options: string
    /** 现状是谁定的（`user`/`engine`/`suggested`），只用于如实标注。 */
    source: string
  }>
})
const editLoadingFields = ref(false)
/**
 * 这次编辑是**刚导入完自动打开的**（而不是用户点「编辑」）。
 *
 * 为什么要自动打开：导入进来的字段名往往就是键本身——AI 应用的 `description` 实测就等于字段名
 * （`"874::text": {"description": "text"}`），而**工作流连这个都没有**（只有 `3::cfg`、`5::width`）。
 * 平台没给语义，运行表单和 agent 看到的都是读不出意思的键。**语义只能由人补一次**，
 * 而"刚导完、还看着平台那一页"正是最该补的时候——所以顺手把人送到这个对话框前，跳过也随意。
 */
const editAfterImport = ref(false)
const openEdit = async (recipe: V2Recipe, afterImport = false) => {
  editingRecipe.value = recipe
  editAfterImport.value = afterImport
  editForm.note = recipe.note ?? ''
  editForm.group = recipe.group ?? ''
  editForm.fields = []
  editLoadingFields.value = true
  try {
    const schema = await workspace.fetchRecipeSchema(recipe.recipeId)
    const stored = recipe.fieldLabels ?? {}
    editForm.fields = Object.keys(schema.properties ?? {}).map((key) => ({
      key,
      label: stored[key] ?? '',
      widget: '',
      options: '',
      source: String(schema.properties?.[key]?.['x-widget-source'] ?? '')
    }))
  } catch {
    // 取不到 schema 不该挡住"改备注/分组"这件本来就能做的事。
    editForm.fields = []
  } finally {
    editLoadingFields.value = false
  }
}
/**
 * 用户在下拉里选了控件的那一刻：如果选的是「下拉」，就地提示他填候选项
 * （没填会被后端退回文本——那是"说了但做不到"的唯一情形，界面上先别让他白选）。
 */
const onEditWidgetChange = (field: { widget: string; options: string }) => {
  if (field.widget !== 'dropdown') field.options = ''
}

const saveEdit = async () => {
  if (!editingRecipe.value) return
  // 整体替换：空字符串即"这条没备注"，所以只提交填了的。
  const fieldLabels: Record<string, string> = {}
  for (const field of editForm.fields) {
    const label = field.label.trim()
    if (label) fieldLabels[field.key] = label
  }
  // 只提交**用户真改过**的那些控件（空 = 保持现状）。
  const fieldWidgets = editForm.fields
    .filter((field) => field.widget)
    .map((field) => ({
      key: field.key,
      widget: field.widget,
      options:
        field.widget === 'dropdown'
          ? field.options
              .split(/[,，\n]/)
              .map((option) => option.trim())
              .filter(Boolean)
          : []
    }))
  await workspace.updateRecipe(editingRecipe.value.recipeId, {
    note: editForm.note.trim() || undefined,
    group: editForm.group.trim() || undefined,
    fieldLabels,
    ...(fieldWidgets.length ? { fieldWidgets } : {})
  })
  closeEdit()
}
/**
 * 关掉编辑框——**保存、取消、ESC、点遮罩、右上角 × 都走这里**。
 *
 * 从运行页过来的（`?edit=`）就**送回运行页**：那是"你从哪来、回哪去"，而不是
 * "跳走再跳回"——那边的表单和已经填好的值都还在（会话存在 `runner-sessions` 里），
 * 所以改完名字接着填就是了。从应用库卡片点进来的（以及导入后自动弹的那次）不留痕，
 * 关掉就还在原地。
 */
const closeEdit = () => {
  editingRecipe.value = undefined
  editAfterImport.value = false
  const returnTo = editFromRunnerId.value
  editFromRunnerId.value = null
  if (returnTo) void router.push({ name: 'V2Runner', params: { id: String(returnTo) } })
}

const removingRecipe = ref<V2Recipe>()

const removeRecipe = async () => {
  if (!removingRecipe.value) return
  try {
    await workspace.removeRecipe(removingRecipe.value.recipeId)
  } catch (caught) {
    workspace.error = caught instanceof Error ? caught.message : String(caught)
  } finally {
    // 失败时也关掉：错误已经显示在页面顶部的横幅里，卡着一个确认框只会挡住下一次尝试。
    removingRecipe.value = undefined
  }
}

// ---------- 已移除（墓碑）----------
//
// 移除是**打墓碑**而不是销毁：应用从资源库移开、Agent 也不再能调用它，但历史任务照旧可解析，
// 而且随时能恢复。所以界面上要能**看见**这一批（"显示已移除"）并把它们拿回来——
// 后端早就支持了，界面此前只能用 API 做这件事。
const showRemoved = ref(false)
const restoringId = ref<number>()
watch(showRemoved, (visible) => {
  // 按需加载：绝大多数会话都不需要看回收站。
  if (visible) void workspace.loadRemovedRecipes()
})
/** 「移除于 …」用**绝对时间**（回收站里的东西可能躺了几周，"48 小时前"读不出是哪天）。 */
const removedAtLabel = (value?: string | null) => {
  if (!value) return ''
  return new Date(timestampOf(value)).toLocaleString(dateLocale.value, {
    dateStyle: 'medium',
    timeStyle: 'short'
  })
}
const restoreRecipe = async (recipe: V2Recipe) => {
  restoringId.value = recipe.recipeId
  try {
    await workspace.restoreRecipe(recipe.recipeId)
  } catch (caught) {
    workspace.error = caught instanceof Error ? caught.message : String(caught)
  } finally {
    restoringId.value = undefined
  }
}

onMounted(async () => {
  await workspace.loadRecipes()
  await workspace.loadConnections()
  if (route.query.import === 'runninghub') {
    if (runningHubConnections.value.length) showImport.value = true
    else void router.replace('/connections')
  }
  // 运行页那行「编辑字段」带着 `?edit=<id>` 过来（用户是在运行页发现字段分不清的，
  // 改名字与控件的对话框长在这一页）。开完就**把 query 抹掉**，否则刷新或后退会反复弹；
  // 记住是谁来的 ⇒ 关掉编辑框时送回运行页（见 `closeEdit`）。
  if (route.query.edit) {
    const target = workspace.recipes.find((item) => item.recipeId === Number(route.query.edit))
    if (target) {
      editFromRunnerId.value = target.recipeId
      await openEdit(target)
    }
    void router.replace({ query: {} })
  }
})

// ---------- 高级创建 ----------
const showCreate = ref(false)
const creating = ref(false)
const createError = ref('')
const form = reactive({
  name: '',
  description: '',
  capability: 'text-to-image',
  driverKey: 'openai/openai-images',
  defaultConnectionId: '' as string,
  inputSchemaJson:
    '{\n  "type": "object",\n  "required": ["prompt"],\n  "properties": {\n    "prompt": { "type": "string" }\n  }\n}'
})

/** 已注册的执行驱动（与后端 DriverRegistry 保持一致）。 */
const DRIVER_KEYS = computed(() => [
  { value: 'runninghub/runninghub', label: t('library.driverRunningHub') },
  { value: 'comfyui/comfyui', label: t('library.driverComfyui') },
  { value: 'openai/openai-images', label: t('library.driverOpenAiImage') },
  { value: 'async/async-task', label: t('library.driverAsyncTask') }
])

/** 与所选 driverKey 匹配（vendor/protocol）的连接，避免驱动与连接对不上。 */
const compatibleConnections = computed(() =>
  workspace.connections.filter(
    (connection) =>
      connection.vendor &&
      connection.protocol &&
      `${connection.vendor}/${connection.protocol}` === form.driverKey
  )
)

const createRecipe = async () => {
  creating.value = true
  createError.value = ''
  try {
    const connectionId = Number(form.defaultConnectionId)
    if (!connectionId) throw new Error(t('library.errNoConnection'))
    let inputSchema: Record<string, unknown>
    try {
      inputSchema = JSON.parse(form.inputSchemaJson) as Record<string, unknown>
    } catch {
      throw new Error(t('library.errInvalidSchema'))
    }
    await workspace.createRecipe({
      name: form.name.trim(),
      capability: form.capability.trim(),
      driverKey: form.driverKey.trim(),
      defaultConnectionId: connectionId,
      inputSchema,
      description: form.description.trim()
    })
    showCreate.value = false
    form.name = ''
    form.description = ''
  } catch (caught) {
    createError.value = caught instanceof Error ? caught.message : String(caught)
  } finally {
    creating.value = false
  }
}

// ---------- RunningHub 导入 ----------
const showImport = ref(false)
const importing = ref(false)
const importError = ref('')
const importForm = reactive({
  connectionId: '' as string,
  source: '',
  name: '',
  /**
   * 他指定的**导入形态**（空串 = 自动）。
   *
   * 为什么给他这个开关：自动那条路的判据只有**链接路径**，而裸 ID 连路径都没有——
   * 实测两个 id 都是 19 位数字、没有任何类型位（连平台自己的发号器都不带标记），
   * 所以"这条 id 是应用还是工作流"这件事，**他可能知道，我们一定判不出来**。
   * 这不是让他帮我们猜，而是这件事他有权说了算：指定了就只走那一条。
   */
  kind: '' as '' | 'app' | 'workflow'
})

/**
 * 打开导入对话框时：**只有一条可用连接就替你选中**。
 *
 * 直觉上"我只有一个 RunningHub 账号"不该出现一道选择题。两条以上（.cn / .ai 各一条）时才需要
 * 你选，那时默认名已经把两者分开了（「（国内）/（国际）」）。
 *
 * ⚠️ 刻意**不**默认选第一条：两条连接是**两个账号**，默认选错等于把任务发到另一个账号上，
 * 那比让你多点一下更糟。
 */
watch(showImport, (open) => {
  if (open && !importForm.connectionId && runningHubConnections.value.length === 1) {
    importForm.connectionId = String(runningHubConnections.value[0].connectionId)
  }
})

const runningHubConnections = computed(() => {
  if (!workspace.connectionsLoaded || workspace.connectionsLoading) return []
  return workspace.connections.filter(
    (connection) =>
      connection.protocol === 'runninghub' &&
      connection.status === 'active' &&
      connection.detailsAvailable === true &&
      connection.healthOk === true &&
      Boolean(connection.baseUrl?.trim()) &&
      connection.apiKeyConfigured === true
  )
})
const hasIncompleteRunningHubConnections = computed(() =>
  workspace.connections.some(
    (connection) =>
      connection.protocol === 'runninghub' &&
      (!connection.baseUrl || connection.apiKeyConfigured !== true)
  )
)

/**
 * 这条链接是平台的「AI 应用」还是「工作流」？——判据是**链接路径**，不是让用户选。
 *
 * 实测的形状（README 里也记过）：
 * - `/post/<id>`            → 工作流（创作者发在社区的那一份）
 * - `/ai-detail/<id>`       → AI 应用
 * ⚠️ 这只是**先试哪个**的偏好，**判官是平台**（见下面 `importFromRunningHub`）：链接路径是提示，
 * 不是权威。README 只认两种形状——`…/ai-detail/<id>`（AI 应用）与 `…/post/<id>`（工作流）；
 * `…/workflow/detail/<id>` 实测**判不出来**（服务端不校验 id，只回一个 SPA 壳，应用页与工作流页
 * 都可能是它），所以那里只当"先按应用试"。
 *
 * 裸 ID 同理 ⇒ `'unknown'`，两种都试。
 */
const detectRhSourceKind = (source: string): 'app' | 'workflow' | 'unknown' => {
  const path = source.trim().toLowerCase().split(/[?#]/)[0]
  if (path.includes('/post/')) return 'workflow'
  if (path.includes('/ai-detail/')) return 'app'
  return 'unknown'
}

/** 从链接里**读**出来的形态：能读就读，读不出来就说读不出来（不装懂）。 */
const detectedRhKind = computed(() => detectRhSourceKind(importForm.source))

/** 这次实际按哪种形态走：他指定的优先，没指定才用读出来的。 */
const effectiveRhKind = computed<'app' | 'workflow' | 'unknown'>(
  () => importForm.kind || detectedRhKind.value
)

/**
 * 他指定的形态和链接读出来的对不上 —— 照样按他说的走（他说了算），但要**先说**，
 * 别让他等一个必然的失败。
 */
const rhKindMismatch = computed(
  () =>
    Boolean(importForm.kind) &&
    detectedRhKind.value !== 'unknown' &&
    importForm.kind !== detectedRhKind.value
)

/**
 * 输入框下面那行（**只在粘了东西之后显示**，见模板里的 `v-if`）：只说**他手上这条链接是什么、
 * 要另一种该去哪儿拿**，不说我们的实现。读出来是哪一种，对应不同的"下一步"。
 */
const rhKindReadHint = computed(() => {
  if (detectedRhKind.value === 'app') return t('library.rhKindHintApp')
  if (detectedRhKind.value === 'workflow') return t('library.rhKindHintWorkflow')
  return t('library.rhKindHintUnknown')
})

/**
 * 平台"查无此物"式的拒绝（拿工作流 id 去查应用、或反过来，都会落到这两个码上）。
 *
 * 只有这类失败才值得换另一种再试一次：网络不通、密钥不对换一种也还是不通，
 * 那就该把**真正的**原因原样报出来。
 */
const isKindRejection = (error: unknown): boolean =>
  error instanceof V2ApiError &&
  ['RUNNINGHUB_WEBAPP_FETCH_REJECTED', 'RUNNINGHUB_WORKFLOW_FETCH_REJECTED'].includes(
    error.error.code
  )

/**
 * 一次导入尝试：形态决定走后端哪条路。
 *
 * 后端本来就是**两个入口**（`/recipes/import` 存 `webappId`、`/recipes/import-workflow` 存
 * `workflowId`），"先试哪条、被拒再换另一条"从头到尾都是**这里**的策略 —— 所以
 * "让用户指定形态"只需要改这一处，后端一行都不用动。
 */
const importAs = (
  connectionId: number,
  kind: 'app' | 'workflow',
  source: string,
  name?: string
): Promise<ImportedRecipeResult> =>
  kind === 'app'
    ? workspace.importRecipe(connectionId, source)
    : workspace.importWorkflowRecipe(connectionId, source, name)

/** 导入成功了：关窗、清表单，然后**只在该做的时候**请他给字段起名字。 */
const finishImport = async (result: ImportedRecipeResult) => {
  showImport.value = false
  importForm.connectionId = ''
  importForm.source = ''
  importForm.name = ''
  importForm.kind = ''
  // 导入是机械的一步；**给字段起名字**是下一步，但只在该做的时候做：
  // 平台给了名字（AI 应用的作者填了描述）就**直接用、不打扰**；没给才请你补一次——
  // 判据是"描述为空，或描述就等于字段名本身"（实测 `{name: "text", description: "text"}`）。
  //
  // ⚠️ 这里**不按形态分**：曾以为"工作流必然要起名"（`getJsonApiFormat` 只回那张图，
  // 字段是 `3::cfg` 这种键），实测不成立——节点标题会变成我们的默认描述
  //（`加载图像 · image（节点 207）`），那不等于字段名，于是**不弹**；反倒是有的 AI 应用
  // 作者没写描述、描述就等于字段名（`image`），**要弹**。所以判据只看描述本身。
  const needsNaming = (result.fields ?? []).some((field) => {
    const description = (field.description ?? '').trim()
    return !description || description === (field.name ?? '').trim()
  })
  const created = workspace.recipes.find((item) => item.recipeId === result.recipeId)
  if (needsNaming && created) await openEdit(created, true)
}

const importFromRunningHub = async () => {
  const connectionId = Number(importForm.connectionId)
  if (!connectionId) {
    importError.value = t('library.errSelectRhConnection')
    return
  }
  const source = importForm.source.trim()
  if (!source) {
    importError.value = t('library.errEnterRhSource')
    return
  }
  importing.value = true
  importError.value = ''
  // 名字只对**工作流**那条路有用：AI 应用的默认名来自平台（比用户自己起的更准）。
  // 所以形态判定为应用时，界面上那个字段**根本不显示**——不给一个填了没用的框。
  const name = importForm.name.trim() || undefined
  try {
    // 他指定了形态 ⇒ **只走那一条**。被平台以"查无此物"拒了也不换路：换了就等于推翻他。
    // 代价（他得自己把对的那条链接拿来）由报错那句话补上——那里正好是「打开AI应用」
    // 这个按钮名唯一有用的时刻。
    const chosen = importForm.kind
    if (chosen) {
      try {
        await finishImport(await importAs(connectionId, chosen, source, name))
      } catch (error) {
        if (!isKindRejection(error)) throw error
        importError.value = t(
          chosen === 'app' ? 'library.errRhChosenAppMissing' : 'library.errRhChosenWorkflowMissing',
          { message: error instanceof Error ? error.message : String(error) }
        )
      }
      return
    }
    // 没指定 ⇒ 链接路径只决定**先试哪条路**，判官是平台：两条路都不花钱，被"查无此物"拒了就换另一条。
    // 这不是为了兜底裸 ID，而是因为**用户是从 RH 网站上复制链接过来的**——平台上同一个东西
    // 可能有好几种 URL 形状（README 只认 `/ai-detail/` 与 `/post/`，而 `/workflow/detail/`
    // 实测既可能是应用页也可能是工作流页）。与其猜，不如让平台说了算。
    const order: Array<'app' | 'workflow'> =
      detectedRhKind.value === 'workflow' ? ['workflow', 'app'] : ['app', 'workflow']
    const failures: Array<{ attempt: 'app' | 'workflow'; error: unknown }> = []
    for (const attempt of order) {
      try {
        await finishImport(await importAs(connectionId, attempt, source, name))
        return
      } catch (error) {
        // 网络不通 / 密钥不对：换一条路也还是不通，别拿第二次失败盖住真正的原因。
        if (!isKindRejection(error)) throw error
        failures.push({ attempt, error })
      }
    }
    // 两条路都被平台以"查无此物"拒了 —— 把两次的原因都摆出来，那是唯一的线索。
    const messageOf = (attempt: 'app' | 'workflow') => {
      const failure = failures.find((item) => item.attempt === attempt)?.error
      return failure instanceof Error ? failure.message : String(failure)
    }
    importError.value = t('library.errRhSourceNeither', {
      app: messageOf('app'),
      workflow: messageOf('workflow')
    })
  } catch (caught) {
    importError.value = caught instanceof Error ? caught.message : String(caught)
  } finally {
    importing.value = false
  }
}

// ---------- ComfyUI API Prompt 导入 ----------
const showComfyUIImport = ref(false)
const comfyImporting = ref(false)
const comfyImportError = ref('')
const comfyStep = ref<'paste' | 'select'>('paste')
const comfyName = ref('')
const comfyPrompt = ref<Record<string, unknown> | null>(null)
const comfyImportForm = reactive({
  connectionId: '' as string,
  promptJson: ''
})
interface ComfyFieldRow extends V2ComfyFieldPreview {
  selected: boolean
  label: string
  /** 用户选的控件；空串 = 默认（不覆盖，让引擎声明/值的形状决定）。 */
  widget: string
  /** 选了「下拉」时他填的候选项（逗号分隔的原文）。 */
  options: string
}
const comfyFieldRows = ref<ComfyFieldRow[]>([])
const comfySelectedCount = computed(() => comfyFieldRows.value.filter((row) => row.selected).length)
const promptPlaceholder = computed(
  () => `${t('library.comfyPromptPlaceholder')}
{
  "6": {
    "class_type": "CLIPTextEncode",
    "inputs": { "text": "a red house" }
  },
  "3": {
    "class_type": "KSampler",
    "inputs": { "seed": 42, "steps": 20, "positive": ["6", 0] }
  }
}`
)

const comfyUiConnections = computed(() => {
  if (!workspace.connectionsLoaded || workspace.connectionsLoading) return []
  return workspace.connections.filter(
    (connection) => connection.protocol === 'comfyui' && connection.status === 'active'
  )
})

const rhConnections = computed(() => {
  if (!workspace.connectionsLoaded || workspace.connectionsLoading) return []
  return workspace.connections.filter(
    (connection) => connection.protocol === 'runninghub' && connection.status === 'active'
  )
})

const comfyWorkflowSource = ref('')
const comfyFetching = ref(false)
/**
 * 这次预览/导入**从引擎那儿拿到了多少**。
 *
 * 拿不到时我们会静默退回通用控件（字符串→文本框、数字→没有范围的框），
 * 用户看到的只是"这表单有点糙"。把原因显示出来，他才知道"把 ComfyUI 起起来重新导入"
 * 能变好——不然这就是个看不见的降级。
 */
const comfyEnrichment = ref<{ classesUsed: number; classesKnown: number; reason: string | null }>()

/**
 * 从**文件**读入 API Prompt。
 *
 * 为什么要有这条路：源物件本身就是磁盘上的一个文件（ComfyUI「保存(API 格式)」导出的
 * `.json`）。只给一个粘贴框，等于要求用户"打开记事本 → 全选 → 复制 → 回到这里粘贴"——
 * 白加的三步。读完仍然填进那个 textarea（**粘贴照旧可用**，而且用户能看一眼再点下一步），
 * 所以后面的流程一步都不用改。
 *
 * 判据：源是文件就给文件入口，源是文本/链接就只给文本框（同一个对话框里那个
 * RunningHub 链接就没加——它天生是从地址栏复制的文本）。
 */
const comfyPromptFileInput = ref<HTMLInputElement>()
const comfyPromptFileName = ref('')

const readComfyPromptFile = async (file: File | undefined) => {
  if (!file) return
  comfyImportError.value = ''
  // 上限对齐后端那条（超过就别读进内存了；前端先拦一句比等后端报错清楚）。
  const limit = 4 * 1024 * 1024
  if (file.size > limit) {
    comfyImportError.value = t('library.errComfyFileTooLarge', { mb: 4 })
    return
  }
  try {
    const text = await file.text()
    // 先在本地判一次格式，用户**不用等一次往返**就知道自己导错了菜单项；
    // 真判据在后端（CLI/MCP 也走那儿），这里只是把同一件事提前一秒说出来。
    const parsed: unknown = JSON.parse(text)
    if (looksLikeUiWorkflow(parsed)) {
      comfyImportError.value = t('library.errComfyUiFormat')
      return
    }
    comfyImportForm.promptJson = JSON.stringify(parsed, null, 2)
    comfyPromptFileName.value = file.name
  } catch {
    comfyImportError.value = t('library.errInvalidComfyJson')
  }
}

/** 只用来"提前一秒提醒"：`nodes` 是数组且同时有 `links` = ComfyUI 点「保存」导出的那种。 */
const looksLikeUiWorkflow = (value: unknown): boolean => {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false
  const object = value as Record<string, unknown>
  return Array.isArray(object.nodes) && 'links' in object
}

const onComfyPromptFileChange = async (event: Event) => {
  const input = event.target as HTMLInputElement
  await readComfyPromptFile(input.files?.[0])
  // 清空 value：同一个文件改完再选一次也要能触发 change。
  input.value = ''
}

/** 拖放：桌面外壳里默认**收不到**（Tauri 注册了自己的 drop handler 并把网页那一层关掉了），
 *  所以这条路只在浏览器里有效，文案上不要承诺它——见 dialog 里的说明。 */
const comfyPromptDragging = ref(false)
const onComfyPromptDrop = async (event: DragEvent) => {
  comfyPromptDragging.value = false
  const file = event.dataTransfer?.files?.[0]
  if (!file) return
  await readComfyPromptFile(file)
}

const fetchRhWorkflow = async () => {
  const rhConnection = rhConnections.value[0]
  if (!rhConnection) {
    comfyImportError.value = t('library.errNoRhConnection')
    return
  }
  if (!comfyWorkflowSource.value.trim()) {
    comfyImportError.value = t('library.errEnterRhWorkflow')
    return
  }
  if (!Number(comfyImportForm.connectionId)) {
    comfyImportError.value = t('library.errSelectComfyConnection')
    return
  }
  comfyFetching.value = true
  comfyImportError.value = ''
  try {
    const prompt = await workspace.fetchComfyUIWorkflow(
      rhConnection.connectionId,
      comfyWorkflowSource.value.trim()
    )
    comfyImportForm.promptJson = JSON.stringify(prompt, null, 2)
    await previewComfyImport()
  } catch (caught) {
    comfyImportError.value = caught instanceof Error ? caught.message : String(caught)
  } finally {
    comfyFetching.value = false
  }
}

const parseComfyPrompt = (): Record<string, unknown> | null => {
  if (!comfyImportForm.promptJson.trim()) {
    comfyImportError.value = t('library.errEnterComfyPrompt')
    return null
  }
  try {
    return JSON.parse(comfyImportForm.promptJson) as Record<string, unknown>
  } catch {
    comfyImportError.value = t('library.errInvalidComfyJson')
    return null
  }
}

const previewComfyImport = async () => {
  if (!Number(comfyImportForm.connectionId)) {
    comfyImportError.value = t('library.errSelectComfyConnection')
    return
  }
  const prompt = parseComfyPrompt()
  if (!prompt) return
  comfyImporting.value = true
  comfyImportError.value = ''
  try {
    // 带上连接：后端会**尽力**问一次 ComfyUI 的节点定义，好把通用控件升级成真控件
    // （枚举→下拉、数字→带范围、多行→多行框）。拿不到照样能导入，只是控件普通。
    const { fields, enrichment } = await workspace.previewComfyUIFields(
      prompt,
      Number(comfyImportForm.connectionId)
    )
    comfyEnrichment.value = enrichment
    comfyPrompt.value = prompt
    comfyFieldRows.value = fields.map((field) => ({
      ...field,
      selected: field.suggested,
      label: '',
      // 控件默认不覆盖：引擎声明 > 值的形状 > 我们的建议（顺序在后端一处决定）。
      widget: '',
      options: ''
    }))
    comfyStep.value = 'select'
  } catch (caught) {
    comfyImportError.value = caught instanceof Error ? caught.message : String(caught)
  } finally {
    comfyImporting.value = false
  }
}

const importFromComfyUI = async () => {
  const connectionId = Number(comfyImportForm.connectionId)
  if (!connectionId) {
    comfyImportError.value = t('library.errSelectComfyConnection')
    return
  }
  const prompt = comfyPrompt.value
  if (!prompt) {
    comfyImportError.value = t('library.errParseComfyFirst')
    return
  }
  if (!comfySelectedCount.value) {
    comfyImportError.value = t('library.errSelectField')
    return
  }
  comfyImporting.value = true
  comfyImportError.value = ''
  try {
    await workspace.importComfyUIRecipe(connectionId, prompt, {
      name: comfyName.value.trim() || undefined,
      fields: comfyFieldRows.value
        .filter((row) => row.selected)
        .map((row) => ({
          key: row.key,
          label: row.label.trim() || undefined,
          // 不选就一个字段都不发 ⇒ 后端按"用户没覆盖"处理（引擎/值的形状说了算）。
          widget: row.widget || undefined,
          options:
            row.widget === 'dropdown'
              ? row.options
                  .split(/[,，\n]/)
                  .map((option) => option.trim())
                  .filter(Boolean)
              : undefined
        }))
    })
    showComfyUIImport.value = false
    resetComfyImport()
  } catch (caught) {
    comfyImportError.value = caught instanceof Error ? caught.message : String(caught)
  } finally {
    comfyImporting.value = false
  }
}

const resetComfyImport = () => {
  comfyStep.value = 'paste'
  comfyName.value = ''
  comfyPrompt.value = null
  comfyFieldRows.value = []
  comfyImportError.value = ''
  comfyImportForm.connectionId = ''
  comfyImportForm.promptJson = ''
  comfyWorkflowSource.value = ''
  comfyFetching.value = false
}

watch(showComfyUIImport, (open) => {
  if (!open) resetComfyImport()
})

// ---------- 应用库的导出 / 导入 ----------
// 只搬「应用」这一个资产：名字、分组、备注名、控件、平台类型。
// 任务历史、产物、连接与密钥都不在包里（整机搬家是 `workspace backup` 的事）。
const libraryBusy = ref<'export' | 'import' | ''>('')
const libraryNotice = ref<{ variant: 'success' | 'info' | 'destructive'; text: string } | null>(
  null
)
const libraryFileInput = ref<HTMLInputElement | null>(null)

const libraryErrorText = (error: unknown) =>
  error instanceof V2ApiError ? error.message : String(error)

const exportLibrary = async () => {
  libraryBusy.value = 'export'
  libraryNotice.value = null
  try {
    const report = await workspace.exportLibrary()
    libraryNotice.value = {
      variant: 'success',
      text: t('library.exportDone', { count: report.recipeCount, path: report.path })
    }
  } catch (error) {
    libraryNotice.value = { variant: 'destructive', text: libraryErrorText(error) }
  } finally {
    libraryBusy.value = ''
  }
}

const pickLibraryFile = () => {
  libraryNotice.value = null
  libraryFileInput.value?.click()
}

const onLibraryFileChange = async (event: Event) => {
  const input = event.target as HTMLInputElement
  const file = input.files?.[0]
  // 清空 value：否则再选**同一个文件**不会触发 change，用户重试时会以为按钮没反应。
  input.value = ''
  if (!file) return

  libraryBusy.value = 'import'
  try {
    const content = await file.text()
    const report = await workspace.importLibrary(content)
    if (!report.imported.length && !report.skipped.length && !report.failed.length) {
      libraryNotice.value = { variant: 'info', text: t('library.importNothing') }
      return
    }
    const summary = t('library.importDone', {
      imported: report.imported.length,
      skipped: report.skipped.length,
      failed: report.failed.length
    })
    // 原因**必须露出来**：最常见的一条是"本机没有这个平台的连接"（`IMPORT_CONNECTION_MISSING`），
    // 只说"失败 1 个"等于把排查丢给用户。
    const firstProblem = report.failed[0]?.reason ?? report.skipped[0]?.reason
    libraryNotice.value = {
      variant: report.failed.length ? 'destructive' : 'success',
      text: firstProblem ? `${summary} · ${firstProblem}` : summary
    }
  } catch (error) {
    libraryNotice.value = { variant: 'destructive', text: libraryErrorText(error) }
  } finally {
    libraryBusy.value = ''
  }
}
</script>

<template>
  <PageContainer>
    <PageHeader
      :eyebrow="t('common.eyebrowLibrary')"
      :title="t('library.title')"
      :description="t('library.description')"
    >
      <template #actions>
        <Button
          v-if="comfyUiConnections.length"
          variant="outline"
          @click="showComfyUIImport = true"
        >
          {{ t('library.addFromComfy') }}
        </Button>
        <RouterLink
          v-if="!runningHubConnections.length"
          to="/connections"
          class="inline-flex h-9 items-center justify-center rounded-md border border-input bg-secondary px-3.5 text-sm font-medium transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
        >
          {{ t('library.configureRunningHub') }}
        </RouterLink>
        <Button v-else variant="outline" @click="showImport = true">{{
          t('library.addFromRunningHub')
        }}</Button>
        <Button variant="ghost" @click="showCreate = true">{{
          t('library.advancedCreate')
        }}</Button>
      </template>
    </PageHeader>

    <NoticePanel class="mt-6" variant="info">
      <p class="font-medium">{{ t('library.noticeTitle') }}</p>
      <p class="mt-1 text-xs text-muted-foreground">
        {{ t('library.noticeBody') }}
      </p>
    </NoticePanel>

    <p
      v-if="workspace.error"
      class="mt-6 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
    >
      {{ workspace.error }}
    </p>

    <section
      class="mt-6 flex flex-col gap-3 border-b border-border pb-4 lg:flex-row lg:items-center lg:justify-between"
    >
      <div class="flex flex-wrap items-center gap-3">
        <p v-if="workspace.recipes.length" class="text-sm text-muted-foreground">
          {{ t('library.count', { count: filteredRecipes.length }) }}
        </p>
        <!-- 这个开关**永远可见**：库里一个应用都不剩时，它正是把应用拿回来的入口。 -->
        <Button
          variant="outline"
          size="sm"
          :aria-pressed="showRemoved"
          @click="showRemoved = !showRemoved"
        >
          <ArchiveRestore class="size-3.5" />
          {{ showRemoved ? t('library.hideRemoved') : t('library.showRemoved') }}
        </Button>
        <!-- 导出/导入的是**应用库本身**（名字、分组、备注名、控件），不是任务历史。 -->
        <Button
          variant="outline"
          size="sm"
          :disabled="libraryBusy !== '' || !workspace.recipes.length"
          @click="exportLibrary"
        >
          <Download class="size-3.5" />
          {{ libraryBusy === 'export' ? t('library.exporting') : t('library.exportLibrary') }}
        </Button>
        <Button variant="outline" size="sm" :disabled="libraryBusy !== ''" @click="pickLibraryFile">
          <Upload class="size-3.5" />
          {{ libraryBusy === 'import' ? t('library.importing') : t('library.importLibrary') }}
        </Button>
        <input
          ref="libraryFileInput"
          type="file"
          accept=".json,application/json"
          class="hidden"
          @change="onLibraryFileChange"
        />
      </div>
      <div v-if="workspace.recipes.length" class="grid w-full gap-2 sm:grid-cols-4 lg:w-auto">
        <div class="relative sm:col-span-1">
          <Search
            class="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
          />
          <Input v-model="query" class="pl-9" :placeholder="t('library.searchPlaceholder')" />
        </div>
        <select
          v-model="appScope"
          class="h-9 rounded-md border border-input bg-secondary px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <option value="all">{{ t('common.all') }}</option>
          <option value="favorites">{{ t('library.scopeFavorites') }}</option>
        </select>
        <select
          v-model="assetType"
          class="h-9 rounded-md border border-input bg-secondary px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <option value="all">{{ t('library.assetTypeAll') }}</option>
          <!-- 筛的也是"这是什么"（种类），不是驱动：用户找东西时想的是"我那个工作流"。 -->
          <option value="rh-app">{{ t('library.kindRhApp') }}</option>
          <option value="rh-workflow">{{ t('library.kindRhWorkflow') }}</option>
          <option value="comfyui-workflow">{{ t('library.kindComfyWorkflow') }}</option>
          <option value="openai-image">{{ t('library.kindOpenAiImage') }}</option>
          <option value="other">{{ t('library.kindOther') }}</option>
        </select>
        <select
          v-model="sortBy"
          class="h-9 rounded-md border border-input bg-secondary px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <option value="recent">{{ t('library.sortRecent') }}</option>
          <option value="added">{{ t('library.sortAdded') }}</option>
          <option value="name">{{ t('library.sortName') }}</option>
        </select>
      </div>
    </section>

    <!-- 导出/导入的结果就地显示：尤其是"缺连接所以没导入"这种，必须说明白。 -->
    <NoticePanel v-if="libraryNotice" class="mt-4" :variant="libraryNotice.variant">
      {{ libraryNotice.text }}
    </NoticePanel>

    <div
      v-if="availableGroups.length"
      class="app-group-filter mt-4"
      :aria-label="t('library.groupFilter')"
    >
      <button
        type="button"
        class="app-group-chip"
        :class="!selectedGroup && 'is-active'"
        @click="selectedGroup = ''"
      >
        {{ t('library.allGroups') }}
      </button>
      <button
        v-for="group in availableGroups"
        :key="group"
        type="button"
        class="app-group-chip"
        :class="selectedGroup === group && 'is-active'"
        @click="selectedGroup = group"
      >
        {{ group }}
      </button>
    </div>

    <!-- 断点用 `xl` 而不是 `lg`：右栏从 1200 起常驻、主区让出 320px，容器宽度在 1200 处
         跳变——视口断点写 `lg:grid-cols-3` 会让 1200 视口下的 640px 容器硬撑三列
         （每张 ~190px）。理由与 `FirstRunGuide` 那条注释相同。 -->
    <div v-if="filteredRecipes.length" class="mt-5 grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
      <Card
        v-for="recipe in filteredRecipes"
        :key="recipe.recipeId"
        class="app-card group relative p-4"
      >
        <RouterLink
          :to="{ name: 'V2Runner', params: { id: String(recipe.recipeId) } }"
          class="block"
        >
          <div class="app-summary flex items-start gap-4">
            <AppCover class="size-12 shrink-0" :alt="recipe.name" icon-class="size-5" />
            <div class="min-w-0 flex-1">
              <div class="flex items-start gap-3">
                <!--
                  名字**折两行**而不是一行截断：它往往是我们生成的「RunningHub 工作流 + 19 位 id」
                  （36 字符），一行在 272px 的卡片里根本放不下 —— 而**认出"这是我哪个东西"
                  只能靠它**（身份 > 状态 > 操作）。悬停还有系统 tooltip 兜底完整名字。
                -->
                <h2 class="line-clamp-2 font-semibold" :title="recipe.name">{{ recipe.name }}</h2>
              </div>
              <p class="mt-1 truncate text-xs text-muted-foreground">
                {{ connectionLabel(recipe) }}
              </p>
            </div>
          </div>
          <p class="mt-4 line-clamp-2 min-h-10 text-sm leading-5 text-muted-foreground">
            {{ stripHtml(recipe.note || recipe.description) || t('library.noNote') }}
          </p>
          <div class="mt-4 flex min-h-6 flex-wrap gap-1.5">
            <span class="tag tag-primary">{{ typeLabel(recipe) }}</span>
            <span v-for="label in visibleLabels(recipe)" :key="label" class="tag">{{ label }}</span>
            <span v-if="recipe.group" class="tag">{{ recipe.group }}</span>
          </div>
          <div class="mt-4 flex items-center justify-between border-t border-border pt-3 text-xs">
            <span class="text-muted-foreground">
              {{ recipe.lastOpenedAt ? t('library.recentlyUsed') : t('library.neverOpened') }}
            </span>
            <span class="app-open-label inline-flex items-center gap-1 font-medium text-primary"
              >{{ t('library.open') }} <ArrowRight class="size-3"
            /></span>
          </div>
        </RouterLink>
        <div class="card-actions">
          <IconButton
            size="sm"
            :label="
              recipe.favorite
                ? t('library.unfavorite', { name: recipe.name })
                : t('library.favorite', { name: recipe.name })
            "
            @click="toggleFavorite(recipe)"
            ><Star class="size-3.5" :fill="recipe.favorite ? 'currentColor' : 'none'"
          /></IconButton>
          <IconButton
            size="sm"
            :label="t('library.editNamed', { name: recipe.name })"
            @click="openEdit(recipe)"
            ><Pencil class="size-3.5"
          /></IconButton>
          <IconButton
            size="sm"
            class="text-destructive"
            :label="t('library.removeNamed', { name: recipe.name })"
            @click="removingRecipe = recipe"
            ><Trash2 class="size-3.5"
          /></IconButton>
        </div>
      </Card>
    </div>
    <EmptyState
      v-else
      class="mt-6"
      :title="workspace.recipes.length ? t('library.emptyFilteredTitle') : t('library.emptyTitle')"
      :description="
        workspace.recipes.length
          ? t('library.emptyFilteredDescription')
          : t('library.emptyDescription')
      "
    >
      <template #icon><Boxes class="size-5" /></template>
      <template #actions>
        <Button
          v-if="workspace.recipes.length && hasFilters"
          variant="outline"
          @click="clearFilters"
        >
          {{ t('library.clearFilters') }}
        </Button>
        <Button
          v-else-if="runningHubConnections.length"
          variant="outline"
          @click="showImport = true"
        >
          {{ t('library.addRhApp') }}
        </Button>
        <RouterLink
          v-else
          to="/connections"
          class="inline-flex h-9 items-center justify-center rounded-md px-3.5 text-sm font-medium transition-colors hover:bg-accent hover:text-accent-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
        >
          {{ t('library.configureRhFirst') }}
        </RouterLink>
      </template>
    </EmptyState>

    <Dialog :open="showImport" :title="t('library.dialogAddRh')" @update:open="showImport = $event">
      <!--
        顶部这句只消掉他唯一的疑虑（我这条链接能不能用），**不解释机制**：
        前几版写的是"由平台判定 / 字段与默认值自动导入 / 无需手写 Schema"——那是在替我们的
        设计辩护，不是在告诉他下一步做什么，而且还和输入框下面那行重复了。
        口径见 useLocale 里 `library.dialogAddRhHint` 上面那段注释。
      -->
      <div class="space-y-4">
        <p class="text-sm text-muted-foreground">{{ t('library.dialogAddRhHint') }}</p>
        <div class="space-y-1.5">
          <label for="runninghub-import-connection" class="text-sm font-medium"
            >RunningHub Connection</label
          >
          <select
            id="runninghub-import-connection"
            v-model="importForm.connectionId"
            class="h-9 w-full rounded-md border border-input bg-secondary px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <option value="" disabled>{{ t('library.selectPlaceholder') }}</option>
            <option
              v-for="connection in runningHubConnections"
              :key="connection.connectionId"
              :value="String(connection.connectionId)"
            >
              {{ connection.displayName }}
            </option>
          </select>
          <p v-if="!runningHubConnections.length" class="text-xs text-muted-foreground">
            {{ t('library.noRhConnectionHint') }}
          </p>
          <p v-if="hasIncompleteRunningHubConnections" class="text-xs text-warning">
            {{ t('library.incompleteConnectionsHidden') }}
          </p>
          <RouterLink
            v-if="!runningHubConnections.length"
            to="/connections"
            class="mt-1 inline-flex text-xs font-medium text-primary underline-offset-4 hover:underline"
          >
            {{ t('library.goConfigureConnection') }}
          </RouterLink>
        </div>
        <div class="space-y-1.5">
          <label for="runninghub-import-source" class="text-sm font-medium">{{
            t('library.appLinkOrId')
          }}</label>
          <Input
            id="runninghub-import-source"
            v-model="importForm.source"
            :placeholder="t('library.rhSourcePlaceholder')"
          />
          <!--
            **粘完才出现**：这行说的是"你手上这条是什么、要另一种去哪拿"。
            同一个作品页上"AI 应用"与"工作流"可能**同时存在**，而且是**两个不同的 id**
            （实测：`…/post/<postId>` 走应用接口回 `WEBAPP_NOT_EXISTS`，而页面上明明有
            「打开AI应用」按钮 ⇒ 那个应用挂在另一个 id 上，这条链接里没有它）。
            我们**不替用户猜**那个 id —— 也不去读平台的页面（那是别人的实现，不是契约）；
            只如实告诉他这条链接给到的是什么、要另一种去哪儿拿。
          -->
          <p v-if="importForm.source.trim()" class="text-xs text-muted-foreground">
            {{ rhKindReadHint }}
          </p>
        </div>
        <div class="space-y-1.5">
          <label for="runninghub-import-kind" class="text-sm font-medium">{{
            t('library.rhKindLabel')
          }}</label>
          <select
            id="runninghub-import-kind"
            v-model="importForm.kind"
            class="h-9 w-full rounded-md border border-input bg-secondary px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <option value="">{{ t('library.rhKindAuto') }}</option>
            <option value="app">{{ t('library.rhKindApp') }}</option>
            <option value="workflow">{{ t('library.rhKindWorkflow') }}</option>
          </select>
          <p v-if="rhKindMismatch" class="text-xs text-warning">
            {{ t('library.rhKindMismatch') }}
          </p>
        </div>
        <!--
          名字只对**工作流**那条路有用（AI 应用的默认名来自平台，比他自己起的更准）——
          所以判定为应用时这个框**不显示**：不给一个填了会被丢掉的输入框。
        -->
        <div v-if="effectiveRhKind !== 'app'" class="space-y-1.5">
          <label for="runninghub-import-name" class="text-sm font-medium">{{
            t('library.appNameOptional')
          }}</label>
          <Input
            id="runninghub-import-name"
            v-model="importForm.name"
            :placeholder="t('library.rhNamePlaceholder')"
          />
        </div>
        <p v-else class="text-xs text-muted-foreground">{{ t('library.rhNamePlatformOwned') }}</p>
        <p v-if="importError" class="text-sm text-destructive">{{ importError }}</p>
      </div>
      <template #footer>
        <Button variant="ghost" @click="showImport = false">{{ t('common.cancel') }}</Button>
        <Button
          :disabled="importing || !runningHubConnections.length || !importForm.connectionId"
          @click="importFromRunningHub"
          >{{ importing ? t('library.importing') : t('library.import') }}</Button
        >
      </template>
    </Dialog>

    <Dialog
      :open="showComfyUIImport"
      :class="comfyStep === 'select' ? 'max-w-2xl' : ''"
      :title="comfyStep === 'paste' ? t('library.dialogAddComfy') : t('library.dialogSelectFields')"
      @update:open="showComfyUIImport = $event"
    >
      <div v-if="comfyStep === 'paste'" class="space-y-4">
        <p class="text-sm text-muted-foreground">
          {{ t('library.dialogAddComfyDescription') }}
        </p>
        <div class="space-y-1.5">
          <label for="comfyui-import-connection" class="text-sm font-medium"
            >ComfyUI Connection</label
          >
          <select
            id="comfyui-import-connection"
            v-model="comfyImportForm.connectionId"
            class="h-9 w-full rounded-md border border-input bg-secondary px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <option value="" disabled>{{ t('library.selectPlaceholder') }}</option>
            <option
              v-for="connection in comfyUiConnections"
              :key="connection.connectionId"
              :value="String(connection.connectionId)"
            >
              {{ connection.displayName }}
            </option>
          </select>
          <p v-if="!comfyUiConnections.length" class="text-xs text-muted-foreground">
            {{ t('library.noComfyConnectionHint') }}
          </p>
          <RouterLink
            v-if="!comfyUiConnections.length"
            to="/connections"
            class="mt-1 inline-flex text-xs font-medium text-primary underline-offset-4 hover:underline"
          >
            {{ t('library.goConfigureConnection') }}
          </RouterLink>
        </div>
        <div class="space-y-1.5">
          <label for="comfyui-import-name" class="text-sm font-medium">{{
            t('library.appNameOptional')
          }}</label>
          <Input
            id="comfyui-import-name"
            v-model="comfyName"
            :placeholder="t('library.appNamePlaceholder')"
          />
        </div>
        <div class="space-y-1.5">
          <label for="comfyui-import-source" class="text-sm font-medium">{{
            t('library.fetchFromRhOptional')
          }}</label>
          <div class="flex gap-2">
            <Input
              id="comfyui-import-source"
              v-model="comfyWorkflowSource"
              :placeholder="t('library.workflowSourcePlaceholder')"
              class="flex-1"
            />
            <Button
              variant="secondary"
              :disabled="comfyFetching || comfyImporting || !comfyWorkflowSource.trim()"
              @click="fetchRhWorkflow"
            >
              {{ comfyFetching ? t('library.fetching') : t('library.fetch') }}
            </Button>
          </div>
          <p class="text-xs text-muted-foreground">
            {{ t('library.fetchHint') }}
          </p>
        </div>
        <div class="space-y-1.5">
          <div class="flex items-center justify-between gap-3">
            <label for="comfyui-import-prompt" class="text-sm font-medium"
              >API Prompt（JSON）</label
            >
            <!--
              文件入口：源物件本身就是个文件（ComfyUI「保存(API 格式)」导出的 .json），
              所以给"读文件"这条路，别逼用户去记事本里复制。
            -->
            <div class="flex items-center gap-2">
              <span
                v-if="comfyPromptFileName"
                class="max-w-52 truncate text-xs text-muted-foreground"
              >
                {{ comfyPromptFileName }}
              </span>
              <Button
                variant="outline"
                size="sm"
                type="button"
                class="h-7 gap-1 px-2 text-xs"
                @click="comfyPromptFileInput?.click()"
              >
                <FolderOpen class="size-3.5" />{{ t('library.chooseJsonFile') }}
              </Button>
            </div>
          </div>
          <input
            ref="comfyPromptFileInput"
            type="file"
            accept=".json,application/json"
            class="hidden"
            @change="onComfyPromptFileChange"
          />
          <textarea
            id="comfyui-import-prompt"
            v-model="comfyImportForm.promptJson"
            rows="10"
            spellcheck="false"
            :placeholder="promptPlaceholder"
            class="w-full rounded-md border border-input bg-secondary p-2.5 font-mono text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            :class="comfyPromptDragging ? 'border-ring ring-2 ring-ring' : ''"
            @dragover.prevent="comfyPromptDragging = true"
            @dragleave.prevent="comfyPromptDragging = false"
            @drop.prevent="onComfyPromptDrop"
          ></textarea>
          <p class="text-xs text-muted-foreground">{{ t('library.promptInputHint') }}</p>
        </div>
        <p v-if="comfyImportError" class="text-sm text-destructive">{{ comfyImportError }}</p>
      </div>

      <div v-else class="space-y-3">
        <p class="text-sm text-muted-foreground">
          {{ t('library.comfyFieldsFound', { count: comfyFieldRows.length }) }}
        </p>
        <div class="max-h-80 space-y-2 overflow-y-auto pr-1">
          <div
            v-for="row in comfyFieldRows"
            :key="row.key"
            class="flex items-center gap-3 rounded-md border border-border px-3 py-2"
            :class="row.selected ? '' : 'opacity-60'"
          >
            <input
              :id="`comfy-field-${row.key}`"
              v-model="row.selected"
              type="checkbox"
              class="size-4 shrink-0 rounded border-input accent-primary"
            />
            <div class="min-w-0 flex-1">
              <label :for="`comfy-field-${row.key}`" class="block truncate text-sm font-medium">
                {{ row.fieldName }}
                <span class="ml-1 font-mono text-xs font-normal text-muted-foreground">
                  {{ row.classType }} · {{ t('library.node', { id: row.nodeId }) }}
                </span>
              </label>
              <p
                v-if="row.default !== undefined && row.default !== null"
                class="truncate text-xs text-muted-foreground"
              >
                {{ t('library.currentValue', { value: String(row.default) }) }}
              </p>
            </div>
            <Badge v-if="row.media" variant="secondary" class="shrink-0">{{
              t('library.file')
            }}</Badge>
            <!--
              **控件**：默认（推荐）= 不覆盖，让引擎声明/值的形状决定；用户一改就是他的决定。
              这件事只有用户知道：`aspect_ratio` 其实只有三种画幅、他常用的采样器就那五个。
              选了「下拉」才要他填候选项（没填就退回文本，不造空下拉）。
            -->
            <select
              v-model="row.widget"
              :disabled="!row.selected"
              :aria-label="t('library.widgetFor', { name: row.fieldName })"
              class="h-8 w-32 shrink-0 rounded-md border border-input bg-secondary px-2 text-xs"
            >
              <option value="">{{ t('library.widgetDefault') }}</option>
              <option value="text">{{ t('library.widgetText') }}</option>
              <option value="textarea">{{ t('library.widgetTextarea') }}</option>
              <option value="number">{{ t('library.widgetNumber') }}</option>
              <option value="switch">{{ t('library.widgetSwitch') }}</option>
              <option value="dropdown">{{ t('library.widgetDropdown') }}</option>
              <option value="file">{{ t('library.widgetFile') }}</option>
            </select>
            <Input
              v-if="row.widget === 'dropdown'"
              v-model="row.options"
              :disabled="!row.selected"
              :placeholder="t('library.widgetOptionsPlaceholder')"
              class="h-8 w-56 shrink-0 text-xs"
            />
            <Input
              v-model="row.label"
              :disabled="!row.selected"
              :placeholder="
                row.defaultLabel || t('library.fieldLabelPlaceholder', { name: row.fieldName })
              "
              class="h-8 w-44 shrink-0 text-xs"
            />
          </div>
        </div>
        <!--
          "控件是真控件还是通用控件"要说出来：拿不到引擎定义时我们会静默降级
          （枚举变文本框、数字没有范围），不说的话用户只会觉得"这表单有点糙"。
        -->
        <p
          v-if="comfyEnrichment && !comfyEnrichment.classesKnown"
          class="text-xs leading-5 text-warning"
        >
          {{
            t('library.enrichmentMissing', {
              reason: comfyEnrichment.reason || t('library.enrichmentUnknown')
            })
          }}
        </p>
        <p v-else-if="comfyEnrichment" class="text-xs leading-5 text-muted-foreground">
          {{
            t('library.enrichmentOk', {
              known: comfyEnrichment.classesKnown,
              used: comfyEnrichment.classesUsed
            })
          }}
        </p>
        <p v-if="comfyImportError" class="text-sm text-destructive">{{ comfyImportError }}</p>
      </div>

      <template #footer>
        <template v-if="comfyStep === 'paste'">
          <Button variant="ghost" @click="showComfyUIImport = false">{{
            t('common.cancel')
          }}</Button>
          <Button
            :disabled="comfyImporting || !comfyUiConnections.length"
            @click="previewComfyImport"
            >{{ comfyImporting ? t('library.parsing') : t('library.nextSelectFields') }}</Button
          >
        </template>
        <template v-else>
          <Button variant="ghost" :disabled="comfyImporting" @click="comfyStep = 'paste'">
            {{ t('library.previousStep') }}
          </Button>
          <Button :disabled="comfyImporting || !comfySelectedCount" @click="importFromComfyUI">{{
            comfyImporting
              ? t('library.importing')
              : t('library.importSelected', { count: comfySelectedCount })
          }}</Button>
        </template>
      </template>
    </Dialog>

    <Dialog :open="showCreate" :title="t('library.createTitle')" @update:open="showCreate = $event">
      <div class="space-y-4">
        <div class="space-y-1.5">
          <label for="recipe-name" class="text-sm font-medium">{{ t('library.formName') }}</label>
          <Input
            id="recipe-name"
            v-model="form.name"
            :placeholder="t('library.formNamePlaceholder')"
          />
        </div>
        <div class="space-y-1.5">
          <label for="recipe-capability" class="text-sm font-medium">{{
            t('library.formCapability')
          }}</label>
          <Input id="recipe-capability" v-model="form.capability" placeholder="text-to-image" />
        </div>
        <div class="space-y-1.5">
          <label for="recipe-driver-key" class="text-sm font-medium">{{
            t('library.formDriver')
          }}</label>
          <select
            id="recipe-driver-key"
            v-model="form.driverKey"
            class="h-9 w-full rounded-md border border-input bg-secondary px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <option v-for="option in DRIVER_KEYS" :key="option.value" :value="option.value">
              {{ option.label }}
            </option>
          </select>
        </div>
        <div class="space-y-1.5">
          <label for="recipe-connection" class="text-sm font-medium">{{
            t('library.formDefaultConnection')
          }}</label>
          <select
            id="recipe-connection"
            v-model="form.defaultConnectionId"
            class="h-9 w-full rounded-md border border-input bg-secondary px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <option value="" disabled>{{ t('library.formConnectionPlaceholder') }}</option>
            <option
              v-for="connection in compatibleConnections"
              :key="connection.connectionId"
              :value="String(connection.connectionId)"
            >
              {{
                t('library.connectionDetail', {
                  name: connection.displayName,
                  vendor: connection.vendor,
                  protocol: connection.protocol
                })
              }}
            </option>
          </select>
          <p v-if="!compatibleConnections.length" class="text-xs text-muted-foreground">
            {{ t('library.noCompatibleConnections') }}
          </p>
        </div>
        <div class="space-y-1.5">
          <label for="recipe-input-schema" class="text-sm font-medium">inputSchema（JSON）</label>
          <textarea
            id="recipe-input-schema"
            v-model="form.inputSchemaJson"
            rows="8"
            spellcheck="false"
            class="w-full rounded-md border border-input bg-secondary p-2.5 font-mono text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          ></textarea>
        </div>
        <div class="space-y-1.5">
          <label for="recipe-description" class="text-sm font-medium">{{
            t('library.formDescription')
          }}</label>
          <Input
            id="recipe-description"
            v-model="form.description"
            :placeholder="t('library.optional')"
          />
        </div>
        <p v-if="createError" class="text-sm text-destructive">{{ createError }}</p>
      </div>
      <template #footer>
        <Button variant="ghost" @click="showCreate = false">{{ t('common.cancel') }}</Button>
        <Button :disabled="creating" @click="createRecipe">{{
          creating ? t('common.creating') : t('common.create')
        }}</Button>
      </template>
    </Dialog>

    <Dialog
      :open="Boolean(editingRecipe)"
      :title="t('library.editTitle', { name: editingRecipe?.name ?? '' })"
      @update:open="!$event && closeEdit()"
    >
      <p class="text-sm text-muted-foreground">{{ t('library.editHint') }}</p>
      <!-- 刚导完自动打开的这一次：把"为什么要现在起名字"说清楚，别让人以为我们在拦他。 -->
      <p
        v-if="editAfterImport"
        class="mt-2 rounded-md bg-secondary/60 px-3 py-2 text-xs leading-5 text-muted-foreground"
      >
        {{ t('library.editHintAfterImport') }}
      </p>
      <div class="mt-5 space-y-4">
        <FormField :label="t('library.formNote')">
          <Textarea
            v-model="editForm.note"
            :rows="3"
            class="resize-y"
            :placeholder="t('library.formNotePlaceholder')"
          />
        </FormField>
        <FormField :label="t('library.formGroup')">
          <Input v-model="editForm.group" :placeholder="t('library.formGroupPlaceholder')" />
        </FormField>

        <!--
          字段备注：把 `874::text` 变成「提示词」。
          平台给的 description 就等于字段名，读不出哪个字段才是提示词——
          这里补的备注会同时出现在**运行表单**和 **MCP 的工具描述**里（同一个合并函数）。
        -->
        <FormField :label="t('library.formFieldLabels')">
          <p class="mb-2 text-xs text-muted-foreground">
            {{ t('library.formFieldLabelsHint') }}
          </p>
          <p v-if="editLoadingFields" class="text-xs text-muted-foreground">
            {{ t('common.loading') }}
          </p>
          <p v-else-if="!editForm.fields.length" class="text-xs text-muted-foreground">
            {{ t('library.formFieldLabelsEmpty') }}
          </p>
          <div v-else class="space-y-2">
            <!--
              **控件**：这一列是"用完之后才发现要改"的那个出口 —— 你只有在用过之后才知道
              这张图的 `cfg` 从来不动、但画幅每次都要换。默认显示它**现在**是什么
              （我们建议的 / 引擎给的 / 你选过的），改了就是你的决定（会写进一份新版本）。
            -->
            <div v-for="field in editForm.fields" :key="field.key" class="flex items-center gap-2">
              <code class="w-40 shrink-0 truncate text-xs text-muted-foreground" :title="field.key">
                {{ field.key }}
              </code>
              <Input v-model="field.label" :placeholder="t('library.formFieldLabelPlaceholder')" />
              <select
                v-model="field.widget"
                :aria-label="t('library.widgetFor', { name: field.key })"
                class="h-9 w-36 shrink-0 rounded-md border border-input bg-secondary px-2 text-xs"
                @change="onEditWidgetChange(field)"
              >
                <option value="">{{ t('library.widgetKeep') }}</option>
                <option value="text">{{ t('library.widgetText') }}</option>
                <option value="textarea">{{ t('library.widgetTextarea') }}</option>
                <option value="number">{{ t('library.widgetNumber') }}</option>
                <option value="switch">{{ t('library.widgetSwitch') }}</option>
                <option value="dropdown">{{ t('library.widgetDropdown') }}</option>
                <option value="file">{{ t('library.widgetFile') }}</option>
              </select>
              <Input
                v-if="field.widget === 'dropdown'"
                v-model="field.options"
                :placeholder="t('library.widgetOptionsPlaceholder')"
                class="w-56 shrink-0"
              />
              <Badge v-if="field.source === 'user'" variant="secondary" class="shrink-0">{{
                t('library.widgetYours')
              }}</Badge>
              <span
                v-else-if="field.source === 'engine'"
                class="shrink-0 text-xs text-muted-foreground"
              >
                {{ t('library.widgetFromEngine') }}
              </span>
            </div>
          </div>
        </FormField>
      </div>
      <template #footer>
        <Button variant="outline" @click="closeEdit">{{ t('common.cancel') }}</Button>
        <Button @click="saveEdit">{{ t('library.save') }}</Button>
      </template>
    </Dialog>

    <ConfirmDialog
      :open="Boolean(removingRecipe)"
      :title="t('library.removeTitle')"
      :description="t('library.removeDescription', { name: removingRecipe?.name ?? '' })"
      :confirm-label="t('library.remove')"
      destructive
      @update:open="!$event && (removingRecipe = undefined)"
      @confirm="removeRecipe"
    />

    <!--
      已移除（墓碑）。刻意**不做成链接**：后端 `run_app` 会拒绝已移除的应用，
      点进去只会撞一堵墙；这里唯一的动作就是把它拿回来。
    -->
    <section v-if="showRemoved" class="mt-10 border-t border-border pt-6">
      <h2 class="text-sm font-semibold">
        {{ t('library.removedSection', { count: workspace.removedRecipes.length }) }}
      </h2>
      <p v-if="workspace.removedLoading" class="mt-3 text-sm text-muted-foreground">
        {{ t('library.removedLoading') }}
      </p>
      <p v-else-if="!workspace.removedRecipes.length" class="mt-3 text-sm text-muted-foreground">
        {{ t('library.removedEmpty') }}
      </p>
      <div v-else class="mt-4 grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
        <Card
          v-for="recipe in workspace.removedRecipes"
          :key="recipe.recipeId"
          class="app-card relative p-4 opacity-80"
        >
          <div class="app-summary flex items-start gap-4">
            <AppCover class="size-12 shrink-0" :alt="recipe.name" icon-class="size-5" />
            <div class="min-w-0 flex-1">
              <h2 class="truncate font-semibold">{{ recipe.name }}</h2>
              <p class="mt-1 truncate text-xs text-muted-foreground">
                {{ connectionLabel(recipe) }}
              </p>
            </div>
          </div>
          <p class="mt-4 line-clamp-2 min-h-10 text-sm leading-5 text-muted-foreground">
            {{ stripHtml(recipe.note || recipe.description) || t('library.noNote') }}
          </p>
          <div class="mt-4 flex min-h-6 flex-wrap gap-1.5">
            <span class="tag tag-primary">{{ typeLabel(recipe) }}</span>
            <span class="tag">{{ t('library.removedBadge') }}</span>
          </div>
          <div
            class="mt-4 flex items-center justify-between gap-2 border-t border-border pt-3 text-xs"
          >
            <span class="text-muted-foreground">{{
              t('library.removedAt', { time: removedAtLabel(recipe.removedAt) })
            }}</span>
            <Button
              size="sm"
              variant="outline"
              :disabled="restoringId === recipe.recipeId"
              :aria-label="t('library.restoreNamed', { name: recipe.name })"
              @click="restoreRecipe(recipe)"
            >
              <ArchiveRestore class="size-3.5" />
              {{ t('library.restore') }}
            </Button>
          </div>
        </Card>
      </div>
    </section>
  </PageContainer>
</template>

<style scoped>
.app-group-filter {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
}
.app-group-chip {
  border: 1px solid var(--app-border);
  background: var(--app-card);
  padding: 0.375rem 0.625rem;
  font-size: 0.75rem;
  color: var(--app-muted-foreground);
  transition:
    border-color 150ms ease,
    background-color 150ms ease,
    color 150ms ease;
}
.app-group-chip:hover {
  border-color: color-mix(in srgb, var(--app-info) 46%, var(--app-border));
  color: var(--app-foreground);
}
.app-group-chip.is-active {
  border-color: var(--app-info);
  background: color-mix(in srgb, var(--app-info) 11%, var(--app-card));
  color: var(--app-info);
}
/*
 * 卡片右侧留一条**窄**竖带给操作按钮（只为它们预留 ~36px）。
 *
 * ⚠️ 这里原来给横排的三个按钮留了 `6.5rem`（104px），而卡片在「右栏打开 + xl 三列」时
 * 只有 272px 宽 —— 减掉图标 48 与间隔 16，**名字只剩 71px**，界面上显示成 "Runnin…"，
 * 用户认不出这是自己的哪个东西（实测 2026-10-14）。
 *
 * 第一性原理：卡片的职责排序是 **身份（名字 + 类型）> 状态 > 操作**，
 * 而操作**不该占用身份的空间** —— 更别说这三个按钮还是**悬停才出现**的。
 * 所以是"让操作让位"，不是"给名字打补丁"（加 tooltip 治标：不悬停照样看不出）。
 */
.app-card {
  /* 40px = 按钮带 32px + 它离右边 4px + 4px 间隙：**刚好不压到内容**（实测 36px 会探进名字 4px）。 */
  padding-right: 2.5rem;
  transition:
    border-color 150ms ease,
    background-color 150ms ease,
    transform 150ms ease;
}
.app-card:hover {
  border-color: color-mix(in srgb, var(--app-info) 55%, var(--app-border));
  background: var(--app-card);
  box-shadow: var(--shadow-sm);
  transform: translateY(-1px);
}
.tag {
  border: 1px solid var(--app-border);
  padding: 0.2rem 0.45rem;
  font-size: 0.6875rem;
  color: var(--app-muted-foreground);
}
.tag-primary {
  border-color: var(--app-primary);
  color: var(--app-primary);
}
/* 操作按钮**竖排**贴在右上角那条窄带里（见上面 `.app-card` 的说明：横排要 104px，竖排只要 36px）。 */
.card-actions {
  position: absolute;
  right: 0.25rem;
  top: 0.7rem;
  display: flex;
  flex-direction: column;
  gap: 0.125rem;
  opacity: 0;
  transition: opacity 150ms ease;
}
.app-card:hover .card-actions,
.app-card:focus-within .card-actions {
  opacity: 1;
}
</style>
