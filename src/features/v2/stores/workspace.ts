import { ref } from 'vue'
import { defineStore } from 'pinia'
import {
  v2,
  type OperationDetail,
  type OperationSummary,
  type RecipeSummary,
  isTerminalStatus
} from '@/shared/api/v2'

export interface V2Connection {
  connectionId: number
  vendor: string
  protocol: string
  displayName: string
  currentRevisionId: number
  status: string
  detailsAvailable?: boolean
  healthOk?: boolean
  baseUrl?: string
  apiKeyConfigured?: boolean
}

export interface V2Recipe extends RecipeSummary {
  inputSchema?: unknown
  outputSchema?: unknown
}

export interface V2ComfyFieldPreview {
  key: string
  nodeId: string
  classType: string
  fieldName: string
  fieldType: string
  default?: unknown
  media: boolean
  suggested: boolean
  /** 不写备注名时它会叫什么（`节点标题 · 字段名` 或 `类名 · 字段名`）——当占位提示用。 */
  defaultLabel: string
}

/**
 * 导入接口的返回（AI 应用与工作流同形）。
 *
 * `fields` 里的 `description` 是**平台给的字段描述**——注意它常常就是字段名本身
 * （实测 `{name: "text", description: "text"}`），那等于**没给名字**：
 * 判据交给界面（见 `V2Recipes.vue` 里"导完要不要请你命名"那段），所以这里原样带着。
 */
export interface ImportedRecipeResult {
  recipeId: number
  name: string
  revisionId?: number
  fields?: Array<{ name?: string; description?: string; descriptionEn?: string }>
}

export const useV2WorkspaceStore = defineStore('v2-workspace', () => {
  const recipes = ref<V2Recipe[]>([])
  /**
   * 已移除（墓碑）的应用 —— **另一个集合**，不是"活着的里面筛一部分"。
   *
   * 按需加载（用户打开"显示已移除"时才拉），因为绝大多数会话都不需要它。
   */
  const removedRecipes = ref<V2Recipe[]>([])
  const removedLoaded = ref(false)
  const removedLoading = ref(false)
  const connections = ref<V2Connection[]>([])
  const connectionsLoaded = ref(false)
  const connectionsLoading = ref(false)
  const operations = ref<OperationDetail[]>([])
  const operationSummaries = ref<OperationSummary[]>([])
  const isLoaded = ref(false)
  const isLoading = ref(false)
  const error = ref('')
  const operationsError = ref('')
  const activeOperationId = ref<number | null>(null)

  let loadPromise: Promise<void> | undefined
  let operationSummariesLoadPromise: Promise<boolean> | undefined
  let operationsLoadPromise: Promise<boolean> | undefined
  const operationDetailPromises = new Map<number, Promise<OperationDetail>>()
  let operationsRequestVersion = 0

  const loadRecipes = async (force = false) => {
    if (isLoaded.value && !force) return
    if (loadPromise) return loadPromise
    isLoading.value = true
    loadPromise = (async () => {
      try {
        const page = await v2.listRecipes(200)
        recipes.value = page.items
        isLoaded.value = true
        error.value = ''
      } catch (caught) {
        error.value = caught instanceof Error ? caught.message : String(caught)
        throw caught
      } finally {
        isLoading.value = false
        loadPromise = undefined
      }
    })()
    return loadPromise
  }

  /**
   * 加载"已移除"（墓碑）那一批。按需调用：用户打开"显示已移除"时才拉。
   *
   * 为什么单独一个集合而不是给 `recipes` 加个过滤标记：移除与否是**两个集合**
   * （服务端也是两个查询），混在一起会让每一处用到 `recipes` 的地方都得再判一次。
   */
  const loadRemovedRecipes = async (force = false) => {
    if (removedLoaded.value && !force) return
    removedLoading.value = true
    try {
      const page = await v2.listRemovedRecipes(200)
      removedRecipes.value = page.items
      removedLoaded.value = true
    } catch (caught) {
      error.value = caught instanceof Error ? caught.message : String(caught)
      throw caught
    } finally {
      removedLoading.value = false
    }
  }

  const loadConnections = async () => {
    connectionsLoading.value = true
    connectionsLoaded.value = false
    connections.value = []
    try {
      const items = await v2.listConnections()
      connections.value = await Promise.all(
        items.map(async (item) => {
          try {
            const detail = await v2.getConnection(item.connectionId)
            let healthOk: boolean | undefined
            try {
              healthOk = (await v2.checkConnection(item.connectionId)).ok
            } catch {
              healthOk = false
            }
            return {
              ...item,
              baseUrl: typeof detail.config.baseUrl === 'string' ? detail.config.baseUrl : '',
              detailsAvailable: true,
              healthOk,
              apiKeyConfigured: detail.secretBindings.some(
                (binding) => binding.slot === 'api_key' || binding.slot.endsWith('-api_key')
              )
            }
          } catch {
            return {
              ...item,
              baseUrl: '',
              detailsAvailable: false,
              healthOk: false,
              apiKeyConfigured: false
            }
          }
        })
      )
      connectionsLoaded.value = true
    } catch (caught) {
      error.value = caught instanceof Error ? caught.message : String(caught)
    } finally {
      connectionsLoading.value = false
    }
  }

  const loadOperationSummaries = async (limit = 100): Promise<boolean> => {
    if (operationSummariesLoadPromise) return operationSummariesLoadPromise
    operationSummariesLoadPromise = (async () => {
      try {
        const page = await v2.listOperations(limit)
        operationSummaries.value = page.items
        operationsError.value = ''
        return true
      } catch (caught) {
        operationsError.value = caught instanceof Error ? caught.message : String(caught)
        return false
      } finally {
        operationSummariesLoadPromise = undefined
      }
    })()
    return operationSummariesLoadPromise
  }

  const loadOperations = async (limit = 100): Promise<boolean> => {
    if (operationsLoadPromise) return operationsLoadPromise
    const requestVersion = ++operationsRequestVersion
    operationsLoadPromise = (async () => {
      const succeeded = await loadOperationSummaries(limit)
      if (requestVersion !== operationsRequestVersion) return false
      if (succeeded) error.value = ''
      operationsLoadPromise = undefined
      return succeeded
    })()
    return operationsLoadPromise
  }

  const createRecipe = async (input: {
    name: string
    capability: string
    driverKey: string
    defaultConnectionId: number
    inputSchema: Record<string, unknown>
    providerConfig?: Record<string, unknown>
    description?: string
  }) => {
    const result = await v2.post<{ recipeId: number; currentRevisionId: number }>(
      '/api/v1/recipes',
      input
    )
    await loadRecipes(true)
    return result
  }

  /**
   * 导出应用库：只含应用本身（名字、分组、备注名、控件、平台类型），
   * 不含任务历史、产物、连接与密钥。不传路径 ⇒ daemon 落到下载目录。
   */
  const exportLibrary = async () => {
    return v2.post<{ path: string; recipeCount: number; bytes: number }>(
      '/api/v1/library/export',
      {}
    )
  }

  /**
   * 导入应用库。
   *
   * 传的是**文件文本**而不是路径：浏览器的文件选择器给不到绝对路径（安全限制）。
   * 缺连接的应用不会进库，服务端把它们放进 `failed` 并带原因。
   */
  const importLibrary = async (content: string, replace = false) => {
    const result = await v2.post<{
      imported: string[]
      skipped: { name: string; reason: string; code: string }[]
      failed: { name: string; reason: string; code: string }[]
    }>('/api/v1/library/import', { content, replace })
    await loadRecipes(true)
    return result
  }

  const importRecipe = async (connectionId: number, source: string) => {
    const result = await v2.post<ImportedRecipeResult>('/api/v1/recipes/import', {
      connectionId,
      source
    })
    await loadRecipes(true)
    return result
  }

  /**
   * 从平台链接/工作流 ID 导入一个**原生 RunningHub 工作流**。
   *
   * 与 `importRecipe`（AI 应用）同一条通道、同一种配方：定义留在平台上，我们只存
   * `workflowId` + 字段映射。与 `importComfyUIRecipe` 的区别是**谁来跑**——那条把图拉进
   * 库里、交给 ComfyUI 引擎（rh-proxy）执行。
   */
  const importWorkflowRecipe = async (connectionId: number, source: string, name?: string) => {
    const result = await v2.post<ImportedRecipeResult>('/api/v1/recipes/import-workflow', {
      connectionId,
      source,
      ...(name ? { name } : {})
    })
    await loadRecipes(true)
    return result
  }

  /**
   * 导入一份 ComfyUI API Prompt。
   *
   * `fields` 里每条除了备注名，还可以带**用户选的控件**（`widget`）与候选项（`options`）：
   * "这个字段该是什么控件、有哪些合法选项"有一部分只有用户知道（他的图他自己清楚）。
   * 不传就是"不覆盖"——由引擎声明、值的形状、我们的建议按顺序决定（后端一处说了算）。
   */
  const importComfyUIRecipe = async (
    connectionId: number,
    prompt: Record<string, unknown>,
    options?: {
      name?: string
      fields?: Array<{ key: string; label?: string; widget?: string; options?: string[] }>
    }
  ) => {
    const result = await v2.post<{ recipeId: number; name: string; fields: unknown[] }>(
      '/api/v1/recipes/comfyui-import',
      {
        connectionId,
        prompt,
        ...(options?.name ? { name: options.name } : {}),
        ...(options?.fields ? { fields: options.fields } : {})
      }
    )
    await loadRecipes(true)
    return result
  }

  /**
   * 预览一份 API Prompt。带上 `connectionId` 时后端会**尽力**问一次 ComfyUI 的节点定义，
   * 好把通用控件升级成真控件（枚举→下拉、数字→带范围、多行→多行框）。
   *
   * 返回值里带着 `enrichment`：拿不到定义时它会说明**为什么**——界面要把这个原因显示出来，
   * 否则"控件是通用的"就成了一个看不见的降级。
   */
  const previewComfyUIFields = async (
    prompt: Record<string, unknown>,
    connectionId?: number
  ): Promise<{
    fields: V2ComfyFieldPreview[]
    enrichment: { classesUsed: number; classesKnown: number; reason: string | null }
  }> => {
    const result = await v2.post<{
      fields: V2ComfyFieldPreview[]
      enrichment?: { classesUsed: number; classesKnown: number; reason: string | null }
    }>('/api/v1/recipes/comfyui-preview', {
      prompt,
      ...(connectionId ? { connectionId } : {})
    })
    return {
      fields: result.fields,
      enrichment: result.enrichment ?? { classesUsed: 0, classesKnown: 0, reason: null }
    }
  }

  const fetchComfyUIWorkflow = async (connectionId: number, source: string) => {
    const result = await v2.post<{ prompt: Record<string, unknown> }>(
      '/api/v1/recipes/comfyui-fetch-workflow',
      { connectionId, source }
    )
    return result.prompt
  }

  const applyRecipeSummary = (summary: RecipeSummary) => {
    const index = recipes.value.findIndex((recipe) => recipe.recipeId === summary.recipeId)
    if (index === -1) recipes.value.push({ ...(summary as V2Recipe) })
    else recipes.value[index] = { ...recipes.value[index], ...summary }
  }

  const updateRecipe = async (
    recipeId: number,
    patch: {
      name?: string
      description?: string
      labels?: string[]
      favorite?: boolean
      group?: string
      note?: string
      /**
       * 字段备注（schema key → 人话名字）。**整体替换**语义：传 `{}` 即清空全部备注。
       *
       * 为什么是替换而不是合并：合并语义下"省略某个键"既可能是"不改"也可能是"删"，
       * 分不清——那样用户就删不掉一条写错的备注。
       */
      fieldLabels?: Record<string, string>
      /**
       * 用户在编辑里改的**字段控件**。
       *
       * 与备注不同：备注是展示语义（读 schema 时合并），控件会改变执行语义
       * （换成"图片或文件"要走资产上传），所以后端会为它**追加一份新 revision**。
       * 只传真改过的那些；不传 = 不改。
       */
      fieldWidgets?: Array<{ key: string; widget: string; options?: string[] }>
    }
  ) => {
    const summary = await v2.put<RecipeSummary>(`/api/v1/recipes/${recipeId}`, patch)
    applyRecipeSummary(summary)
    return summary
  }

  /** 取某个应用的 inputSchema（**已合并用户备注**）。界面表单与 Agent 看的是同一份。 */
  const fetchRecipeSchema = async (recipeId: number) => {
    const result = await v2.getRecipeSchema(recipeId)
    return result.inputSchema as {
      properties?: Record<string, { description?: string; 'x-widget-source'?: string }>
    }
  }

  const toggleFavorite = async (recipeId: number) => {
    const recipe = recipes.value.find((item) => item.recipeId === recipeId)
    if (!recipe) return
    const next = !(recipe.favorite ?? false)
    // 乐观更新，失败回滚。
    recipe.favorite = next
    try {
      const summary = await v2.put<RecipeSummary>(`/api/v1/recipes/${recipeId}`, { favorite: next })
      applyRecipeSummary(summary)
    } catch (caught) {
      recipe.favorite = !next
      throw caught
    }
  }

  const touchRecipe = async (recipeId: number) => {
    await v2.post<{ touched: boolean }>(`/api/v1/recipes/${recipeId}/touch`)
    const recipe = recipes.value.find((item) => item.recipeId === recipeId)
    if (recipe) recipe.lastOpenedAt = new Date().toISOString()
  }

  /**
   * 从资源库**移除**一个应用。
   *
   * ⚠️ 叫 remove 而不是 delete：这不是销毁。服务端打的是墓碑（`removed_at`），
   * 历史任务照旧可解析，而且随时能恢复（`restoreRecipe`）。界面上的文案也必须是"移除"
   * ——"删除"会让人以为记录没了，于是不敢用这个功能（这正是上一轮撞墙的原因之一）。
   * HTTP 动词仍是 DELETE（`/api/v1/recipes/{id}`，与既有客户端保持兼容）。
   */
  const removeRecipe = async (recipeId: number) => {
    await v2.delete<{ deleted: boolean }>(`/api/v1/recipes/${recipeId}`)
    recipes.value = recipes.value.filter((recipe) => recipe.recipeId !== recipeId)
    // 回收站已经加载过就同步刷新：用户移除一个应用后，切到"显示已移除"应当能看到它
    // ——否则那一下看起来就像"移除把它弄丢了"。
    if (removedLoaded.value) await loadRemovedRecipes(true)
  }

  /** 从"已移除"里恢复一个应用（墓碑清掉，它重新出现在资源库与 Agent 的可用应用目录里）。 */
  const restoreRecipe = async (recipeId: number) => {
    await v2.restoreRecipe(recipeId)
    removedRecipes.value = removedRecipes.value.filter((recipe) => recipe.recipeId !== recipeId)
    // 重新拉一次活着的列表（而不是把手里那份挪过去）：服务端才是权威，
    // 而且恢复回来的那一份要参与排序/筛选。
    await loadRecipes(true)
  }

  const runRecipes = async (input: {
    recipeRevisionId: number
    runs: Array<{
      inputs: Record<string, unknown>
      clientRunId?: string
      /**
       * 媒体资产：**键 = 字段键**（recipe inputSchema 的 key，如 "475::image"）。
       *
       * 位置数组（`asset-N` 下标）只是 HTTP 的兼容编码——资产的语义是"写进哪个
       * 节点字段"，由键决定；数组位置既可能漂移又会被上游顺序左右。
       */
      assets?: Record<string, { assetId: number; checksum: string }>
    }>
    idempotencyKey?: string
  }) => {
    const operationKey =
      input.idempotencyKey ?? `ui-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`
    const result = await v2.createOperation(
      operationKey,
      input.runs.map((run, index) => ({
        clientRunId: run.clientRunId ?? `${operationKey}-run-${index + 1}`,
        recipeRevisionId: input.recipeRevisionId,
        inputs: run.inputs,
        ...(run.assets && Object.keys(run.assets).length ? { assets: run.assets } : {})
      }))
    )
    activeOperationId.value = result.operationId
    try {
      await refreshOperation(result.operationId)
    } catch {
      // 创建成功后详情读取失败不应伪装成提交失败；Runner 会继续轮询。
    }
    return result
  }

  const runRecipe = async (input: {
    recipeRevisionId: number
    inputs: Record<string, unknown>
    idempotencyKey?: string
    clientRunId?: string
    /** 键 = 字段键（见 `runRecipes` 的同款说明）。 */
    assets?: Record<string, { assetId: number; checksum: string }>
  }) =>
    runRecipes({
      recipeRevisionId: input.recipeRevisionId,
      idempotencyKey: input.idempotencyKey,
      runs: [{ inputs: input.inputs, clientRunId: input.clientRunId, assets: input.assets }]
    })

  const refreshOperation = async (operationId: number) => {
    const existingRequest = operationDetailPromises.get(operationId)
    if (existingRequest) return existingRequest
    const request = (async () => {
      const detail = await v2.getOperation(operationId)
      const index = operations.value.findIndex((item) => item.operationId === operationId)
      if (index === -1) operations.value.unshift(detail)
      else operations.value[index] = detail
      const summary = operationSummaries.value.find((item) => item.operationId === operationId)
      if (summary) {
        summary.status = detail.status
        summary.runCount = detail.runs.length
      } else {
        operationSummaries.value.unshift({
          operationId: detail.operationId,
          status: detail.status,
          runCount: detail.runs.length,
          createdAt: detail.createdAt
        })
      }
      return detail
    })()
    operationDetailPromises.set(operationId, request)
    try {
      return await request
    } finally {
      operationDetailPromises.delete(operationId)
    }
  }

  const loadOperationDetail = async (operationId: number) => {
    const existing = operations.value.find((operation) => operation.operationId === operationId)
    const summary = operationSummaries.value.find((item) => item.operationId === operationId)
    if (
      existing &&
      summary &&
      existing.status === summary.status &&
      existing.runs.length === summary.runCount
    ) {
      return existing
    }
    return refreshOperation(operationId)
  }

  const cancelOperation = async (operationId: number) => {
    const result = await v2.cancelOperation(operationId)
    await refreshOperation(operationId)
    return result
  }

  const activeOperation = () =>
    operations.value.find((item) => item.operationId === activeOperationId.value) ?? null

  const isActiveTerminal = () => {
    const operation = activeOperation()
    return operation ? isTerminalStatus(operation.status) : false
  }

  return {
    recipes,
    removedRecipes,
    removedLoaded,
    removedLoading,
    connections,
    operations,
    isLoaded,
    isLoading,
    error,
    operationsError,
    activeOperationId,
    loadRecipes,
    loadRemovedRecipes,
    loadConnections,
    loadOperations,
    loadOperationSummaries,
    createRecipe,
    exportLibrary,
    importLibrary,
    importRecipe,
    importWorkflowRecipe,
    importComfyUIRecipe,
    previewComfyUIFields,
    fetchComfyUIWorkflow,
    updateRecipe,
    fetchRecipeSchema,
    toggleFavorite,
    touchRecipe,
    removeRecipe,
    restoreRecipe,
    runRecipe,
    runRecipes,
    connectionsLoaded,
    connectionsLoading,
    operationSummaries,
    refreshOperation,
    loadOperationDetail,
    cancelOperation,
    activeOperation,
    isActiveTerminal
  }
})
