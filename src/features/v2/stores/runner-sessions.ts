import { ref } from 'vue'
import { defineStore } from 'pinia'
import { useV2WorkspaceStore } from './workspace'

export interface RunnerFileRef {
  assetId: number
  checksum: string
  filename: string
}

export interface RunnerSession {
  appId: string
  title: string
  values: Record<string, unknown>
  files: Record<string, RunnerFileRef>
  mode: 'single' | 'batch'
  batchCount: string
  /** 批量模式下勾选为“逐行填写”的字段 key。 */
  variableKeys?: string[]
  /** 批量模式的任务行草稿。 */
  batchRows?: RunnerBatchRow[]
}

export interface RunnerBatchRow {
  id: number
  values: Record<string, unknown>
  files: Record<string, RunnerFileRef>
  /**
   * 被用户**显式移除**的文件槽位（详见 `V2Runner.vue` 的 `BatchRow.clearedFiles`）。
   *
   * 必须跟着草稿一起存：否则关掉应用再打开，"这一行明确不要那张统一图片"会退化成
   * "这一行没覆盖"，提交时统一图片又回来了——付费行为，不能靠运气。
   */
  clearedFiles?: string[]
  /**
   * 这一条自己的名字（可选，进产物文件名）。
   *
   * **只在内存里**（见 `PersistedDraft` 的说明）：它在提交时就是 `clientRunId`，
   * 而草稿刻意不把参与幂等的标识带回下一次会话。
   */
  name?: string
  /** 提交后的行级执行状态（草稿行无）。 */
  status?: 'pending' | 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled'
  /** 提交时生成的显式 id，行↔run 映射以此为准。 */
  clientRunId?: string
  runId?: number
  taskId?: string
}

/**
 * 草稿在 localStorage 里的形状。**带版本号**：形状变了就整份丢弃，而不是猜着迁移
 * ——草稿是"用户随手填的东西"，猜错的迁移比丢掉更糟（会填进错的参数，而那是花钱的）。
 */
interface PersistedDraft {
  values: Record<string, unknown>
  files: Record<string, RunnerFileRef>
  mode: 'single' | 'batch'
  batchCount: string
  variableKeys: string[]
  /**
   * 行草稿：**只留 `id` / `values` / `files`**。
   *
   * ⚠️ 刻意**不存** `status` / `runId` / `taskId` / `clientRunId`：
   * * `status` 是**上一次会话的执行状态**，存下来会让重开表单时显示一个假的"运行中"
   *   （而那个任务早就结束了）；
   * * `clientRunId` 参与**幂等**（见 `clients/verbs.rs`：调用方给了就用它）——
   *   把一个上一轮的 id 带回来，最坏的结果是服务端**去重**、于是"点了生成却什么都没发生"。
   */
  batchRows: Array<Pick<RunnerBatchRow, 'id' | 'values' | 'files' | 'clearedFiles'>>
}

const DRAFT_STORAGE_KEY = 'renderloft-runner-drafts'
const DRAFT_VERSION = 1
/** 落盘节流：表单是**每次输入**都会 update 的，逐键写 localStorage 是白费。 */
const PERSIST_DEBOUNCE_MILLIS = 400

/**
 * 读回草稿。**任何异常都当成"没有草稿"**：localStorage 里可能是别的版本、被手工改过、
 * 或者是别的应用写坏的数据 —— 那都不该让表单打不开（草稿只是便利，不是数据）。
 */
const loadDrafts = (): Record<string, PersistedDraft> => {
  try {
    const raw = localStorage.getItem(DRAFT_STORAGE_KEY)
    if (!raw) return {}
    const parsed = JSON.parse(raw) as { version?: number; drafts?: Record<string, PersistedDraft> }
    if (parsed?.version !== DRAFT_VERSION || !parsed.drafts) return {}
    return parsed.drafts
  } catch {
    return {}
  }
}

const draftOf = (session: RunnerSession): PersistedDraft => ({
  values: session.values,
  files: session.files,
  mode: session.mode,
  batchCount: session.batchCount,
  variableKeys: session.variableKeys ?? [],
  batchRows: (session.batchRows ?? []).map((row) => ({
    id: row.id,
    values: row.values,
    files: row.files,
    clearedFiles: [...(row.clearedFiles ?? [])]
  }))
})

export const useRunnerSessionsStore = defineStore('runner-sessions', () => {
  const tabs = ref<RunnerSession[]>([])
  const workspace = useV2WorkspaceStore()
  /**
   * 每个应用的**草稿**：与"打开的标签"分开存。
   *
   * 为什么分开（这是产品愿景里"保存参数、复用历史"的那半句）：标签是**这一次会话**开了哪些应用，
   * 而草稿是"这个应用我填到哪儿了"。把标签也一起存下来会让重开应用后**多出几个没开过的标签**
   * （今天的界面里，重开就是干净的一排），那不是用户要的；草稿则应当**活过重启**。
   */
  const drafts = ref<Record<string, PersistedDraft>>(loadDrafts())

  let persistTimer: number | undefined
  const persistNow = () => {
    try {
      localStorage.setItem(
        DRAFT_STORAGE_KEY,
        JSON.stringify({ version: DRAFT_VERSION, drafts: drafts.value })
      )
    } catch {
      // 存不下（配额满 / 隐私模式）就算了：草稿是便利，不该让填表本身失败。
    }
  }
  const schedulePersist = () => {
    if (persistTimer !== undefined) window.clearTimeout(persistTimer)
    persistTimer = window.setTimeout(() => {
      persistTimer = undefined
      persistNow()
    }, PERSIST_DEBOUNCE_MILLIS)
  }

  /** 把内存里的标签同步进草稿（`update` / 表单变化都走这里，保证两者不会各说各话）。 */
  const remember = (session: RunnerSession) => {
    drafts.value = { ...drafts.value, [session.appId]: draftOf(session) }
    schedulePersist()
  }

  const open = (appId: string, title: string) => {
    // 记录「最近使用」：本函数是**所有**打开配方路径的唯一漏斗——
    // 资源库网格、首页最近卡片、AppShell 工作标签、直接访问 /library/{id}，
    // 最终都会进入 V2Runner 并调用它（V2Runner.vue setup 与加载后各一次）。
    // 所以放在这一处即可，不必在每个入口各补一次（重复的规则迟早打架）。
    // 失败静默：记录"最近使用"不该影响打开流程本身。
    const recipeId = Number(appId)
    if (Number.isFinite(recipeId)) void workspace.touchRecipe(recipeId).catch(() => {})

    const existing = tabs.value.find((tab) => tab.appId === appId)
    if (existing) {
      existing.title = title
      existing.files ??= {}
      existing.batchCount ??= '2'
      existing.variableKeys ??= []
      existing.batchRows ??= []
      return existing
    }
    // 新标签**从草稿起步**：这就是"关掉应用再打开，参数还在"的全部实现。
    const draft = drafts.value[appId]
    const tab: RunnerSession = {
      appId,
      title,
      values: { ...(draft?.values ?? {}) },
      files: { ...(draft?.files ?? {}) },
      mode: draft?.mode ?? 'single',
      batchCount: draft?.batchCount ?? '2',
      variableKeys: [...(draft?.variableKeys ?? [])],
      batchRows: (draft?.batchRows ?? []).map((row) => ({
        id: row.id,
        values: { ...row.values },
        files: { ...row.files },
        clearedFiles: [...(row.clearedFiles ?? [])]
      }))
    }
    tabs.value.push(tab)
    return tab
  }

  const get = (appId: string) => tabs.value.find((tab) => tab.appId === appId)

  const update = (
    appId: string,
    patch: Partial<
      Pick<RunnerSession, 'values' | 'files' | 'mode' | 'batchCount' | 'variableKeys' | 'batchRows'>
    >
  ) => {
    const tab = get(appId)
    if (!tab) return
    if (patch.values) tab.values = { ...patch.values }
    if (patch.files) tab.files = { ...patch.files }
    if (patch.mode) tab.mode = patch.mode
    if (patch.batchCount !== undefined) tab.batchCount = patch.batchCount
    if (patch.variableKeys) tab.variableKeys = [...patch.variableKeys]
    if (patch.batchRows)
      tab.batchRows = patch.batchRows.map((row) => ({
        ...row,
        values: { ...row.values },
        files: { ...row.files },
        clearedFiles: [...(row.clearedFiles ?? [])]
      }))
    remember(tab)
  }

  /**
   * 清空这个应用的**草稿内容**（保留标签本身与"单次/批量"这类偏好）。
   *
   * 谁用它：表单上的「清空」（人主动要一张干净的表）；以及关标签页时（那是"我不用这个应用了"
   * 的显式动作，与浏览器里关掉一个填了一半的页面同义）。
   */
  const reset = (appId: string) => {
    const tab = get(appId)
    if (tab) {
      tab.values = {}
      tab.files = {}
      tab.batchRows = []
      tab.variableKeys = []
    }
    const rest = { ...drafts.value }
    delete rest[appId]
    drafts.value = rest
    persistNow()
  }

  const close = (appId: string) => {
    tabs.value = tabs.value.filter((tab) => tab.appId !== appId)
    // 关标签页 = "这个应用我暂时不用了"：草稿一起清掉，否则下次打开会看到一张
    // 自己都忘了什么时候填的表。
    reset(appId)
  }

  /**
   * 落盘节流器在**页面隐藏/卸载**时立刻兑现：否则最后 400 毫秒里敲的字会丢，
   * 而"刚敲完就关掉"恰恰是最常见的一种关法。
   *
   * （表单每次输入都会走 `update` → `remember` → 排一次落盘，所以这里只需要管"还没到期就关了"。）
   */
  if (typeof window !== 'undefined') {
    const flush = () => {
      if (persistTimer !== undefined) {
        window.clearTimeout(persistTimer)
        persistTimer = undefined
        persistNow()
      }
    }
    window.addEventListener('pagehide', flush)
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'hidden') flush()
    })
  }

  return { tabs, open, get, update, reset, close }
})
