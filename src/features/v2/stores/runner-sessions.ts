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
  /** 提交后的行级执行状态（草稿行无）。 */
  status?: 'pending' | 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled'
  /** 提交时生成的显式 id，行↔run 映射以此为准。 */
  clientRunId?: string
  runId?: number
  taskId?: string
}

export const useRunnerSessionsStore = defineStore('runner-sessions', () => {
  const tabs = ref<RunnerSession[]>([])
  const workspace = useV2WorkspaceStore()

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
    const tab: RunnerSession = {
      appId,
      title,
      values: {},
      files: {},
      mode: 'single',
      batchCount: '2',
      variableKeys: [],
      batchRows: []
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
        files: { ...row.files }
      }))
  }

  const close = (appId: string) => {
    tabs.value = tabs.value.filter((tab) => tab.appId !== appId)
  }

  return { tabs, open, get, update, close }
})
