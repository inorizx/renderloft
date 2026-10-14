import { computed, ref } from 'vue'
import { v2 } from '@/shared/api/v2'

export type ConnectionStatus = 'connected' | 'not-configured' | 'unknown'

export interface ProviderConnection {
  id: string
  providerId: string
  name: string
  account?: string
  config?: Record<string, string>
  /** 连接的基础地址（加载时从 config.baseUrl 拍平）。就绪判定读这个字段，不读嵌套 config。 */
  baseUrl: string
  apiKeyConfigured?: boolean
  detailsAvailable?: boolean
  healthOk?: boolean
  status?: string
  connectionId: number
  currentRevisionId: number
}

/**
 * 就绪判定需要的字段。
 *
 * ⚠️ `baseUrl` 必须是**扁平字段**，别改回读 `connection.config.baseUrl`：
 * 连接页（Connections.vue 的 ConnItem）和 workspace store 各自构造的视图模型都只拍平了
 * baseUrl，读嵌套 config 对它们恒为 undefined —— 于是判定恒 false，明明密钥配好了也
 * 一律显示「缺少密钥」。现在 baseUrl 是必填项，形状对不上会在 vue-tsc 阶段直接报错，
 * 而不是静默判成「未就绪」。
 */
export type ConnectionReadiness = Pick<
  ProviderConnection,
  'baseUrl' | 'apiKeyConfigured' | 'detailsAvailable' | 'healthOk' | 'status'
>

export const isConnectionReady = (connection: ConnectionReadiness, requiresApiKey: boolean) =>
  connection.detailsAvailable === true &&
  connection.status !== 'disabled' &&
  connection.status !== 'invalid' &&
  connection.healthOk !== false &&
  Boolean(connection.baseUrl?.trim()) &&
  (!requiresApiKey || connection.apiKeyConfigured === true)

const connections = ref<ProviderConnection[]>([])
const isLoading = ref(false)
const isLoaded = ref(false)
const loadError = ref('')
let loadingPromise: Promise<void> | undefined

export const invalidateExecutionConnections = () => {
  isLoaded.value = false
  loadError.value = ''
}

// providerId 已在加载时规范化为 vendor（openai-images → openai-image，协议名按实际），
// 这里只做精确匹配，不再兼容旧连接命名。
const providerMatches = (providerId: string, connection: ProviderConnection): boolean =>
  connection.providerId === providerId

export function useExecutionConnections() {
  const loadConnections = async (force = false) => {
    if (!force && isLoaded.value) return
    if (loadingPromise) return loadingPromise

    isLoading.value = true
    loadingPromise = v2
      .listConnections()
      .then(async (items) => {
        // 并行拉取详情，避免连接多时首屏串行变慢（N+1）。
        const details = await Promise.all(
          items.map(async (item) => {
            let config: Record<string, string> = {}
            let apiKeyConfigured = false
            let detailsAvailable = false
            let healthOk: boolean | undefined
            try {
              const detail = await v2.getConnection(item.connectionId)
              try {
                healthOk = (await v2.checkConnection(item.connectionId)).ok
              } catch {
                healthOk = false
              }
              config = Object.fromEntries(
                Object.entries(detail.config).map(([key, value]) => [key, String(value)])
              )
              apiKeyConfigured = detail.secretBindings.some(
                (binding) => binding.slot === 'api_key' || binding.slot.endsWith('-api_key')
              )
              detailsAvailable = true
            } catch {
              // 详情失败时保留基础信息
            }
            return { item, config, apiKeyConfigured, detailsAvailable, healthOk }
          })
        )
        const mapped: ProviderConnection[] = details.map(
          ({ item, config, apiKeyConfigured, detailsAvailable, healthOk }) => ({
            id: String(item.connectionId),
            providerId:
              item.vendor === 'openai' && item.protocol === 'openai-images'
                ? 'openai-image'
                : item.vendor,
            name: item.displayName,
            config,
            baseUrl: String(config.baseUrl ?? ''),
            apiKeyConfigured,
            detailsAvailable,
            healthOk,
            status: item.status,
            connectionId: item.connectionId,
            currentRevisionId: item.currentRevisionId
          })
        )
        connections.value = mapped
        isLoaded.value = true
        loadError.value = ''
      })
      .catch((error) => {
        loadError.value = error instanceof Error ? error.message : String(error)
        isLoaded.value = false
        throw error
      })
      .finally(() => {
        isLoading.value = false
        loadingPromise = undefined
      })

    return loadingPromise
  }

  const connectionFor = (providerId: string, preferredId?: string) => {
    const matches = connections.value.filter((connection) =>
      providerMatches(providerId, connection)
    )
    // preferredId 为数字连接 id 时精确匹配，否则回退该 provider 第一个连接。
    const numeric = preferredId && /^\d+$/.test(preferredId)
    if (numeric) {
      return matches.find((connection) => connection.id === preferredId) ?? matches[0]
    }
    return matches[0]
  }

  const connectionStatus = (providerId: string, preferredId?: string): ConnectionStatus => {
    if (providerId === 'runninghub') {
      // RunningHub 可用性取决于连接详情和凭据是否完整。
      if (!isLoaded.value || loadError.value) return 'unknown'
      const runningHubConnections = connections.value.filter(
        (connection) => connection.providerId === 'runninghub'
      )
      if (runningHubConnections.some((connection) => isConnectionReady(connection, true))) {
        return 'connected'
      }
      if (runningHubConnections.some((connection) => connection.detailsAvailable === false)) {
        return 'unknown'
      }
      return 'not-configured'
    }
    if (!isLoaded.value || loadError.value) return 'unknown'

    const matches = connections.value.filter((connection) =>
      providerMatches(providerId, connection)
    )
    if (matches.length === 0) return 'not-configured'
    // preferredId：传入数字连接 id 时只评估该特定连接；
    // 未指定时看该 provider 下是否存在任一可用连接，
    // 避免第一个未完成的条目把整体状态误判为 not-configured。
    const numeric = preferredId && /^\d+$/.test(preferredId)
    const preferred = numeric
      ? matches.find((connection) => connection.id === preferredId)
      : undefined

    const requiresApiKey = providerId === 'openai-image'
    const candidates = preferred ? [preferred] : matches
    if (candidates.some((connection) => isConnectionReady(connection, requiresApiKey))) {
      return 'connected'
    }
    if (candidates.some((connection) => connection.detailsAvailable === false)) {
      return 'unknown'
    }
    return 'not-configured'
  }

  const connectionName = (connectionId: string, fallback?: string) =>
    connections.value.find((connection) => connection.id === connectionId)?.name ??
    fallback ??
    connectionId

  const allConnections = () => [...connections.value]

  return {
    connections,
    isLoading,
    isLoaded,
    loadError,
    loadConnections,
    connectionFor,
    connectionName,
    connectionStatus,
    allConnections,
    runningHubStatus: computed(() => connectionStatus('runninghub')),
    comfyuiStatus: computed(() => connectionStatus('comfyui')),
    openAiImageStatus: computed(() => connectionStatus('openai-image'))
  }
}
