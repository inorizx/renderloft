<script setup lang="ts">
// 连接页：统一管理生成源（RunningHub 国内/国际、ComfyUI、OpenAI 图片）。
// 每类可存在多个连接（如 RunningHub .cn 与 .ai 独立平台数据不互通，各配一个连接，
// Recipe 通过 binding 绑定各自连接）。
import { computed, onMounted, reactive, ref, watch } from 'vue'
import { v2 } from '@/shared/api/v2'
import type { SecretHint } from '@/shared/api/v2'
import { useAppStore } from '@/shared/stores/app'
import { useConnectionFacetsStore } from '@/shared/stores/connection-facets'
import { formatAccountFacet, formatQueueFacet } from '@/shared/lib/connectionFacet'
import type { DiagnosticInfo } from '@/shared/types/runninghub'
import {
  isConnectionReady,
  invalidateExecutionConnections,
  useExecutionConnections
} from '@/shared/composables/useExecutionConnections'
import { useLocale } from '@/shared/composables/useLocale'
import PageContainer from '@/components/layout/PageContainer.vue'
import PageHeader from '@/components/PageHeader.vue'
import Card from '@/components/ui/Card.vue'
import Button from '@/components/ui/Button.vue'
import Dialog from '@/components/ui/Dialog.vue'
import Input from '@/components/ui/Input.vue'
import Select from '@/components/ui/Select.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'

const { t } = useLocale()
const facetsStore = useConnectionFacetsStore()

/**
 * 连接级可选参数的**默认值与范围**：取自 daemon 的诊断接口，**不在前端写死**。
 *
 * 为什么值得为两个占位数字多打一次 GET：这两处的文案是"留空就用默认值（120 秒），
 * 范围 10–3600"——那个默认值和范围正是 daemon 真正在用的数。前端抄一份，改的时候就漂了
 * （这个仓库为"同一规则留两处"栽过好几次）。
 *
 * 拿不到时退回内置兜底值：**它只影响输入框里的提示文字与占位符**，不影响实际行为
 * （真正生效的值永远由 daemon 决定）。
 */
const optionDefaults = ref<DiagnosticInfo['defaults']>()
const defaultRequestTimeout = computed(() => optionDefaults.value?.requestTimeoutSeconds ?? 120)
const defaultTrackingDeadline = computed(
  () => optionDefaults.value?.trackingDeadlineSeconds ?? 3600
)
const requestTimeoutRange = computed(() =>
  optionDefaults.value
    ? `${optionDefaults.value.requestTimeoutMin}–${optionDefaults.value.requestTimeoutMax}`
    : '10–3600'
)
const trackingDeadlineRange = computed(() =>
  optionDefaults.value
    ? `${optionDefaults.value.trackingDeadlineMin}–${optionDefaults.value.trackingDeadlineMax}`
    : '60–86400'
)

type TypeKey = 'runninghub' | 'comfyui' | 'openaiImages'

interface ConnItem {
  connectionId: number
  vendor: string
  protocol: string
  displayName: string
  status: string
  /** 被冻结的原因（key 失效 / 余额不足）。有值时状态显示「已暂停接单」。 */
  pausedReason?: string
  baseUrl: string
  apiKeyConfigured: boolean
  detailsAvailable: boolean
  healthOk?: boolean
  /** 健康检查还在路上：这一条的状态先显示「测试中…」，不要当成"未知/失败"。 */
  checking?: boolean
  /**
   * 用户**手动**点过「测试」且通过。
   *
   * 为什么要单独记一笔：手动测试此前唯一的反馈是右下角那条 3 秒的提示条——徽章不变、
   * 行内没有任何痕迹，提示条一过就像"点了没反应"（实测就是这么报的）。手动测试的结果
   * 必须**落在被点的那个地方**：徽章变成「连接正常」并留到下次刷新。
   */
  testedOk?: boolean
  /**
   * 上一次检查失败的原因（成功时清空）。
   *
   * ⚠️ 徽章只说「连接测试失败」等于没说：用户 2026-10-12 就是看着这四个字来问
   * "是因为没 key 吗"。真正的原因（连不上 / 平台回了 401 / 代理没在跑）本来就在
   * 错误信封的 message 里，之前只在右下角提示条闪一下。**失败原因必须留在行内。**
   */
  healthMessage?: string
  defaultModel?: string
}

interface ConnectionTypeDef {
  key: TypeKey
  label: string
  short: string
  description: string
  /** 一句话场景说明：这个渠道覆盖什么、什么时候用它。 */
  scope: string
  /** API Key 去哪里获取（按渠道给不同引导）。 */
  apiKeyHint?: string
  /** 自定义 Base URL 时的帮助说明。 */
  baseUrlHint?: string
  capabilities: string[]
  lifecycleFeatures: Record<string, boolean>
  defaultBaseUrl: string
  requiresApiKey: boolean
  extraFields?: 'defaultModel'[]
}

// 返回类型必须显式标注：四个条目的 lifecycleFeatures 键各不相同，不标注时 TS 会把数组
// 推成含 `synchronous_result?: undefined` 的联合类型，反而塞不进 Record<string, boolean>。
const TYPE_DEFS = computed((): ConnectionTypeDef[] => [
  {
    key: 'runninghub',
    label: t('connection.runningHub'),
    short: 'RH',
    description: t('provider.runningHub.description'),
    scope: t('provider.runningHub.scope'),
    apiKeyHint: t('provider.runningHub.apiKeyHint'),
    baseUrlHint: t('provider.runningHub.baseUrlHint'),
    capabilities: ['workflow-execution'],
    lifecycleFeatures: { asset_upload: true, remote_reconciliation: true },
    defaultBaseUrl: 'https://www.runninghub.cn',
    requiresApiKey: true
  },
  {
    key: 'comfyui',
    label: t('connection.comfyui'),
    short: 'CF',
    description: t('provider.comfyui.description'),
    scope: t('provider.comfyui.scope'),
    baseUrlHint: t('provider.comfyui.baseUrlHint'),
    capabilities: ['workflow-execution'],
    lifecycleFeatures: { asset_upload: true, remote_reconciliation: true },
    defaultBaseUrl: 'http://127.0.0.1:8188',
    requiresApiKey: false,
    // 本地 ComfyUI 没有鉴权 ⇒ 这个 key 是可选的；它只服务 rh-proxy 与带鉴权的远程地址。
    // 不写 hint 的话字段会光秃秃地显示"API Key"，本地用户会以为连本地也要 key。
    apiKeyHint: t('provider.comfyui.apiKeyHint')
  },
  {
    key: 'openaiImages',
    label: t('connection.openAiImage'),
    short: 'OI',
    description: t('provider.openaiImages.description'),
    scope: t('provider.openaiImages.scope'),
    apiKeyHint: t('provider.openaiImages.apiKeyHint'),
    baseUrlHint: t('provider.openaiImages.baseUrlHint'),
    capabilities: ['text-to-image'],
    lifecycleFeatures: { synchronous_result: true },
    defaultBaseUrl: 'https://api.openai.com/v1',
    requiresApiKey: true,
    extraFields: ['defaultModel']
  }
])

const RUNNINGHUB_ENDPOINTS = computed(
  () =>
    [
      {
        value: 'cn',
        label: t('connections.endpointCn'),
        // ⚠️ 平台地址是 `www.runninghub.cn`，**不是 `api.runninghub.cn`**——后者这个域名
        // 根本不存在（DNS NXDOMAIN）。曾经写成 `api.` 让每个新用户都卡在第 1 步，
        // 而报错是"地址无法安全解析"，看起来像被 SSRF 守卫拦了。官方 OpenAPI 文档里的
        // `servers:` 就是 `https://www.runninghub.ai`，接口与网站同一个主机。
        baseUrl: 'https://www.runninghub.cn',
        region: 'cn',
        /**
         * 默认显示名。**两个站点必须一眼分得开**：两个平台数据独立、可以各配一条连接，
         * 而它们默认同名的话，列表和运行页的连接选择器里就是两行一模一样的「RunningHub」，
         * 只能靠底下那行小字 URL 区分——选错就等于把任务发到了另一个账号。
         */
        defaultName: 'RunningHub（国内）'
      },
      {
        value: 'ai',
        label: t('connections.endpointAi'),
        baseUrl: 'https://www.runninghub.ai',
        region: 'global',
        defaultName: 'RunningHub（国际）'
      }
    ] as const
)

const COMFYUI_PRESET_KEY = 'renderloft-comfyui-presets'
interface EndpointPreset {
  id: string
  name: string
  baseUrl: string
  /** rh 内置预设为 rh-proxy 模式；其余标准模式。 */
  mode?: 'standard' | 'rh-proxy'
}
/** 内置 ComfyUI 执行环境预设（用户可在此基础上自建命名预设）。 */
const BUILTIN_COMFYUI_PRESETS = computed<EndpointPreset[]>(() => [
  {
    id: 'local',
    name: t('connections.presetLocal'),
    baseUrl: 'http://127.0.0.1:8188',
    mode: 'standard'
  },
  {
    id: 'rh',
    name: t('connections.presetRhCloud'),
    baseUrl: 'https://www.runninghub.cn/proxy',
    mode: 'rh-proxy'
  },
  {
    /**
     * 国际站也有 `/proxy`（实测 `POST/GET https://www.runninghub.ai/proxy` 都是 401 =
     * 路径存在、缺鉴权，与 .cn 一模一样）。**必须给**：两个站点是各自的账号世界，
     * 拿 .ai 的 key 打 .cn 的代理只会认证失败，而此前下拉里只有 .cn 那一条。
     */
    id: 'rh-ai',
    name: t('connections.presetRhCloudAi'),
    baseUrl: 'https://www.runninghub.ai/proxy',
    mode: 'rh-proxy'
  }
])

/**
 * RunningHub 邀请码：**两个站点是两套账号、两个码**（实测 .cn 是 1000 币、.ai 是 500 币）。
 *
 * ⚠️ 数字与措辞取自平台当时的分享文案（2026-10-12 从平台页面抄录）。**规则由平台定、会变**——
 * 改这里一处 + README 一处，文案里也写明"以平台页面为准"。
 */
const RUNNINGHUB_INVITES: Record<'cn' | 'ai', { code: string; coins: number }> = {
  cn: { code: 'aaokutnp', coins: 1000 },
  ai: { code: '0l7oqn8y', coins: 500 }
}

const loadUserComfyPresets = (): EndpointPreset[] => {
  try {
    const raw = localStorage.getItem(COMFYUI_PRESET_KEY)
    if (!raw) return []
    const parsed = JSON.parse(raw) as EndpointPreset[]
    return Array.isArray(parsed)
      ? parsed.filter(
          (item) => item && typeof item.name === 'string' && typeof item.baseUrl === 'string'
        )
      : []
  } catch {
    return []
  }
}
const userComfyPresets = ref<EndpointPreset[]>(loadUserComfyPresets())
const persistComfyPresets = () => {
  try {
    localStorage.setItem(COMFYUI_PRESET_KEY, JSON.stringify(userComfyPresets.value))
  } catch {
    // 本地存储不可用时静默降级为仅当前会话。
  }
}
/** 下拉可选的全部 ComfyUI 执行环境：内置 + 用户预设。 */
const allComfyPresets = computed<EndpointPreset[]>(() => [
  ...BUILTIN_COMFYUI_PRESETS.value,
  ...userComfyPresets.value
])

// ---------- ComfyUI 端点预设管理（轻量，localStorage 持久化） ----------
const presetManageOpen = ref(false)
const presetName = ref('')
const presetUrl = ref('')
const presetError = ref('')

/**
 * 打开"管理预设"并清空上次的输入。
 *
 * 抽成方法而不是内联多语句：内联多语句被格式化拆行后会丢掉分号，
 * Vue 编译器随即解析失败（本仓库踩过：prettier 拆行后构建直接失败）。
 */
const openPresetManage = () => {
  presetName.value = ''
  presetUrl.value = ''
  presetError.value = ''
  presetManageOpen.value = true
}
const selectedPreset = computed(() =>
  allComfyPresets.value.find((item) => item.id === form.comfyPresetId)
)
const addPreset = () => {
  const name = presetName.value.trim()
  const url = presetUrl.value.trim()
  if (!name) {
    presetError.value = t('connections.presetNameRequired')
    return
  }
  if (!url) {
    presetError.value = t('connections.baseUrlRequired')
    return
  }
  if (
    BUILTIN_COMFYUI_PRESETS.value.some((item) => item.baseUrl === url) ||
    userComfyPresets.value.some((item) => item.baseUrl === url)
  ) {
    presetError.value = t('connections.presetUrlExists')
    return
  }
  userComfyPresets.value = [
    ...userComfyPresets.value,
    { id: `user-${Date.now()}`, name, baseUrl: url, mode: 'standard' }
  ]
  persistComfyPresets()
  presetName.value = ''
  presetUrl.value = ''
  presetError.value = ''
}
const removePreset = (id: string) => {
  userComfyPresets.value = userComfyPresets.value.filter((item) => item.id !== id)
  persistComfyPresets()
  if (form.comfyPresetId === id) form.comfyPresetId = 'local'
}

const appStore = useAppStore()
const { loadConnections: loadExecutionConnections } = useExecutionConnections()
const connections = ref<ConnItem[]>([])
const loading = ref(false)
const loadError = ref('')

const matches = (def: ConnectionTypeDef) => (conn: ConnItem) =>
  def.key === 'runninghub'
    ? conn.vendor === 'runninghub'
    : def.key === 'comfyui'
      ? conn.vendor === 'comfyui'
      : conn.vendor === 'openai' && conn.protocol === 'openai-images'

const listOf = (def: ConnectionTypeDef) => connections.value.filter(matches(def))
const anyConfigured = computed(() => connections.value.length > 0)

/**
 * 列表行上的端点说明。
 *
 * 只显示原始 URL 是看不出"本机 ComfyUI"还是"RunningHub 云端"的 —— 而这两者的行为
 * 完全不同（rh-proxy 模式没有进度事件、不能远端取消，见连接详情里的 lifecycleFeatures）。
 * 能对上预设就带上预设名。
 */
const endpointLabel = (conn: ConnItem): string => {
  const preset = [...BUILTIN_COMFYUI_PRESETS.value, ...userComfyPresets.value].find(
    (item) => item.baseUrl === conn.baseUrl
  )
  return preset ? `${preset.name} · ${conn.baseUrl}` : conn.baseUrl
}

const loadConnections = async () => {
  loading.value = true
  loadError.value = ''
  try {
    const items = await v2.listConnections()
    /*
     * 列表阶段**只取本地详情**（SQLite，毫秒级），健康检查放到下一阶段异步补。
     *
     * 为什么必须拆开：模板是 `v-if="!loading"` 罩住整页的，而 checkConnection 要打到
     * 平台（RunningHub 那次要 5 秒才超时）—— 实测整页空白 5.2 秒，连「添加连接」按钮
     * 都还没出现，用户会以为这里根本没有入口。
     */
    connections.value = await Promise.all(
      items.map(async (item) => {
        let config: Record<string, unknown> = {}
        let apiKeyConfigured = false
        let detailsAvailable = false
        try {
          const detail = await v2.getConnection(item.connectionId)
          config = detail.config
          apiKeyConfigured = detail.secretBindings.some(
            (binding) => binding.slot === 'api_key' || binding.slot.endsWith('-api_key')
          )
          detailsAvailable = true
        } catch {
          // 详情失败时保留基础信息
        }
        return {
          connectionId: item.connectionId,
          vendor: item.vendor,
          protocol: item.protocol,
          displayName: item.displayName,
          status: item.status,
          pausedReason: item.pausedReason ?? undefined,
          baseUrl: String(config.baseUrl ?? ''),
          apiKeyConfigured,
          detailsAvailable,
          healthOk: undefined,
          checking: true,
          testedOk: false,
          defaultModel: typeof config.defaultModel === 'string' ? config.defaultModel : undefined
        }
      })
    )
  } catch (error) {
    loadError.value = error instanceof Error ? error.message : String(error)
  } finally {
    loading.value = false
  }
  // 不 await：谁先回来谁先更新，慢的那条不拖累整页。
  void refreshHealth()
}

/** 健康检查：逐条更新状态。失败也只影响这一条。 */
const refreshHealth = async () => {
  await Promise.all(
    connections.value.map(async (conn) => {
      try {
        const result = await v2.checkConnection(conn.connectionId)
        conn.healthOk = result.ok
        // 失败要把**原因**留下；成功就清掉上一次的旧原因。
        conn.healthMessage = result.ok ? undefined : (result.message ?? undefined)
      } catch (error) {
        // 检查本身没跑成（网络类错误是 503 信封）：原因在错误的 message 里。
        conn.healthOk = false
        conn.healthMessage = error instanceof Error ? error.message : String(error)
      } finally {
        conn.checking = false
      }
    })
  )
  // 状态面要等健康检查落地再问：`shouldLoadFacets` 读的就是刚写下的 `healthOk`
  // （给一条刚测失败的连接查余额，只会多一次注定失败的请求）。
  void refreshFacets()
}

const statusOf = (
  conn: ConnItem,
  def: ConnectionTypeDef
): { status: 'success' | 'warning' | 'neutral'; label: string } => {
  // 检查还在路上：这一条先报「测试中…」，别当成"未知"或"失败"。
  if (conn.checking) return { status: 'neutral', label: t('connections.testing') }
  if (!conn.detailsAvailable) return { status: 'neutral', label: t('connections.statusUnknown') }
  if (conn.status !== 'active') return { status: 'warning', label: t('connections.statusDisabled') }
  // 冻结比"已配置"更要紧：用户看到的是"任务莫名不动"，这里必须说清为什么。
  if (conn.pausedReason) return { status: 'warning', label: t('connections.statusPaused') }
  if (conn.healthOk === false)
    return { status: 'warning', label: t('connections.statusTestFailed') }
  // 手动测试通过：给一个留在行内的肯定答复（自动健康检查通过时不抢这个位置）。
  if (conn.testedOk) return { status: 'success', label: t('connections.statusTested') }
  if (!conn.baseUrl.trim()) return { status: 'warning', label: t('connections.statusIncomplete') }
  if (!isConnectionReady(conn, def.requiresApiKey))
    return { status: 'warning', label: t('connections.statusMissingKey') }
  return { status: 'success', label: t('connections.statusConfigured') }
}

/*
 * 连接的**状态面**：余额（币 / 法币）与队列（并发 / 排队）。
 *
 * 只对 RunningHub 发这两个请求：面的实现只有它一家（`src-tauri/src/v2/ports/provider.rs`
 * 的默认实现返回 `STATUS_NOT_SUPPORTED`），对 ComfyUI / OpenAI 图像发过去只会拿回一个错误，
 * 而那个错误会被显示成「余额不可读」——一句关于用户的假话。
 */
const hasFacets = (conn: ConnItem) => conn.vendor === 'runninghub' && conn.protocol === 'runninghub'

/**
 * 什么时候去问平台：**这条连接真的能认证时**。
 *
 * 没填密钥、已停用、测试失败时去问，拿回的必然是一个失败——而那不是故障，是"还没配好"
 * （同一个意思徽章已经说过了）。把它显示成「余额不可读」等于把配置状态说成运行时故障。
 */
const shouldLoadFacets = (conn: ConnItem) =>
  hasFacets(conn) &&
  !conn.checking &&
  conn.status === 'active' &&
  conn.healthOk !== false &&
  isConnectionReady(conn, true)

/** 行内的面：`余额 12,345 币 · CNY 2.160 · 并发 1/1 · 排队 0`。没有面就是空串，不占位。 */
const facetLine = (conn: ConnItem): string => {
  const facets = facetsStore.facetsFor(conn.connectionId)
  const balance = formatAccountFacet(facets.account)
  const queue = formatQueueFacet(facets.queue)
  return [
    balance
      ? t('connections.facetBalance', { value: balance })
      : facets.accountError
        ? t('connections.facetBalanceUnavailable')
        : '',
    queue ?? (facets.queueError ? t('connections.facetQueueUnavailable') : '')
  ]
    .filter(Boolean)
    .join(' · ')
}

/** 面查不到时的原因：挂在行内的 `title` 上（跟健康检查失败原因同一个姿势）。 */
const facetError = (conn: ConnItem): string | undefined => {
  const facets = facetsStore.facetsFor(conn.connectionId)
  return facets.accountError ?? facets.queueError ?? undefined
}

/** 逐条更新（跟健康检查同一个姿势）：慢的那条不拖累别的，失败的也只影响自己。 */
const refreshFacets = async () => {
  const ids = connections.value.filter(shouldLoadFacets).map((conn) => conn.connectionId)
  // 先把不再够格的清掉，再拉够格的：一条刚被停用（或 Key 被删）的连接，不该继续
  // 显示上一次那个账号的余额——那个数已经不是它的事实了。
  facetsStore.keepOnly(ids)
  await facetsStore.refreshMany(ids)
}

const drawerOpen = ref(false)
const drawerMode = ref<'create' | 'edit'>('create')
const drawerType = ref<TypeKey>('runninghub')
const editingId = ref<number | null>(null)
const saving = ref(false)
const formError = ref('')
const testingId = ref<number | null>(null)
const form = reactive({
  displayName: '',
  baseUrl: '',
  rhEndpoint: 'cn' as 'cn' | 'ai' | 'custom',
  comfyPresetId: 'local' as string,
  apiKey: '',
  defaultModel: '',
  /**
   * 这条连接**声明**能跑哪些模型（`config.models`）。
   *
   * 它是给人方便的过滤器、不是闸门：跑表单的 model 字段据此变成下拉，省得手打模型名
   * （打错了要到提交时才知道）。真发了清单外的模型，由平台去拒。
   */
  models: [] as string[],
  /**
   * ComfyUI 跑在 RunningHub 云端时：**密钥从哪来**。
   *
   * `null` = 这条连接自己填 Key（老路，两条路并存）；数字 = 引用那条 RunningHub 连接的密钥
   * （写进 config.sourceConnectionId）。引用的是**绑定**不是密钥——daemon 在执行前把被引用
   * 连接的密钥槽位装到这条连接上，所以轮换自动生效、被引用的连接删了就立刻报错。
   */
  sourceConnectionId: null as number | null,
  /** ComfyUI：是否允许连局域网地址（显卡机在另一台机器上）。默认关，见 style 之外的 safety.rs。 */
  allowPrivateNetwork: false,
  /**
   * 可选运行参数（**留空 = 用默认值**，见 src-tauri 的 `domain/connection_options.rs`）。
   *
   * 为什么这两个量属于连接：它们不是厂商协议的一部分，而是**用户环境**的一部分——
   * 一条走代理的远端 ComfyUI 有多慢、一个视频工作流要跑多久，只有用户知道。
   * 存的是**字符串**（输入框的形态），保存时转成数字；空/非法 ⇒ **不写这个键**
   * （缺席 = 用默认值，与 `allowPrivateNetwork` 同一个约定）。
   */
  requestTimeoutSeconds: '',
  trackingDeadlineSeconds: ''
})
/**
 * API Key 输入框的"显示/隐藏"。
 *
 * 只对**正在输入、还没提交**的那个值生效——不涉及任何回显（已保存的密钥永不出凭据库，
 * 界面能看到的只有它的指纹，见连接详情的 `secretHint`）。
 * 痛点是真的：粘完一串密钥看不清有没有粘错，只能盲存。
 */
const showApiKey = ref(false)

/**
 * 模型发现：这条连接背后平台声明的模型清单（点「拉取可用模型」才有）。
 *
 * 与 `form.models`（**用户勾选的**声明）刻意分开：发现是"平台有什么"，声明是"我要用哪些"。
 * 混成一个字段的话，每次拉取都会把用户的勾选冲掉。
 */
const discoveredModels = ref<string[]>([])
const fetchingModels = ref(false)
const modelsError = ref('')

const fetchModels = async () => {
  if (editingId.value === null) {
    // 新建时还没有连接 id，拉不了清单——先保存，再从编辑态拉。
    modelsError.value = t('connections.fetchModelsNeedsSave')
    return
  }
  fetchingModels.value = true
  modelsError.value = ''
  try {
    const result = await v2.listConnectionModels(editingId.value)
    discoveredModels.value = result.models
    if (!result.models.length) modelsError.value = t('connections.fetchModelsEmpty')
  } catch (error) {
    modelsError.value = error instanceof Error ? error.message : String(error)
  } finally {
    fetchingModels.value = false
  }
}

const toggleModel = (model: string) => {
  form.models = form.models.includes(model)
    ? form.models.filter((item) => item !== model)
    : [...form.models, model]
}

/**
 * 正在编辑的这条连接**已保存密钥的指纹**（尾 4 位 + 长度）。
 *
 * 密钥的值永远不会到这里——daemon 只回派生信息（`SecretHint`），界面也只需要这么多：
 * 用户要确认的是"我填的是哪把"，不是"密钥是什么"。
 */
const editingSecretHint = ref<SecretHint | null>(null)

/**
 * API Key 标签旁那枚指纹徽章。
 *
 * 两种情况分开说，因为它们对用户是两件事：
 * - 输入框空着 ⇒ 保存不会动密钥 ⇒「已保存 · 尾号 a1b2」；
 * - 用户正在输入新的 ⇒ 保存会**替换**它 ⇒ 说清将被换掉的是哪一把（否则"已保存"会误导：
 *   用户刚粘了一串，标签却说"已保存"，看起来像没生效）。
 *
 * 没保存过密钥时返回 null（不显示徽章），而不是显示"尾号 undefined"。
 */
const savedKeyBadge = computed(() => {
  const hint = editingSecretHint.value
  if (drawerMode.value !== 'edit' || !hint?.saved || !hint.tail) return null
  const replacing = form.apiKey.trim().length > 0
  return {
    text: t(replacing ? 'connections.apiKeyWillReplace' : 'connections.apiKeySaved', {
      tail: hint.tail
    }),
    // 长度只进 title：认人靠尾号，长度是给"我这把是不是被截断了"这种怀疑用的。
    title: t('connections.apiKeySavedDetail', {
      tail: hint.tail,
      length: String(hint.length ?? hint.tail.length)
    })
  }
})

/** 当前抽屉编辑的连接类型定义，供表单帮助文案引用。 */
const currentDef = computed(() => TYPE_DEFS.value.find((item) => item.key === drawerType.value))

/**
 * 邀请码提示：**跟着抽屉里选的站点走**（两个码不能混——拿 .cn 的码去 .ai 注册没有奖励）。
 *
 * 位置是刻意的：用户此刻正被要求"去拿一个 API Key"，而**新人这一刻才刚要注册**。
 * 文案里三件事都写明了：双方各得什么、**填谁的码他都拿这么多**（所以这不是零和）、
 * 以及"不填不影响任何功能 + 规则以平台页面为准"。
 */
const runningHubInvite = computed(() => {
  const site: 'cn' | 'ai' = form.rhEndpoint === 'ai' ? 'ai' : 'cn'
  const invite = RUNNINGHUB_INVITES[site]
  return t('provider.runningHub.inviteHint', {
    code: invite.code,
    coins: String(invite.coins),
    site: t(site === 'ai' ? 'connections.inviteSiteAi' : 'connections.inviteSiteCn')
  })
})

/**
 * 站点一换，默认显示名跟着换（国内 ↔ 国际）。
 *
 * 只在名字**还是预设的默认名**时才动它：用户自己写过的名字不该被我们改掉，
 * 而"两个站点两行同名"正是这条要防的事。
 */
watch(
  () => form.rhEndpoint,
  (value) => {
    const preset = RUNNINGHUB_ENDPOINTS.value.find((item) => item.value === value)
    if (!preset) return
    const untouchedDefault = RUNNINGHUB_ENDPOINTS.value.some(
      (item) => item.defaultName === form.displayName
    )
    if (untouchedDefault) form.displayName = preset.defaultName
  }
)

/**
 * API Key 字段的标签。
 *
 * ⚠️ 字段**总是显示**（rh-proxy 与带鉴权的远程地址要用它），但驱动声明
 * `requiresApiKey: false` 时必须说清"可选"——否则本地 ComfyUI 的用户会以为
 * "连本地也要 key？"（实测问过）。判据来自驱动的声明，不在这里另写一份类型表。
 */
const apiKeyLabel = computed(() => {
  if (drawerMode.value === 'edit') return t('connections.apiKeyEditHint')
  // ComfyUI 跑在 RunningHub 云端（地址带 /proxy）时，用的**就是 RunningHub 那把 Key**——
  // 标签必须说清是"哪一把"，否则用户会照"通常留空"的提示跳过它，然后在运行时 401。
  if (drawerType.value === 'comfyui' && runningHubProxySelected.value) {
    return t('connections.apiKeyRunningHub')
  }
  return currentDef.value?.requiresApiKey === false
    ? t('connections.apiKeyOptional')
    : t('connections.apiKey')
})

/**
 * 当前 ComfyUI 连接是不是跑在 RunningHub 云端。
 *
 * 判据取**地址**（`/proxy`）而不是预设名字：名字可以改、可以被用户自定义，
 * 而"要带 RunningHub 的 Key"这件事是由地址决定的。
 *
 * ⚠️ 地址要跟**真实请求**取同一个来源：预设选中时用的是预设对象的 `baseUrl`，
 * `form.baseUrl` 只在自定义那条路上才是权威——读错了会出现"切了预设但提示不变"
 * （第一次就是这么错的，浏览器里当场看出来的）。
 */
const runningHubProxySelected = computed(() => {
  if (drawerType.value !== 'comfyui') return false
  const selected = allComfyPresets.value.find((item) => item.id === form.comfyPresetId)
  const baseUrl = (selected?.baseUrl ?? form.baseUrl).trim().replace(/\/+$/, '')
  return baseUrl.endsWith('/proxy')
})

/** ComfyUI 抽屉里那行帮助文案：RH 云端要的是 RunningHub 的 Key，别的 ComfyUI 才"通常留空"。 */
const comfyApiKeyHint = computed(() =>
  runningHubProxySelected.value
    ? t('provider.comfyui.apiKeyHintRunningHub')
    : t('provider.comfyui.apiKeyHint')
)

/**
 * 可以当密钥来源的连接：**只有 RunningHub**。
 *
 * 这是 daemon 侧的硬规则（`credential_reference`），界面这里只是照着它给选项——
 * 两边都写是因为界面能提前把不可能的选项去掉，而 daemon 不能相信界面。
 */
const runningHubConnections = computed(() =>
  connections.value.filter((conn) => conn.vendor === 'runninghub')
)

/**
 * 这个站**还没配过连接**吗？——只有这种时候才提邀请码。
 *
 * 判据是"他此刻需不需要它"，不是"这行字好不好看"：新人在这里才知道要去注册，提示是帮助；
 * 而**已经配好的人**（回来换 Key、改名字）看到「还没有账号？」就是噪音——同一行字，常驻
 * 就变成了广告。所以它跟着**当前选的站点**判，不跟着"有没有任意一条 RunningHub 连接"判：
 * .cn 与 .ai 是两套独立账号，配了国内站之后第一次配国际站，注册提示对他仍然有用。
 *
 * 位置刻意放在 `runningHubConnections` 之后：这段读它，别让读者去猜顺序。
 */
const runningHubSiteUnconfigured = computed(() => {
  const host = form.rhEndpoint === 'ai' ? 'runninghub.ai' : 'runninghub.cn'
  return !runningHubConnections.value.some((conn) => (conn.baseUrl || '').includes(host))
})

/**
 * 当前是不是"借别人的 Key"。
 *
 * 只有 ComfyUI + RH 云端这个组合才有意义：别的地址（本地/局域网/自建）根本不需要
 * RunningHub 的 Key，给它们引用等于把一把平台密钥当 Bearer 发到别的服务器上。
 */
const usingCredentialReference = computed(
  () => runningHubProxySelected.value && form.sourceConnectionId !== null
)

/** 下拉的取值：连接 id 用字符串（Select 的 value 就是字符串），"自己填" 用 `own`。 */
const credentialSourceValue = computed({
  get: () => (form.sourceConnectionId === null ? 'own' : String(form.sourceConnectionId)),
  set: (value: string) => {
    form.sourceConnectionId = value === 'own' ? null : Number(value)
    // 切回"自己填"时把已输入的密钥留着不动；切到引用时清掉它——否则保存会把一把
    // 没人再看的密钥写进凭据库（而它永远不会被用到，正是"同一份密钥存两份"的老毛病）。
    if (form.sourceConnectionId !== null) form.apiKey = ''
  }
})

/**
 * 引用目标的状态。**界面自己能判出来**（连接列表里就有 vendor 与 status），
 * 所以不必让 daemon 在详情里多回一个字段——少一个字段就少一处会不一致的地方。
 */
const credentialSourceState = computed(() => {
  if (!runningHubProxySelected.value) return 'n/a' as const
  if (form.sourceConnectionId === null) return 'own' as const
  const target = runningHubConnections.value.find(
    (conn) => conn.connectionId === form.sourceConnectionId
  )
  if (!target) return 'missing' as const
  if (target.status !== 'active') return 'disabled' as const
  return 'ok' as const
})

/**
 * 编辑抽屉里凭证那一行的余额（有才显示）。
 *
 * 挂在这里而不是挂在"连接"上：余额属于**账户**，账户由那把 Key 决定。凭证行回答的问题
 * 本来就是"我填的是哪把"（尾号徽章），"这把还能不能跑"是同一族的第二个问题——
 * 而原来它被放在侧栏那张「连接」卡里，只对 RunningHub 成立，还写成了"钱包"。
 */
const editingBalance = computed(() => {
  if (drawerType.value !== 'runninghub' || editingId.value === null) return ''
  const balance = formatAccountFacet(facetsStore.facetsFor(editingId.value).account)
  return balance ? t('connections.facetBalance', { value: balance }) : ''
})

/**
 * 借来的那把 Key 属于哪个账户、还剩多少。
 *
 * 数字**取自被引用连接已经查到的面**，不再单独查一次：引用的是同一把 Key，也就是同一个
 * 账户；各查一次会得到两个可能不一致的数——正是"同一份密钥存两份"那个老毛病。
 */
const sourceBalance = computed(() => {
  if (form.sourceConnectionId === null) return ''
  const balance = formatAccountFacet(facetsStore.facetsFor(form.sourceConnectionId).account)
  return balance ? t('connections.facetBalance', { value: balance }) : ''
})

/**
 * 切到 RH 云端时默认选中"唯一的那条 RunningHub 连接"；切走时清掉引用。
 *
 * 清掉是必须的：引用只对 RH 云端有意义，留在一条本地 ComfyUI 的配置里会让它
 * 拿着 RunningHub 的 Key 去请求本机（daemon 那边照借不误）。
 */
watch(runningHubProxySelected, (isCloud) => {
  if (!isCloud) {
    form.sourceConnectionId = null
    return
  }
  if (form.sourceConnectionId === null && runningHubConnections.value.length === 1) {
    form.sourceConnectionId = runningHubConnections.value[0].connectionId
  }
})

/**
 * 连接的**平台**那一维：协议由连接类型回答（这条是 ComfyUI），平台由**地址**回答。
 *
 * 分开说是有原因的：看到「ComfyUI 连接 + `runninghub.cn/proxy`」，用户的直觉必然是
 * "这不就是 RunningHub 吗"。说清"协议 ComfyUI ／ 平台 RunningHub 云端"，两个问题就分开了——
 * 跟应用库那条一致（`kind` 回答"这是什么"，连接名回答"跑在哪"）。
 *
 * 判据取地址不取预设名字：名字可以改，地址才决定它在跟谁说话。
 */
const comfyPlatformLabel = computed(() => {
  const selected = allComfyPresets.value.find((item) => item.id === form.comfyPresetId)
  const baseUrl = (selected?.baseUrl ?? form.baseUrl).trim()
  if (baseUrl.endsWith('/proxy') && baseUrl.includes('runninghub')) {
    return t('connections.platformRunningHubCloud')
  }
  const host = baseUrl.replace(/^https?:\/\//, '').split(/[/:]/)[0] ?? ''
  if (host === '127.0.0.1' || host === 'localhost') return t('connections.platformLocal')
  if (/^(10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.)/.test(host)) {
    return t('connections.platformLan')
  }
  return t('connections.platformRemote')
})

/**
 * config 里存的正整数（运行参数）→ 输入框的字符串。
 *
 * 两种形态都认：daemon 写回的是数字，而历史上表单传过字符串。空/非法/非正数一律回空串
 * ——输入框空着 = "用默认值"，比显示一个 0 或 `NaN` 诚实。
 */
const positiveIntFromConfig = (value: unknown): string => {
  const parsed = typeof value === 'number' ? value : Number.parseInt(String(value ?? ''), 10)
  return Number.isFinite(parsed) && parsed > 0 ? String(parsed) : ''
}

const openCreate = (key: TypeKey) => {
  const def = TYPE_DEFS.value.find((item) => item.key === key)!
  drawerMode.value = 'create'
  drawerType.value = key
  editingId.value = null
  form.displayName = def.label
  form.baseUrl = def.defaultBaseUrl
  form.rhEndpoint = def.key === 'runninghub' ? 'cn' : 'custom'
  form.comfyPresetId = def.key === 'comfyui' ? 'local' : 'local'
  if (def.key === 'runninghub') {
    // 两个站点默认同名的话，选择器里就是两行一模一样的「RunningHub」——见预设表上的说明。
    form.displayName = RUNNINGHUB_ENDPOINTS.value[0].defaultName
  }
  form.apiKey = ''
  form.defaultModel = ''
  form.models = []
  form.sourceConnectionId = null
  form.allowPrivateNetwork = false
  form.requestTimeoutSeconds = ''
  form.trackingDeadlineSeconds = ''
  editingSecretHint.value = null
  discoveredModels.value = []
  modelsError.value = ''
  formError.value = ''
  drawerOpen.value = true
}

const openEdit = async (def: ConnectionTypeDef, conn: ConnItem) => {
  drawerMode.value = 'edit'
  drawerType.value = def.key
  editingId.value = conn.connectionId
  form.displayName = conn.displayName || def.label
  form.baseUrl = conn.baseUrl || def.defaultBaseUrl
  form.apiKey = ''
  form.defaultModel = conn.defaultModel ?? ''
  form.models = []
  form.sourceConnectionId = null
  form.allowPrivateNetwork = false
  form.requestTimeoutSeconds = ''
  form.trackingDeadlineSeconds = ''
  editingSecretHint.value = null
  discoveredModels.value = []
  modelsError.value = ''
  formError.value = ''
  try {
    const detail = await v2.getConnection(conn.connectionId)
    const config = detail.config
    form.baseUrl = typeof config.baseUrl === 'string' ? config.baseUrl : form.baseUrl
    form.defaultModel = typeof config.defaultModel === 'string' ? config.defaultModel : ''
    form.allowPrivateNetwork = config.allowPrivateNetwork === true
    form.requestTimeoutSeconds = positiveIntFromConfig(config.requestTimeoutSeconds)
    form.trackingDeadlineSeconds = positiveIntFromConfig(config.trackingDeadlineSeconds)
    form.sourceConnectionId =
      typeof config.sourceConnectionId === 'number' ? config.sourceConnectionId : null
    form.models = Array.isArray(config.models)
      ? config.models.filter((model): model is string => typeof model === 'string')
      : []
    editingSecretHint.value = detail.secretHint ?? null
  } catch (error) {
    formError.value = error instanceof Error ? error.message : String(error)
    return
  }
  const preset = RUNNINGHUB_ENDPOINTS.value.find((item) => item.baseUrl === form.baseUrl)
  form.rhEndpoint = preset ? preset.value : 'custom'
  if (def.key === 'comfyui') {
    // 按**模式**判，不按 id 写死：rh-proxy 现在有国内/国际两条预设。
    const local = BUILTIN_COMFYUI_PRESETS.value.find(
      (item) => item.mode !== 'rh-proxy' && item.baseUrl === form.baseUrl
    )
    const rh = BUILTIN_COMFYUI_PRESETS.value.find(
      (item) => item.mode === 'rh-proxy' && form.baseUrl.startsWith(item.baseUrl)
    )
    const user = userComfyPresets.value.find((item) => item.baseUrl === form.baseUrl)
    form.comfyPresetId = rh ? rh.id : local ? local.id : user ? user.id : 'custom'
  }
  drawerOpen.value = true
}

const resolveRunningHubConfig = (): Record<string, string> => {
  if (form.rhEndpoint === 'custom') {
    return { baseUrl: form.baseUrl.trim(), region: 'custom' }
  }
  const preset = RUNNINGHUB_ENDPOINTS.value.find((item) => item.value === form.rhEndpoint)!
  return { baseUrl: preset.baseUrl, region: preset.region }
}

const saveConnection = async () => {
  const def = TYPE_DEFS.value.find((item) => item.key === drawerType.value)!
  saving.value = true
  formError.value = ''
  try {
    let config: Record<string, unknown>
    if (def.key === 'runninghub') {
      config = resolveRunningHubConfig()
    } else if (def.key === 'comfyui') {
      const selected = allComfyPresets.value.find((item) => item.id === form.comfyPresetId)
      if (selected) {
        config = { baseUrl: selected.baseUrl, mode: selected.mode ?? 'standard' }
      } else {
        if (!form.baseUrl.trim()) throw new Error(t('connections.baseUrlRequired'))
        config = { baseUrl: form.baseUrl.trim(), mode: 'standard' }
      }
      // 显式开关：daemon 侧的 SSRF 守卫默认拒绝局域网地址，只有这里勾了才放行。
      // 不勾就写 undefined —— 让"没开"在配置里是缺席而不是 false，语义更干净。
      config.allowPrivateNetwork = form.allowPrivateNetwork || undefined
      // 凭据引用：只在 RH 云端这条路写；不引用时**不写这个键**（缺席 = 走自己的密钥槽，
      // 老连接因此一行都不用改）。写 undefined 会被 JSON 丢掉，与"缺席"同义。
      config.sourceConnectionId =
        runningHubProxySelected.value && form.sourceConnectionId !== null
          ? form.sourceConnectionId
          : undefined
    } else {
      if (!form.baseUrl.trim()) throw new Error(t('connections.baseUrlRequired'))
      config = { baseUrl: form.baseUrl.trim() }
    }
    if (def.key === 'openaiImages') {
      config.defaultModel = form.defaultModel.trim() || undefined
      // 声明的模型清单：空数组等于"没声明"⇒ 不写这个键（缺席比空数组干净，
      // 而且 schema 里没有它就代表"不过滤"）。
      config.models = form.models.length ? [...form.models] : undefined
    }
    /*
     * 可选运行参数：**留空就不写这个键**（缺席 = 用 daemon 的默认值，与
     * `allowPrivateNetwork` 同一个约定）。非法值也当没填——daemon 那边还会再夹一次范围，
     * 但让 0/NaN 走到那一步没有意义（它只会变成"用默认值"，不如在这里就说清楚）。
     */
    const requestTimeout = Number.parseInt(form.requestTimeoutSeconds, 10)
    config.requestTimeoutSeconds =
      Number.isFinite(requestTimeout) && requestTimeout > 0 ? requestTimeout : undefined
    const trackingDeadline = Number.parseInt(form.trackingDeadlineSeconds, 10)
    config.trackingDeadlineSeconds =
      Number.isFinite(trackingDeadline) && trackingDeadline > 0 ? trackingDeadline : undefined
    const input = {
      displayName: form.displayName.trim() || def.label,
      config,
      capabilities: def.capabilities,
      lifecycleFeatures: def.lifecycleFeatures
    }
    const secretBindings = form.apiKey.trim()
      ? [{ slot: 'api_key', resolution: 'current' as const, value: form.apiKey.trim() }]
      : undefined
    if (drawerMode.value === 'edit' && editingId.value !== null) {
      await v2.updateConnection(
        editingId.value,
        secretBindings ? { ...input, secretBindings } : input
      )
    } else {
      await v2.createConnection({
        // OpenAI 图像是同一家（openai）的另一个协议，所以 vendor 要归一到 `openai`。
        vendor: def.key === 'openaiImages' ? 'openai' : def.key,
        protocol: def.key === 'openaiImages' ? 'openai-images' : def.key,
        ...input,
        secretBindings
      })
    }
    await loadConnections()
    invalidateExecutionConnections()
    void loadExecutionConnections(true).catch(() => undefined)
    drawerOpen.value = false
    appStore.showNotification('success', t('connections.saved', { label: def.label }))
  } catch (error) {
    formError.value = error instanceof Error ? error.message : String(error)
  } finally {
    saving.value = false
  }
}

const testConnection = async (conn: ConnItem) => {
  testingId.value = conn.connectionId
  try {
    const result = await v2.checkConnection(conn.connectionId)
    // 结果落在行内（徽章 + 原因），别只靠那条 3 秒的提示条：提示条飘在右下角、还会被节流的帧吃掉。
    conn.healthOk = result.ok
    conn.testedOk = result.ok
    conn.healthMessage = result.ok ? undefined : (result.message ?? undefined)
    appStore.showNotification(
      result.ok ? 'success' : 'error',
      result.ok
        ? t('connections.testOk', { name: conn.displayName })
        : (result.message ?? t('connections.testFailed'))
    )
    // 测试通过后刷新共享连接缓存，让左下角状态同步变为正常。
    if (result.ok) {
      invalidateExecutionConnections()
      void loadExecutionConnections(true).catch(() => undefined)
      // 刚测通 ⇒ 这条连接现在能认证了，状态面可以问了（失败时不问，见 shouldLoadFacets）。
      void refreshFacets()
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error)
    conn.healthOk = false
    conn.testedOk = false
    conn.healthMessage = message
    appStore.showNotification('error', message)
  } finally {
    testingId.value = null
  }
}

const deleteConnection = async (conn: ConnItem) => {
  if (!window.confirm(t('connections.confirmDelete', { name: conn.displayName }))) return
  try {
    await v2.deleteConnection(conn.connectionId)
    // 先清掉这条连接的面：不清的话，`loadConnections` 跑完之前那一小段时间里，
    // 界面上还留着一个已经不存在的账号的余额。
    facetsStore.forget(conn.connectionId)
    await loadConnections()
    invalidateExecutionConnections()
    void loadExecutionConnections(true).catch(() => undefined)
    appStore.showNotification('success', t('connections.deleted'))
  } catch (error) {
    appStore.showNotification('error', error instanceof Error ? error.message : String(error))
  }
}

/**
 * 可选参数的默认值与范围（高级项的提示文字用）：跟着连接一起拉。
 *
 * 拿不到就退回兜底值（`optionDefaults` 为空时那三个 computed 会给），**不打扰用户**：
 * 这一条失败不影响任何功能，只影响输入框里的提示数字。
 */
const loadOptionDefaults = async () => {
  try {
    optionDefaults.value = (await v2.get<DiagnosticInfo>('/api/v1/diagnostics')).defaults
  } catch {
    optionDefaults.value = undefined
  }
}

onMounted(() => {
  void loadConnections()
  void loadOptionDefaults()
})
</script>

<template>
  <PageContainer class="max-w-5xl">
    <PageHeader
      :eyebrow="t('connections.eyebrow')"
      :title="t('connections.title')"
      :description="t('connections.description')"
    />

    <div v-if="loadError" class="mt-6">
      <p class="text-sm text-destructive">
        {{ t('connections.loadFailed', { message: loadError }) }}
      </p>
    </div>

    <div v-if="!loading" class="mt-6 space-y-8">
      <Card v-if="!anyConfigured" class="border-info/30 bg-info/5 p-5">
        <p class="text-sm font-medium">{{ t('connections.step1Title') }}</p>
        <p class="mt-1 text-sm text-muted-foreground">
          {{ t('connections.step1Body') }}
        </p>
        <div class="mt-3">
          <Button @click="openCreate('runninghub')">{{ t('connections.addRhConnection') }}</Button>
        </div>
      </Card>

      <section>
        <div class="mb-3">
          <h2 class="text-sm font-medium text-muted-foreground">
            {{ t('connections.groupGeneration') }}
          </h2>
          <p class="mt-0.5 text-xs text-muted-foreground">
            {{ t('connections.groupGenerationHint') }}
          </p>
        </div>
        <div class="space-y-4">
          <Card v-for="def in TYPE_DEFS" :key="def.key" class="p-4">
            <div class="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
              <div class="flex min-w-0 gap-3">
                <div
                  class="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-secondary text-sm font-medium"
                >
                  {{ def.short }}
                </div>
                <div class="min-w-0">
                  <h3 class="text-sm font-semibold">{{ def.label }}</h3>
                  <p class="mt-0.5 text-xs leading-5 text-muted-foreground">{{ def.scope }}</p>
                </div>
              </div>
              <Button
                variant="outline"
                size="sm"
                class="shrink-0 self-start"
                @click="openCreate(def.key)"
              >
                {{ t('connections.addConnection') }}
              </Button>
            </div>
            <div class="mt-4 space-y-2">
              <div
                v-for="conn in listOf(def)"
                :key="conn.connectionId"
                class="flex flex-col gap-2 rounded-lg border border-border/70 bg-secondary/40 px-3 py-2.5 sm:flex-row sm:items-center"
              >
                <div class="min-w-0 flex-1">
                  <div class="flex items-center gap-2">
                    <p class="truncate text-sm font-medium">{{ conn.displayName || def.label }}</p>
                    <StatusBadge
                      :status="statusOf(conn, def).status"
                      :label="statusOf(conn, def).label"
                    />
                  </div>
                  <p
                    v-if="conn.pausedReason"
                    class="mt-0.5 truncate text-xs text-amber-700 dark:text-amber-400"
                    :title="conn.pausedReason"
                  >
                    {{ t('connections.statusPausedHint', { reason: conn.pausedReason }) }}
                  </p>
                  <p class="mt-0.5 truncate text-xs text-muted-foreground">
                    {{ conn.baseUrl ? endpointLabel(conn) : def.description }}
                  </p>
                  <!--
                    失败原因就贴在行内（而不是只在提示条里闪 3 秒）：徽章说"失败了"，
                    这一行说"为什么"——缺 key、平台回了什么、还是根本连不上（含出网代理）。
                  -->
                  <p
                    v-if="conn.healthOk === false && conn.healthMessage"
                    class="mt-1 line-clamp-2 text-xs leading-5 text-destructive"
                    :title="conn.healthMessage"
                  >
                    {{ conn.healthMessage }}
                  </p>
                  <!--
                    状态面就贴在这一行（`余额 … · 并发 …`）：它跟上面的徽章回答的是同一件事
                    ——"这条连接现在能不能跑"。平台不提供面的连接（ComfyUI / OpenAI 图像）
                    这里什么都不显示，而不是显示一个错误：那是常态，不是故障。
                  -->
                  <p
                    v-if="facetLine(conn)"
                    class="mt-1 text-xs leading-5 tabular-nums text-muted-foreground"
                    :title="facetError(conn)"
                  >
                    {{ facetLine(conn) }}
                  </p>
                </div>
                <div class="flex shrink-0 items-center gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    :disabled="testingId !== null"
                    @click="testConnection(conn)"
                  >
                    {{
                      testingId === conn.connectionId
                        ? t('connections.testing')
                        : t('connections.test')
                    }}
                  </Button>
                  <Button variant="outline" size="sm" @click="openEdit(def, conn)">{{
                    t('connections.edit')
                  }}</Button>
                  <Button variant="ghost" size="sm" @click="deleteConnection(conn)">{{
                    t('connections.delete')
                  }}</Button>
                </div>
              </div>
              <p v-if="listOf(def).length === 0" class="py-1 text-xs text-muted-foreground">
                {{ t('connections.emptyHint', { label: def.label }) }}
              </p>
            </div>
          </Card>
        </div>
      </section>
    </div>

    <Dialog
      v-model:open="drawerOpen"
      :title="
        drawerMode === 'create'
          ? t('connections.drawerCreate', { label: currentDef?.label ?? '' })
          : t('connections.drawerEdit', { label: currentDef?.label ?? '' })
      "
      class="max-h-[calc(100dvh-2rem)] overflow-y-auto sm:max-w-lg"
      body-class="p-0"
    >
      <template #default>
        <div class="space-y-4 p-5">
          <p
            v-if="currentDef?.scope"
            class="rounded-md bg-info/5 px-3 py-2 text-xs leading-5 text-muted-foreground"
          >
            {{ currentDef.scope }}
          </p>
          <div class="space-y-3">
            <div>
              <label
                for="connection-display-name"
                class="mb-1 block text-xs text-muted-foreground"
                >{{ t('connections.displayName') }}</label
              >
              <Input
                id="connection-display-name"
                v-model.trim="form.displayName"
                :placeholder="t('connections.displayNamePlaceholder')"
              />
            </div>

            <template v-if="drawerType === 'runninghub'">
              <div>
                <label
                  for="connection-runninghub-endpoint"
                  class="mb-1 block text-xs text-muted-foreground"
                  >{{ t('connections.platform') }}</label
                >
                <Select id="connection-runninghub-endpoint" v-model="form.rhEndpoint">
                  <option
                    v-for="item in RUNNINGHUB_ENDPOINTS"
                    :key="item.value"
                    :value="item.value"
                  >
                    {{ item.label }}
                  </option>
                  <option value="custom">{{ t('connections.customAddress') }}</option>
                </Select>
              </div>
              <div v-if="form.rhEndpoint === 'custom'">
                <label
                  for="connection-runninghub-base-url"
                  class="mb-1 block text-xs text-muted-foreground"
                  >Base URL</label
                >
                <Input
                  id="connection-runninghub-base-url"
                  v-model.trim="form.baseUrl"
                  placeholder="https://www.runninghub.cn"
                />
                <p
                  v-if="currentDef?.baseUrlHint"
                  class="mt-1 text-[11px] leading-4 text-muted-foreground"
                >
                  {{ currentDef.baseUrlHint }}
                </p>
              </div>
            </template>

            <template v-else-if="drawerType === 'comfyui'">
              <div>
                <label
                  for="connection-comfyui-endpoint"
                  class="mb-1 block text-xs text-muted-foreground"
                  >{{ t('connections.execEnv') }}</label
                >
                <div class="flex items-center gap-2">
                  <Select
                    id="connection-comfyui-endpoint"
                    v-model="form.comfyPresetId"
                    class="flex-1"
                  >
                    <option v-for="item in allComfyPresets" :key="item.id" :value="item.id">
                      {{ item.name }}
                    </option>
                    <option value="custom">{{ t('connections.customAddress') }}</option>
                  </Select>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    class="shrink-0"
                    @click="openPresetManage()"
                    >{{ t('connections.manage') }}</Button
                  >
                </div>
                <p v-if="selectedPreset" class="mt-1.5 text-[11px] leading-5 text-muted-foreground">
                  <code>{{ selectedPreset.baseUrl }}</code>
                </p>
                <!--
                  把两个维度**分开说**：协议由连接类型回答（这条是 ComfyUI），平台由地址回答。
                  不分开说，用户看到「ComfyUI 连接 + runninghub.cn/proxy」必然问"这不就是 RunningHub 吗"。
                  判据取地址，不取预设名字（名字可改）。
                -->
                <p class="mt-1 text-[11px] leading-4 text-muted-foreground">
                  {{ t('connections.protocolLabel') }}：ComfyUI ／
                  {{ t('connections.platformLabel') }}：{{ comfyPlatformLabel }}
                </p>
              </div>
              <div v-if="!selectedPreset" class="mt-1.5">
                <label for="connection-base-url" class="mb-1 block text-xs text-muted-foreground"
                  >Base URL</label
                >
                <Input
                  id="connection-base-url"
                  v-model.trim="form.baseUrl"
                  placeholder="https://your-comfyui-host:8188"
                />
                <p
                  v-if="currentDef?.baseUrlHint"
                  class="mt-1 text-[11px] leading-4 text-muted-foreground"
                >
                  {{ currentDef.baseUrlHint }}
                </p>
              </div>
              <p
                v-if="selectedPreset?.mode === 'rh-proxy'"
                class="mt-1.5 text-[11px] leading-5 text-muted-foreground"
              >
                {{ t('connections.rhCloudNoteBefore') }}
                <code>{{ selectedPreset.baseUrl }}</code
                >{{ t('connections.rhCloudNoteAfter') }}
              </p>
              <label class="mt-3 flex cursor-pointer items-start gap-2">
                <input
                  v-model="form.allowPrivateNetwork"
                  type="checkbox"
                  class="mt-0.5 size-3.5 shrink-0 accent-[var(--primary)]"
                />
                <span class="text-xs leading-5">
                  {{ t('connections.allowPrivateNetwork') }}
                  <span class="mt-0.5 block text-[11px] leading-4 text-muted-foreground">{{
                    t('connections.allowPrivateNetworkHint')
                  }}</span>
                </span>
              </label>
            </template>

            <div v-else>
              <label for="connection-base-url" class="mb-1 block text-xs text-muted-foreground"
                >Base URL</label
              >
              <Input
                id="connection-base-url"
                v-model.trim="form.baseUrl"
                :placeholder="currentDef?.defaultBaseUrl"
              />
              <p
                v-if="currentDef?.baseUrlHint"
                class="mt-1 text-[11px] leading-4 text-muted-foreground"
              >
                {{ currentDef.baseUrlHint }}
              </p>
            </div>

            <!--
              凭据引用：RH 云端**不显示密钥字段**，改选"用哪条 RunningHub 连接的 Key"。
              这一步就是这次改动的全部意义——此前用户要回控制台把同一把 Key 再复制一遍。
            -->
            <div v-if="runningHubProxySelected">
              <div class="mb-1 flex flex-wrap items-center gap-x-2 gap-y-1">
                <label for="connection-credential-source" class="text-xs text-muted-foreground">{{
                  t('connections.credentialSource')
                }}</label>
                <!-- 指纹：借来的那把也要能认人（尾号来自 daemon 的有效解析，不是本连接的槽位）。 -->
                <span
                  v-if="savedKeyBadge"
                  class="rounded bg-secondary px-1.5 py-0.5 text-[10px] leading-4 text-muted-foreground"
                  :title="savedKeyBadge.title"
                  >{{ savedKeyBadge.text }}</span
                >
                <!-- 借来的那把属于哪个账户、还剩多少：数字取自被引用连接已查到的面（不重复查）。 -->
                <span
                  v-if="sourceBalance"
                  class="rounded bg-secondary px-1.5 py-0.5 text-[10px] leading-4 tabular-nums text-muted-foreground"
                  >{{ sourceBalance }}</span
                >
              </div>
              <Select
                id="connection-credential-source"
                v-model="credentialSourceValue"
                :disabled="!runningHubConnections.length"
              >
                <option
                  v-for="conn in runningHubConnections"
                  :key="conn.connectionId"
                  :value="String(conn.connectionId)"
                >
                  {{ conn.displayName }}（#{{ conn.connectionId }}）
                </option>
                <option value="own">{{ t('connections.credentialSourceOwn') }}</option>
              </Select>
              <p
                v-if="credentialSourceState === 'missing'"
                class="mt-1.5 text-[11px] leading-4 text-destructive"
              >
                {{ t('connections.credentialSourceMissing') }}
              </p>
              <p
                v-else-if="credentialSourceState === 'disabled'"
                class="mt-1.5 text-[11px] leading-4 text-destructive"
              >
                {{ t('connections.credentialSourceDisabled') }}
              </p>
              <p
                v-else-if="!runningHubConnections.length"
                class="mt-1.5 text-[11px] leading-4 text-destructive"
              >
                {{ t('connections.credentialSourceNone') }}
              </p>
              <p
                v-else-if="usingCredentialReference"
                class="mt-1.5 text-[11px] leading-4 text-muted-foreground"
              >
                {{ t('connections.credentialSourceHint') }}
              </p>
            </div>

            <div v-if="!usingCredentialReference">
              <!--
                徽章刻意**放在 `<label>` 外面**：label 的文字会成为输入框的可访问名，
                而"已保存 · 尾号 a1b2"是**当前状态**、不是字段名——放进去会让这个字段的名字
                随用户打字变来变去（读屏软件会跟着念），也让按名字找元素变得不稳定。
                视觉上仍与标签同一行（用户此刻正盯着这个框问"我填的是哪把"）。
              -->
              <div class="mb-1 flex flex-wrap items-center gap-x-2 gap-y-1">
                <label for="connection-api-key" class="text-xs text-muted-foreground">{{
                  apiKeyLabel
                }}</label>
                <span
                  v-if="savedKeyBadge"
                  class="rounded bg-secondary px-1.5 py-0.5 text-[10px] leading-4 text-muted-foreground"
                  :title="savedKeyBadge.title"
                  >{{ savedKeyBadge.text }}</span
                >
                <!--
                  余额就挂在凭证这一行：尾号答"我填的是哪把"，余额答"这把还能不能跑"。
                  这是它在界面上唯一的家（侧栏那两格已删）——它属于**账户**，不属于工作台。
                -->
                <span
                  v-if="editingBalance"
                  class="rounded bg-secondary px-1.5 py-0.5 text-[10px] leading-4 tabular-nums text-muted-foreground"
                  >{{ editingBalance }}</span
                >
              </div>
              <div class="flex items-center gap-2">
                <Input
                  id="connection-api-key"
                  v-model.trim="form.apiKey"
                  :type="showApiKey ? 'text' : 'password'"
                  autocomplete="off"
                  :placeholder="t('connections.apiKeyPlaceholder')"
                  class="flex-1"
                />
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  class="shrink-0"
                  @click="showApiKey = !showApiKey"
                  >{{
                    showApiKey ? t('connections.hideApiKey') : t('connections.showApiKey')
                  }}</Button
                >
              </div>
              <p
                v-if="drawerType === 'comfyui' || currentDef?.apiKeyHint"
                class="mt-1.5 text-[11px] leading-4 text-muted-foreground"
              >
                {{ drawerType === 'comfyui' ? comfyApiKeyHint : currentDef?.apiKeyHint }}
              </p>
              <p class="mt-1.5 text-[11px] leading-4 text-muted-foreground">
                {{ t('connections.apiKeyStorageNote') }}
              </p>
              <!-- 邀请码：只给 RunningHub、只在**这个站还没配过**的时候（见 runningHubSiteUnconfigured）。 -->
              <p
                v-if="drawerType === 'runninghub' && runningHubSiteUnconfigured"
                class="mt-1.5 text-[11px] leading-4 text-muted-foreground"
              >
                {{ runningHubInvite }}
              </p>
            </div>
            <div v-if="drawerType === 'openaiImages'">
              <label
                for="connection-default-model"
                class="mb-1 block text-xs text-muted-foreground"
                >{{ t('connections.defaultModelOptional') }}</label
              >
              <!--
                `list` + `<datalist>`：**有建议、但不挡自由输入**。用下拉会挡住"清单里没有
                但我就是想用"的情况（网关的清单未必全），而自由输入又正是"手打模型名"的痛点。
                datalist 两者都要。
              -->
              <Input
                id="connection-default-model"
                v-model.trim="form.defaultModel"
                list="connection-model-options"
                placeholder="gpt-image-1"
              />
              <datalist id="connection-model-options">
                <option v-for="model in form.models" :key="model" :value="model" />
              </datalist>
              <p class="mt-1.5 text-[11px] leading-4 text-muted-foreground">
                {{ t('connections.defaultModelHint') }}
              </p>

              <!-- 模型发现：平台声明了什么（拉取），以及我要用哪些（勾选 ⇒ config.models）。 -->
              <div class="mt-4 rounded-md border border-border p-3">
                <div class="flex flex-wrap items-center gap-2">
                  <span class="text-xs font-medium">{{ t('connections.models') }}</span>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    class="ml-auto shrink-0"
                    :disabled="fetchingModels"
                    @click="fetchModels"
                    >{{
                      fetchingModels
                        ? t('connections.fetchingModels')
                        : t('connections.fetchModels')
                    }}</Button
                  >
                </div>
                <p class="mt-1.5 text-[11px] leading-4 text-muted-foreground">
                  {{ t('connections.modelsHint') }}
                </p>
                <p v-if="modelsError" class="mt-1.5 text-[11px] leading-4 text-destructive">
                  {{ modelsError }}
                </p>
                <div
                  v-if="discoveredModels.length"
                  class="mt-2 max-h-44 space-y-1 overflow-y-auto pr-1"
                >
                  <label
                    v-for="model in discoveredModels"
                    :key="model"
                    class="flex cursor-pointer items-center gap-2 rounded px-1.5 py-1 text-xs hover:bg-secondary/60"
                  >
                    <input
                      type="checkbox"
                      class="size-3.5 shrink-0 accent-[var(--primary)]"
                      :checked="form.models.includes(model)"
                      @change="toggleModel(model)"
                    />
                    <span class="truncate">{{ model }}</span>
                  </label>
                </div>
                <p
                  v-else-if="form.models.length"
                  class="mt-1.5 text-[11px] leading-4 text-muted-foreground"
                >
                  {{ t('connections.modelsDeclared', { models: form.models.join('、') }) }}
                </p>
              </div>
            </div>
            <!--
              高级（可选）：只有**用户环境**才知道的两个量。
              它们不是厂商协议的一部分——一条走代理的远端 ComfyUI 有多慢、一个视频工作流要跑
              多久，只有用户知道；写死时慢的那条连接永远失败、长任务会被本地判失败，而用户
              没有任何办法（见 src-tauri/src/v2/domain/connection_options.rs 的文件头）。
              留空 = 用默认值，所以**不填与今天完全一样**。
            -->
            <div class="rounded-md border border-border p-3">
              <p class="text-xs font-medium">{{ t('connections.advanced') }}</p>
              <div class="mt-2 grid gap-3 sm:grid-cols-2">
                <div>
                  <label
                    for="connection-request-timeout"
                    class="mb-1 block text-xs text-muted-foreground"
                    >{{ t('connections.requestTimeout') }}</label
                  >
                  <Input
                    id="connection-request-timeout"
                    v-model.trim="form.requestTimeoutSeconds"
                    inputmode="numeric"
                    :placeholder="String(defaultRequestTimeout)"
                  />
                  <p class="mt-1.5 text-[11px] leading-4 text-muted-foreground">
                    {{
                      t('connections.requestTimeoutHint', {
                        default: defaultRequestTimeout,
                        range: requestTimeoutRange
                      })
                    }}
                  </p>
                </div>
                <!-- 同步出图的连接（OpenAI 图像）没有"跟踪多久"这回事，不摆一个永远不生效的框。 -->
                <div v-if="!currentDef?.lifecycleFeatures.synchronous_result">
                  <label
                    for="connection-tracking-deadline"
                    class="mb-1 block text-xs text-muted-foreground"
                    >{{ t('connections.trackingDeadline') }}</label
                  >
                  <Input
                    id="connection-tracking-deadline"
                    v-model.trim="form.trackingDeadlineSeconds"
                    inputmode="numeric"
                    :placeholder="String(defaultTrackingDeadline)"
                  />
                  <p class="mt-1.5 text-[11px] leading-4 text-muted-foreground">
                    {{
                      t('connections.trackingDeadlineHint', {
                        default: defaultTrackingDeadline,
                        range: trackingDeadlineRange
                      })
                    }}
                  </p>
                </div>
              </div>
            </div>
            <p v-if="formError" class="text-xs text-destructive">{{ formError }}</p>
          </div>
          <div class="flex justify-end gap-2 border-t border-border pt-4">
            <Button variant="outline" @click="drawerOpen = false">{{ t('common.cancel') }}</Button>
            <Button :disabled="saving" @click="saveConnection">
              {{ saving ? t('connections.saving') : t('connections.save') }}
            </Button>
          </div>
        </div>
      </template>
    </Dialog>

    <Dialog
      v-model:open="presetManageOpen"
      :title="t('connections.presetTitle')"
      class="sm:max-w-md"
    >
      <div class="space-y-3 p-4">
        <p class="text-xs text-muted-foreground">
          {{ t('connections.presetDescription') }}
        </p>
        <div class="space-y-1.5">
          <div
            v-for="item in userComfyPresets"
            :key="item.id"
            class="flex items-center gap-2 rounded-md bg-secondary/60 px-3 py-2 text-xs"
          >
            <div class="min-w-0 flex-1">
              <p class="truncate font-medium">{{ item.name }}</p>
              <p class="truncate text-muted-foreground">{{ item.baseUrl }}</p>
            </div>
            <Button variant="ghost" size="sm" @click="removePreset(item.id)">{{
              t('connections.delete')
            }}</Button>
          </div>
          <p v-if="!userComfyPresets.length" class="py-1 text-xs text-muted-foreground">
            {{ t('connections.noCustomPresets') }}
          </p>
        </div>
        <div class="space-y-1.5 border-t border-border pt-3">
          <Input v-model.trim="presetName" :placeholder="t('connections.presetNamePlaceholder')" />
          <Input v-model.trim="presetUrl" :placeholder="t('connections.presetUrlPlaceholder')" />
          <p v-if="presetError" class="text-xs text-destructive">{{ presetError }}</p>
          <Button size="sm" class="w-full" @click="addPreset">{{
            t('connections.addPreset')
          }}</Button>
        </div>
      </div>
      <template #footer>
        <Button variant="outline" @click="presetManageOpen = false">{{
          t('connections.done')
        }}</Button>
      </template>
    </Dialog>
  </PageContainer>
</template>
