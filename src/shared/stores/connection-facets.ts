import { ref } from 'vue'
import { defineStore } from 'pinia'
import { v2 } from '@/shared/api/v2'
import type { AccountStatus, QueueStatus } from '@/shared/types/runninghub'

/**
 * 连接的**状态面**，按连接 id 存。
 *
 * 为什么按连接存，而不是像原来那样全局一份：面的归属是**账号**，账号由那条连接的凭据决定。
 * 这个 store 的前身（`stores/account.ts`）只有一份 `status`，查的是
 * `/api/v1/runninghub/account`——**不带 connectionId**。后端于是取列表里**第一条**
 * RunningHub 连接（`application/settings.rs` 的 `runninghub_revision`）。配了国内站 + 国际站
 * 两条连接（两个独立账号，见 `Connections.vue` 的端点预设）时，界面上那个数可能是
 * **另一个账号**的。带上 id 才说得清"这是谁的钱"。
 *
 * 面的**种类**也按连接存：本地 ComfyUI 一个面都没有，别的平台将来报的也不是币。
 * 写成"余额 + 队列两个全局变量"，第二个平台进来就要改形状。
 *
 * 这里只记「查不到」；「平台不提供」由调用方按厂商判断（只有 RunningHub 实现这两个面，
 * 见 `src-tauri/src/v2/ports/provider.rs` 的默认实现返回 `STATUS_NOT_SUPPORTED`）。
 * 两者必须分得开：把"不提供"显示成错误，用户会去修一个没坏的东西。
 */
export interface ConnectionFacets {
  account?: AccountStatus
  queue?: QueueStatus
  accountError?: string
  queueError?: string
  loading?: boolean
}

const messageOf = (reason: unknown) => (reason instanceof Error ? reason.message : String(reason))

export const useConnectionFacetsStore = defineStore('connection-facets', () => {
  const facets = ref<Record<number, ConnectionFacets>>({})

  const facetsFor = (connectionId?: number | null): ConnectionFacets =>
    (connectionId == null ? undefined : facets.value[connectionId]) ?? {}

  const patch = (connectionId: number, next: Partial<ConnectionFacets>) => {
    facets.value = { ...facets.value, [connectionId]: { ...facetsFor(connectionId), ...next } }
  }

  /**
   * 拉一条连接的全部面。
   *
   * 两个端点**各自成败**：队列挂了余额照样要显示（这是两个面，不是一个）。所以用
   * `allSettled` 而不是 `all`——`all` 会让队列的失败把余额一起吞掉。
   */
  async function refresh(connectionId: number) {
    patch(connectionId, { loading: true })
    const [account, queue] = await Promise.allSettled([
      v2.get<AccountStatus>(`/api/v1/runninghub/account?connectionId=${connectionId}`),
      v2.get<QueueStatus>(`/api/v1/runninghub/queue?connectionId=${connectionId}`)
    ])
    patch(connectionId, {
      loading: false,
      ...(account.status === 'fulfilled'
        ? { account: account.value, accountError: undefined }
        : { accountError: messageOf(account.reason) }),
      ...(queue.status === 'fulfilled'
        ? { queue: queue.value, queueError: undefined }
        : { queueError: messageOf(queue.reason) })
    })
  }

  async function refreshMany(connectionIds: number[]) {
    await Promise.all(connectionIds.map((connectionId) => refresh(connectionId)))
  }

  /**
   * 连接被删掉之后清掉它的面：留着的话，下一次刷新前的这一段时间里，
   * 界面上会出现一个已经不存在的账号的余额。
   */
  function forget(connectionId: number) {
    const next = { ...facets.value }
    delete next[connectionId]
    facets.value = next
  }

  /**
   * 只保留名单里的面。
   *
   * 一条连接**不再够格**（停用、Key 被删、测试失败）时，上一次查到的余额就**不再是它的事实**
   * ——留着会让一条已经不能用的连接继续显示一个账号的数。清掉比在渲染处再判一次更稳：
   * 判据只有一处（调用方的 `shouldLoadFacets`），渲染处不必知道"什么算够格"。
   */
  function keepOnly(connectionIds: number[]) {
    const keep = new Set(connectionIds)
    const next: Record<number, ConnectionFacets> = {}
    for (const [key, value] of Object.entries(facets.value)) {
      const id = Number(key)
      if (keep.has(id)) next[id] = value
    }
    facets.value = next
  }

  return { facets, facetsFor, refresh, refreshMany, forget, keepOnly }
})
