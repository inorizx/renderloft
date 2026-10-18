// V2 API 客户端：RenderLoft /api/v1 的浏览器端封装。
// 统一信封解析、结构化错误、camelCase DTO。前端迁移的起点。

import type { RunUsage } from '@/shared/lib/runUsage'
import { useLocale } from '@/shared/composables/useLocale'

// 错误消息同样要双语：useLocale().t 读的是模块级 locale ref，setup 外调用也成立。
const { t } = useLocale()

export interface ApiError {
  status: number
  code: string
  kind: string
  message: string
  retryable: boolean
  details: Record<string, unknown>
}

export class V2ApiError extends Error {
  constructor(public readonly error: ApiError) {
    super(error.message)
    this.name = 'V2ApiError'
  }
}

export interface Envelope<T> {
  contractVersion: string
  ok: boolean
  data: T
  error?: ApiError
}

/**
 * 已保存密钥的**指纹**：只有尾 4 位与长度，**密钥本体永不出凭据库**。
 *
 * 为什么要有它：用户需要确认"我填的是哪把 Key"（两个站点两把是常态），而把完整密钥发到
 * 前端等于拆掉「密钥不出凭据库」这条边界，每一次回显也都是一次泄漏面（截图、录屏、剪贴板、
 * 浏览器扩展）。尾 4 位是 GitHub / 云控制台的口径：够认人，推不出全量。
 *
 * `saved: false` 表示这个槽位没有可用密钥（从没填过，或凭据库里的值已经不在）——
 * 此时**没有** `tail` / `length`，界面不要说"尾号 undefined"。
 */
export interface SecretHint {
  saved: boolean
  tail?: string
  length?: number
}

export interface RecipeSummary {
  recipeId: number
  name: string
  description: string
  capability: string
  currentRevisionId: number
  visibility: 'private' | 'shared'
  labels?: string[]
  favorite?: boolean
  group?: string
  note?: string
  lastOpenedAt?: string
  createdAt?: string
  driverKey?: string
  /**
   * 配方**种类**（`rh-app` / `rh-workflow` / `comfyui-workflow` / `openai-image` / `other`）。
   *
   * 与 `driverKey` 是两个问题：种类答"**这是什么**"（卡片上的标签用它），驱动答"**在哪跑**"
   * （紧挨着的连接名答得更具体）。由后端从绑定的形状派生——判据只留一份。
   */
  kind?: string
  defaultConnectionId?: number
  /**
   * 字段备注（schema key → 人话名字）。**原始备注**，不是合并进 schema 的那一份。
   *
   * 编辑框要用它预填：合并后的 `description` 含平台给的字段名（`text`、`batch_size`），
   * 拿它当"用户填过的值"会让用户以为自己写过。
   */
  fieldLabels?: Record<string, string>
  /**
   * 移除时间（墓碑）。活着的应用是 `null`。
   *
   * 移除是**打墓碑**而不是销毁：应用不再出现在资源库里，但历史任务照旧可解析，
   * 而且随时可以恢复。界面据此显示「移除于 …」——否则前端只能靠"它不在列表里"猜。
   */
  removedAt?: string | null
}

/**
 * 「为什么没在动」——后端给的**稳定原因码** + 一句人话 + 死线 + 到点会怎样 + 现在能做什么。
 *
 * 为什么界面需要它：排队、退避重试、提交结果不明这几种，状态**都显示「进行中」**
 * （实测吃过一次 40 分钟误报：对外报"预计 10 分钟出片"，实际要等到 1 小时死线才判失败）。
 * 有了它，界面能说清"它在等什么、等到什么时候、到点会怎样"。
 *
 * ⚠️ `message` / `deadlineEffect` 是**后端的中文原文**（给 CLI / MCP / Agent 用的）。
 * 界面**不直接显示它们**——那两句在英文界面里会是中文。界面按 `reason` 取自己的 i18n 文案
 * （见 `@/shared/lib/attention.ts`），原因码才是机器契约。
 */
export interface RunAttention {
  /** 稳定原因码：`SUBMISSION_UNCERTAIN` / `RETRY_BACKOFF` / `SUBMITTING` / `QUEUED`。 */
  reason: string
  message: string
  since: string
  /** 死线（毫秒时间戳字符串）；没有死线的档位是 `null`。 */
  deadline?: string | null
  deadlineEffect?: string | null
  nextActions: string[]
}

/**
 * 列表摘要里的**精简版**：只有原因码与死线。
 *
 * 后端刻意不在摘要里带人话与 nextActions（一页 50 条不该带上 50 份散文）——
 * 要看全的调 `operation get`。
 */
export interface AttentionSummary {
  reason: string
  deadline?: string | null
}

export interface RunRef {
  runId: number
  operationId: number
  clientRunId: string
  sequence: number
  recipeRevisionId: number
  connectionRevisionId: number
  status: string
  trackingState: string
  /**
   * 提交时用的输入。**详情接口会发**（`GET /api/v1/operations/{id}` 与 `GET /api/v1/runs/{id}`）。
   *
   * ⚠️ 2026-10-18 才补上：此前 `run_response` **从来没发过这个字段**，而服务出去的文案
   * （`renderloft_get_operation` 的描述、cookbook §11 的恢复建议）早就承诺了它 ——
   * 也就是说界面与调用方照文案去做会拿不到，而它正处在"要不要再花一次钱"的判断里。
   * 这条注释原先写的是"后端一直在发，这里只是没声明"：**那句话是错的**，是照着文案想当然写的，
   * 直到给预览框做"结果按输入分组"时发现界面拿不到 inputs，回头一查才查出来。
   * 列表接口按设计**不带** runs、也不带 inputs（只回摘要）。
   *
   * 界面用它给结果贴「这条来自哪条输入」的标签（见 `shared/lib/runLabel.ts`）——
   * 批量里"哪几张图对应哪条提示词"要靠它回答。
   */
  inputs?: Record<string, unknown>
  /**
   * 这条 run 属于**哪个应用**（由它自己的修订反查；`null` = 应用已删或查不到）。
   *
   * 界面靠它实现"**用这套参数再跑一次**"：路由是 `/library/:recipeId`，只有修订 id 跳不过去。
   * `null` 时那个入口**不出现**——宁可不给按钮，也不要跳到一个 schema 都不一样的表单里。
   */
  recipeId?: number | null
  /**
   * 提交时用的**素材**，形状与 `POST /operations/validate` 的解析结果一致。
   *
   * `fieldKey` 为空的是**旧的位置编码**：不能当字段键用（资产的真实身份是「(修订, 字段键)」，
   * 不是数组下标），所以复用参数时**跳过**它们并让用户重选，而不是猜一个键。
   */
  assets?: Array<{ fieldKey?: string | null; assetId: number; checksum: string }>
  /** 「它为什么没在动」；正常在跑与终态是 `null`。 */
  attention?: RunAttention | null
  /**
   * 平台报的用量（**只转述，不推算**）：`{ unit: "coins", amount: "6", taskCostTime: "27" }`。
   *
   * `taskCostTime` 是**任务耗时**（秒，字符串；批量输出会求和）——平台**按时长计费**，
   * 所以"这张为什么贵"要靠它回答。取不到就是"不知道"：本地 ComfyUI 这类没有计费概念的
   * 连接不会有这个字段，界面据此**不显示**，而不是编一个 0。
   */
  usage?: RunUsage | null
  /** 终态结构化错误（失败/取消时由后端持久化返回）。 */
  error?: RunError
  createdAt: string
  updatedAt: string
}

/** 与后端 SafeError 对齐的结构化错误。 */
export interface RunError {
  code: string
  kind: string
  message: string
  retryable: boolean
  details?: Record<string, unknown>
}

export interface OperationSummary {
  operationId: number
  status: string
  runCount: number
  createdAt: string
  /** 摘要版的「为什么没在动」；没有就是 `null`（"没有"与"有但空"必须能分辨）。 */
  attention?: AttentionSummary | null
}

export interface OperationDetail {
  operationId: number
  status: string
  actorId: string
  origin: string
  idempotencyKey: string
  requestHash: string
  metadata: Record<string, unknown>
  createdAt: string
  updatedAt: string
  /** 由后端从首个 run 的配方解析出的应用名；配方被删/解析失败时为 undefined。 */
  recipeName?: string
  /**
   * operation 级「为什么没在动」：这条任务里**最该被看见**的那条 run 的 attention
   * （不确定 > 退避 > 提交中 > 排队）。每个 run 上也各有一份。
   */
  attention?: RunAttention | null
  runs: RunRef[]
}

export interface CreateOperationResult {
  operationId: number
  status: string
  runIds: number[]
  statusUrl: string
  eventsUrl: string
}

export interface Page<T> {
  items: T[]
  /** 后端分页端点返回；无分页的端点（agents/messages）为 undefined */
  nextCursor?: number | null
  hasMore?: boolean
}

const env = (import.meta as unknown as { env?: Record<string, string> }).env ?? {}
const BASE = env.VITE_RENDERLOFT_URL ?? ''
const TOKEN = env.VITE_RENDERLOFT_TOKEN ?? ''
/** 普通请求超时；发送 Agent 消息等长任务可单独传更长超时。 */
const DEFAULT_TIMEOUT_MS = 30_000

async function request<T>(
  method: string,
  path: string,
  body?: unknown,
  timeoutMs = DEFAULT_TIMEOUT_MS
): Promise<T> {
  const headers: Record<string, string> = {}
  if (TOKEN) headers.Authorization = `Bearer ${TOKEN}`
  if (body !== undefined) headers['Content-Type'] = 'application/json'
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), timeoutMs)
  let response: Response
  try {
    response = await fetch(`${BASE}${path}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: controller.signal,
      // 生产模式（daemon 直接伺服控制台）用 bootstrap 会话 cookie 认证；
      // dev 模式由 vite 代理注入 Bearer，携带 cookie 无副作用。
      credentials: 'include'
    })
  } catch (caught) {
    clearTimeout(timer)
    if (caught instanceof DOMException && caught.name === 'AbortError') {
      throw new V2ApiError({
        status: 0,
        code: 'REQUEST_TIMEOUT',
        kind: 'internal',
        message: t('api.timeout', { seconds: timeoutMs / 1000 }),
        retryable: false,
        details: {}
      })
    }
    throw new V2ApiError({
      status: 0,
      code: 'NETWORK_ERROR',
      kind: 'network',
      message: caught instanceof Error ? caught.message : String(caught),
      retryable: true,
      details: {}
    })
  }
  clearTimeout(timer)
  // 先取原文再解析：Response body 只能消费一次，
  // 若直接 response.json() 失败，服务端原文就再也取不回来，
  // 所有 body 反序列化错误都会被抹平成同一句"无法解析的响应"。
  const raw = await response.text()
  let envelope: Envelope<T>
  try {
    envelope = JSON.parse(raw) as Envelope<T>
  } catch {
    // 两类真实场景：① 后端反序列化失败（axum JsonRejection 返回纯文本 422）；
    // ② 网关/代理返回 HTML 错误页。都保留原文，避免丢失定位线索。
    const excerpt = raw.slice(0, 500)
    throw new V2ApiError({
      status: response.status,
      code: 'INVALID_RESPONSE',
      kind: 'internal',
      message: t('api.nonJsonResponse', { status: response.status, excerpt }),
      retryable: false,
      details: { raw: excerpt }
    })
  }
  if (!envelope.ok || !response.ok) {
    throw new V2ApiError(
      envelope.error ?? {
        status: response.status,
        code: 'UNKNOWN',
        kind: 'internal',
        message: t('api.unknownError'),
        retryable: false,
        details: {}
      }
    )
  }
  return envelope.data
}

export const v2 = {
  get<T>(path: string): Promise<T> {
    return request<T>('GET', path)
  },
  post<T>(path: string, body?: unknown, timeoutMs?: number): Promise<T> {
    return request<T>('POST', path, body, timeoutMs)
  },
  put<T>(path: string, body?: unknown): Promise<T> {
    return request<T>('PUT', path, body)
  },
  delete<T>(path: string): Promise<T> {
    return request<T>('DELETE', path)
  },

  listRecipes(limit = 50): Promise<Page<RecipeSummary>> {
    return this.get(`/api/v1/recipes?limit=${limit}`)
  },
  /**
   * 列出**已移除**（墓碑）的应用。
   *
   * `removed=true` 不是"在活着的里面筛一部分"，而是**另一个集合**（墓碑）。
   * 它天然是个小集合，所以服务端不分页。
   */
  listRemovedRecipes(limit = 200): Promise<Page<RecipeSummary>> {
    return this.get(`/api/v1/recipes?removed=true&limit=${limit}`)
  },
  /** 从"已移除"里恢复一个应用。不在墓碑里的会拿到 404 `RECIPE_NOT_IN_TRASH`。 */
  restoreRecipe(recipeId: number): Promise<{ restored: boolean }> {
    return this.post(`/api/v1/recipes/${recipeId}/restore`)
  },
  getRecipe(
    recipeId: number
  ): Promise<{ recipe: RecipeSummary; inputSchema: unknown; outputSchema: unknown }> {
    return this.get(`/api/v1/recipes/${recipeId}`)
  },
  getRecipeSchema(recipeId: number): Promise<{
    recipeId: number
    /** 人看的修订号（单调递增）。 */
    revision: number
    /** 修订记录主键：提交任务时用的 `recipeRevisionId` 就是它。 */
    revisionId: number
    inputSchema: unknown
    outputSchema: unknown
    defaults?: Record<string, unknown>
    /**
     * 这条配方绑定的连接**声明**能跑的模型（`config.models`，空 = 没声明 ⇒ 不过滤）。
     *
     * 放在 schema 这条路上是因为跑表单本来就只调这一个端点——省得它再去查一遍连接，
     * 也就不会出现"表单拿到了清单、但用的不是这条连接"的偏差。
     */
    modelOptions?: string[]
  }> {
    return this.get(`/api/v1/recipes/${recipeId}/schema`)
  },

  createOperation(
    idempotencyKey: string,
    runs: Array<{
      clientRunId: string
      recipeRevisionId: number
      connectionId?: number
      inputs: Record<string, unknown>
      /**
       * 媒体资产：**键 = 字段键**（如 "475::image"）。服务端也收位置数组，
       * 但那只为旧 revision 的兼容保留——新代码一律用字段键。
       */
      assets?: Record<string, { assetId: number; checksum: string }>
    }>
  ): Promise<CreateOperationResult> {
    return this.post('/api/v1/operations', { idempotencyKey, runs })
  },
  getOperation(operationId: number): Promise<OperationDetail> {
    return this.get(`/api/v1/operations/${operationId}`)
  },
  listOperations(limit = 50): Promise<Page<OperationSummary>> {
    return this.get(`/api/v1/operations?limit=${limit}`)
  },
  cancelOperation(operationId: number): Promise<{
    operationId: number
    cancelled: number[]
    alreadyTerminal: number[]
    pending: number[]
    remoteMayContinue: number[]
  }> {
    // cancel 是**变更**操作 ⇒ 用 POST（与 retry / delete 一致）。此前用的 GET 违反 HTTP 语义。
    return this.post(`/api/v1/operations/${operationId}/cancel`)
  },
  retryOperation(operationId: number): Promise<{ operationId: number }> {
    return this.post(`/api/v1/operations/${operationId}/retry`)
  },
  retryRun(operationId: number, runId: number): Promise<{ operationId: number }> {
    return this.post(`/api/v1/operations/${operationId}/runs/${runId}/retry`)
  },
  deleteOperation(operationId: number): Promise<{ deleted: boolean }> {
    return this.delete(`/api/v1/operations/${operationId}`)
  },
  operationEvents(
    operationId: number,
    after = 0
  ): Promise<
    Array<{
      eventId: number
      eventType: string
      operationId: number
      runId: number | null
      sequence: number
      createdAt: string
      payload: unknown
    }>
  > {
    return this.get(`/api/v1/operations/${operationId}/events?after=${after}`)
  },

  getArtifact(artifactId: number): Promise<{
    artifactId: number
    filename: string
    mimeType: string
    byteSize: number
    checksum: string
  }> {
    return this.get(`/api/v1/artifacts/${artifactId}`)
  },
  /**
   * 按 id 取**素材的元数据**（不取字节；字节见 `fetchAssetBytes`）。
   *
   * 用途很具体：把某次任务"用这套参数再跑一次"时，run 上只有 `{fieldKey, assetId, checksum}`，
   * 而表单的文件位要显示**文件名**。与其让后端在详情里多塞一个字段（每次轮询都要付），
   * 不如在这里按需取一次——只有真的要复用时才发生。
   */
  getAsset(assetId: number): Promise<{
    assetId: number
    filename: string
    mimeType: string
    byteSize: number
    checksum: string
  }> {
    return this.get(`/api/v1/assets/${assetId}`)
  },
  listArtifacts(runId: number): Promise<
    Page<{
      artifactId: number
      runId: number
      filename: string
      mimeType: string
      byteSize: number
      checksum: string
    }>
  > {
    return this.get(`/api/v1/artifacts?runId=${runId}`)
  },
  /**
   * 全局产物列表，**最新在前**（「结果」视角用它）。
   *
   * 为什么不能用 `listArtifacts` 那套：它的第一页是**最旧**的一批（升序游标），而"我刚生成的那张
   * 在哪"要的是反方向 —— 把整个列表翻完再倒序是拿流量换顺序。所以服务端多了一个方向：
   * `order=desc` 配 `before`（比它更旧）。
   */
  listArtifactsNewest(
    limit: number,
    before?: number
  ): Promise<
    Page<{
      artifactId: number
      runId: number
      filename: string
      mimeType: string
      byteSize: number
      checksum: string
    }>
  > {
    const cursor = before === undefined ? '' : `&before=${before}`
    return this.get(`/api/v1/artifacts?order=desc&limit=${limit}${cursor}`)
  },
  /**
   * 取**原始字节**。artifact 与 asset 的 `/bytes` 端点共用（这类响应不是 JSON 信封）。
   * 返回 Blob 而不碰 DOM —— object URL 的创建与回收属视图职责。
   */
  async fetchBytes(path: string, fallbackName: string): Promise<{ blob: Blob; filename: string }> {
    const headers: Record<string, string> = {}
    if (TOKEN) headers.Authorization = `Bearer ${TOKEN}`
    const response = await fetch(`${BASE}${path}`, {
      headers,
      credentials: 'include'
    })
    if (!response.ok) {
      const raw = await response.text()
      let envelope: Envelope<unknown> | null = null
      try {
        envelope = JSON.parse(raw) as Envelope<unknown>
      } catch {
        envelope = null
      }
      if (envelope?.error) {
        throw new V2ApiError(envelope.error as ApiError)
      }
      const excerpt = raw.slice(0, 500)
      throw new V2ApiError({
        status: response.status,
        code: 'DOWNLOAD_FAILED',
        kind: 'internal',
        message: excerpt
          ? t('api.downloadFailedWithDetail', { status: response.status, excerpt })
          : t('api.downloadFailed', { status: response.status }),
        retryable: false,
        details: excerpt ? { raw: excerpt } : {}
      })
    }
    // ⚠️ 先认 `filename*=UTF-8''…`（RFC 5987）：只读 `filename="…"` 会拿到 daemon 给的
    // **ASCII 回退**（中文全被换成 `_`），而中文名本来就是我们要保住的东西。
    const disposition = response.headers.get('content-disposition') ?? ''
    const encoded = /filename\*=UTF-8''([^;]+)/i.exec(disposition)?.[1]
    const plain = /filename="([^"]+)"/.exec(disposition)?.[1]
    return {
      blob: await response.blob(),
      filename: encoded ? decodeURIComponent(encoded) : (plain ?? fallbackName)
    }
  },
  async fetchArtifactBytes(artifactId: number): Promise<{ blob: Blob; filename: string }> {
    return this.fetchBytes(`/api/v1/artifacts/${artifactId}/bytes`, `artifact-${artifactId}`)
  },
  /** 取上传资产（如图片素材）的原始字节，供历史附图展示。 */
  async fetchAssetBytes(assetId: number): Promise<Blob> {
    const { blob } = await this.fetchBytes(`/api/v1/assets/${assetId}/bytes`, `asset-${assetId}`)
    return blob
  },
  /**
   * 把一个产物**落盘**到「下载目录」设置里那个目录，返回**完整路径**。
   *
   * 为什么不让浏览器/外壳去下载（2026-10-12 定）：桌面外壳是 WebView2，它**不弹下载提示、
   * 落点也不可知**——文件其实进了系统下载目录，界面上却什么都没发生，我们连"存哪了"都说不出来。
   * 由本机 daemon 写盘，路径是确定的，界面才能如实显示路径并给出"在文件夹中显示"。
   * 重名按浏览器口径递增（`名字 (1).png`），不覆盖用户已有的文件。
   */
  saveArtifact(artifactId: number): Promise<{
    path: string
    filename: string
    directory: string
  }> {
    return this.post(`/api/v1/artifacts/${artifactId}/save`)
  },
  /**
   * 在系统文件管理器里打开「下载目录」；给了 `reveal` 就顺带**选中那个文件**。
   *
   * 选中而不是只开目录是主流做法（浏览器、VS Code 的"在文件夹中显示"）。`reveal` 必须落在
   * 下载目录内，daemon 会自己核对（界面不该成为"打开任意路径"的开关）。
   */
  openDownloadDir(reveal?: string): Promise<{ directory: string }> {
    const query = reveal ? `?reveal=${encodeURIComponent(reveal)}` : ''
    // ⚠️ 路径是 `/settings/open-download-dir`（handler 在 shell.rs 里，但路由挂在 settings 下）。
    // 按模块名猜成 `/shell/…` 会拿到 ROUTE_NOT_FOUND——而调用方把异常吞了，界面上就是"点了没反应"。
    return this.post(`/api/v1/settings/open-download-dir${query}`)
  },

  // ---------- connections ----------

  listConnections(): Promise<
    Array<{
      connectionId: number
      vendor: string
      protocol: string
      displayName: string
      currentRevisionId: number
      status: string
    }>
  > {
    return this.get('/api/v1/connections')
  },
  getConnection(connectionId: number): Promise<{
    connection: {
      connectionId: number
      vendor: string
      protocol: string
      displayName: string
      currentRevisionId: number
      status: string
    }
    config: Record<string, unknown>
    /**
     * 这条连接**实际会用到**的密钥槽位：自己的，或凭据引用借来的（见 `config.sourceConnectionId`）。
     *
     * 为什么给"有效"而不是"自己存了哪些"：读它的地方（就绪判定、CLI 的覆盖守卫）问的都是
     * 同一个问题——"这条连接能不能认证"。回原始的会让每个调用方各自再实现一遍引用解析。
     */
    secretBindings: Array<{ slot: string; resolution: 'current' | 'pinned' }>
    /** 密钥的身份（尾 4 位 + 长度），值不给——见 `SecretHint` 上的说明。同样取**有效**的那把。 */
    secretHint?: SecretHint
    lifecycleFeatures: Record<string, boolean>
  }> {
    return this.get(`/api/v1/connections/${connectionId}`)
  },
  createConnection(input: {
    vendor: string
    protocol: string
    displayName: string
    config: Record<string, unknown>
    capabilities?: string[]
    secretBindings?: Array<{
      slot: string
      resolution: 'current' | 'pinned'
      value?: string
    }>
    lifecycleFeatures?: Record<string, boolean>
  }): Promise<{ connectionId: number; currentRevisionId: number }> {
    return this.post('/api/v1/connections', input)
  },
  updateConnection(
    connectionId: number,
    input: {
      displayName?: string
      config?: Record<string, unknown>
      capabilities?: string[]
      secretBindings?: Array<{
        slot: string
        resolution: 'current' | 'pinned'
        value?: string
      }>
      lifecycleFeatures?: Record<string, boolean>
    }
  ): Promise<{
    connectionId: number
    vendor: string
    protocol: string
    displayName: string
    currentRevisionId: number
    status: string
  }> {
    return this.put(`/api/v1/connections/${connectionId}`, input)
  },
  checkConnection(connectionId: number): Promise<{
    connectionId: number
    ok: boolean
    capabilities: string[]
    message: string | null
  }> {
    return this.get(`/api/v1/connections/${connectionId}/check`)
  },
  /**
   * 这条连接背后平台声明的可用模型（`GET {baseUrl}/models` 那类）。
   *
   * ⚠️ 没有清单的驱动会回 `MODELS_NOT_SUPPORTED`，**不是空数组**——空数组的意思是
   * "平台说它一个模型都没有"。界面要能分开这两件事。
   */
  listConnectionModels(connectionId: number): Promise<{ connectionId: number; models: string[] }> {
    return this.get(`/api/v1/connections/${connectionId}/models`)
  },
  deleteConnection(connectionId: number): Promise<{ deleted: boolean }> {
    return this.delete(`/api/v1/connections/${connectionId}`)
  },

  // ---------- assets ----------

  /**
   * 上传素材（图片/视频等），返回 daemon 里的 assetId。
   *
   * ⚠️ 这条端点的契约是 **query 参数 + 原始字节体**（`?filename=…&mimeType=…`，body 就是文件字节），
   * 不是 JSON 信封——CLI 那侧一直这么发（`clients/client.rs::upload_asset`），而**界面这边原来
   * 发的是 `{filename, mimeType, bytes: [1,2,3,…]}`**：query 里没有 filename ⇒ daemon 直接回
   * 「filename 不能为空」（实测就是这么撞上的），而且就算参数对了，真实图片过一遍 JSON 数字数组
   * 会膨胀约 4 倍、撞上 axum 2 MB 的请求体上限（CLI 那边踩过同一个坑，注释还留着）。
   * **字节就是字节，别过文本编码。**
   */
  async uploadAsset(
    filename: string,
    mimeType: string,
    bytes: Uint8Array
  ): Promise<{ assetId: number; checksum: string }> {
    const query = new URLSearchParams({ filename, mimeType })
    const headers: Record<string, string> = { 'Content-Type': mimeType }
    if (TOKEN) headers.Authorization = `Bearer ${TOKEN}`
    const response = await fetch(`${BASE}/api/v1/assets?${query.toString()}`, {
      method: 'POST',
      headers,
      credentials: 'include',
      // `BodyInit` 想要 BufferSource；`Uint8Array` 在某些 TS 版本里不算，转一层 ArrayBuffer 最稳。
      body: bytes.slice().buffer as ArrayBuffer
    })
    const envelope = (await response.json()) as Envelope<{ assetId: number; checksum: string }>
    if (!envelope.ok || !response.ok || !envelope.data) {
      throw new V2ApiError(
        envelope.error ?? {
          status: response.status,
          code: 'UNKNOWN',
          kind: 'internal',
          message: t('api.unknownError'),
          retryable: false,
          details: {}
        }
      )
    }
    return envelope.data
  },

  // ---------- capabilities ----------

  capabilities(): Promise<{
    contractVersion: string
    protocols: Array<{
      vendor: string
      protocol: string
      displayName: string
      capabilities: string[]
      connectionSchema: unknown
      bindingSchema: unknown
    }>
  }> {
    return this.get('/api/v1/capabilities')
  }
}

export function isTerminalStatus(status: string): boolean {
  return (
    status === 'SUCCEEDED' || status === 'PARTIAL' || status === 'FAILED' || status === 'CANCELLED'
  )
}
