<template>
  <div
    class="min-h-dvh bg-background text-foreground"
    :class="taskCenter.isOpen ? 'task-center-is-open' : 'task-center-is-closed'"
  >
    <aside
      class="app-sidebar glass-chrome glass-hairline-r fixed inset-y-0 left-0 z-nav hidden w-60 flex-col lg:flex"
    >
      <div class="flex h-16 items-center gap-3 border-b border-border px-5">
        <div class="brand-mark"><img :src="logoUrl" alt="" class="brand-logo" /></div>
        <div>
          <p class="text-sm font-semibold tracking-normal">RenderLoft</p>
          <p class="text-[11px] text-muted-foreground">{{ t('shell.localWorkspace') }}</p>
        </div>
      </div>
      <nav class="flex-1 px-3 py-5" :aria-label="t('shell.mainNav')">
        <p class="px-3 pb-2 text-[10px] font-semibold tracking-normal text-muted-foreground">
          {{ t('shell.workspaceGroup') }}
        </p>
        <RouterLink
          v-for="item in navigation"
          :key="item.to"
          :to="item.to"
          class="nav-item"
          :class="isNavigationActive(item.to) && 'is-active'"
          :aria-current="isNavigationActive(item.to) ? 'page' : undefined"
          ><component :is="item.icon" class="size-4" /><span>{{ item.label }}</span></RouterLink
        >
        <p class="mt-7 px-3 pb-2 text-[10px] font-semibold tracking-normal text-muted-foreground">
          {{ t('shell.systemGroup') }}
        </p>
        <RouterLink
          to="/settings"
          class="nav-item"
          :class="isNavigationActive('/settings') && 'is-active'"
          :aria-current="isNavigationActive('/settings') ? 'page' : undefined"
          ><Settings2 class="size-4" /><span>{{ t('nav.settings') }}</span></RouterLink
        >
      </nav>
      <div class="border-t border-border p-4">
        <RouterLink
          to="/connections"
          class="glass-panel block rounded-lg p-3 transition-[border-color,box-shadow] duration-200 hover:border-primary/40"
          ><div class="flex items-center justify-between">
            <span class="text-xs font-medium">{{ t('shell.connections') }}</span
            ><span
              :class="anyConnectionReady ? 'bg-success' : 'bg-warning'"
              class="size-2 rounded-full"
            />
          </div>
          <template v-if="providerConnections.length"
            ><div class="mt-3 grid gap-2 border-t border-border pt-3">
              <div
                v-for="connection in providerConnections"
                :key="connection.connectionId"
                class="flex items-center gap-2"
              >
                <span
                  :class="connectionDotClass(connection)"
                  class="size-2 shrink-0 rounded-full"
                />
                <span class="min-w-0 flex-1 truncate text-xs">{{ connection.name }}</span>
                <span class="shrink-0 text-xs text-muted-foreground">{{
                  connectionStateText(connection)
                }}</span>
              </div>
            </div></template
          >
          <p v-else class="mt-2 text-xs text-muted-foreground">
            {{ t('shell.noConnections') }}
          </p></RouterLink
        >
      </div>
    </aside>
    <header class="app-header glass-header glass-hairline-b sticky top-0 z-header h-16 lg:ml-60">
      <div class="app-header-content flex h-full items-center justify-between">
        <RouterLink to="/" class="flex items-center gap-2 lg:hidden"
          ><div class="brand-mark"><img :src="logoUrl" alt="" class="brand-logo" /></div>
          <span class="text-sm font-semibold">RenderLoft</span></RouterLink
        >
        <div class="hidden lg:block">
          <p class="text-sm font-medium">{{ pageTitle }}</p>
          <p class="text-xs text-muted-foreground">{{ t('shell.localWorkspace') }}</p>
        </div>
        <div class="flex items-center gap-2">
          <RouterLink
            to="/connections"
            class="hidden items-center gap-2 rounded-md border border-border px-3 py-2 text-xs font-medium transition-colors hover:bg-accent sm:flex"
            ><CircleAlert v-if="!runningHubConnected" class="size-3.5 text-warning" /><span>{{
              runningHubConnected
                ? t('shell.connected')
                : runningHubStatus === 'unknown'
                  ? t('shell.connectionUnknown')
                  : t('shell.configure')
            }}</span></RouterLink
          ><button
            class="grid size-9 place-items-center rounded-md border border-border transition-colors hover:bg-accent"
            type="button"
            :aria-label="themeLabel"
            @click="toggleTheme"
          >
            <Sun v-if="theme === 'light'" class="size-4" /><Moon v-else class="size-4" />
          </button>
        </div>
      </div>
    </header>
    <nav v-if="workTabs.tabs.length && isRunnerRoute" class="work-tabs glass-hairline-b lg:ml-60">
      <div class="app-header-content flex min-w-0 gap-1 overflow-x-auto py-2">
        <div
          v-for="tab in workTabs.tabs"
          :key="tab.appId"
          class="work-tab"
          :class="String(route.params.id) === tab.appId && 'is-active'"
        >
          <button
            type="button"
            class="min-w-0 flex-1 truncate text-left"
            @click="openWorkTab(tab.appId)"
          >
            {{ tab.title }}
          </button>
          <button
            type="button"
            class="work-tab-close"
            :aria-label="t('shell.closeTab', { title: tab.title })"
            :title="t('shell.closeTab', { title: tab.title })"
            @click="closeWorkTab(tab.appId)"
          >
            <X class="size-3.5" />
          </button>
        </div>
      </div>
    </nav>
    <div class="app-main lg:ml-60">
      <main class="min-w-0">
        <RouterView v-slot="{ Component, route: matchedRoute }">
          <!--
            key 用 **path**，不用 fullPath。

            两者只差 query，但差别会咬人：`/library?edit=12` 是"从运行页带参数过来打开
            编辑框"，页面自己会 `router.replace({ query: {} })` 把参数抹掉防止刷新重弹。
            用 fullPath 当 key ⇒ 抹参数那一刻 key 变了 ⇒ 整页重挂载 ⇒ 刚打开的编辑框、
            "关掉后送回运行页"的记录全丢（功能等于不可用）。
            用 path ⇒ 换应用（`/library/1` → `/library/2`）、换页面照样重挂载，
            只换 query 时保留页面状态——这正是 query 该有的语义。
          -->
          <component :is="Component" :key="matchedRoute.path" />
        </RouterView>
      </main>
    </div>
    <TaskCenter />
    <nav
      class="fixed inset-x-0 bottom-0 z-nav flex h-16 border-t border-border bg-card px-2 pb-[env(safe-area-inset-bottom)] lg:hidden"
      :aria-label="t('shell.mobileNav')"
    >
      <RouterLink
        v-for="item in navigation"
        :key="item.to"
        :to="item.to"
        class="mobile-nav-item"
        :class="isNavigationActive(item.to) && 'is-active'"
        :aria-current="isNavigationActive(item.to) ? 'page' : undefined"
        ><component :is="item.icon" class="size-4" /><span>{{ item.label }}</span></RouterLink
      ><RouterLink
        to="/settings"
        class="mobile-nav-item"
        :class="isNavigationActive('/settings') && 'is-active'"
        :aria-current="isNavigationActive('/settings') ? 'page' : undefined"
        ><Settings2 class="size-4" /><span>{{ t('nav.settings') }}</span></RouterLink
      >
    </nav>
  </div>
</template>
<script setup lang="ts">
import { computed, onMounted, watch } from 'vue'
import TaskCenter from '@/components/TaskCenter.vue'
import {
  CircleAlert,
  Clock3,
  Link2,
  LibraryBig,
  Moon,
  Settings2,
  Sparkles,
  Sun,
  X
} from '@lucide/vue'
import { useRoute, useRouter } from 'vue-router'
import { useSettingsStore } from '@/features/settings/stores'
import { useTaskCenterStore } from '@/shared/stores/task-center'
import { useTheme } from '@/shared/composables/useTheme'
import { useLocale } from '@/shared/composables/useLocale'
import {
  isConnectionReady,
  useExecutionConnections
} from '@/shared/composables/useExecutionConnections'
import { useRunnerSessionsStore } from '@/features/v2/stores/runner-sessions'
import logoUrl from '@/assets/renderloft-logo.svg'
const route = useRoute()
const router = useRouter()
const settingsStore = useSettingsStore()
const taskCenter = useTaskCenterStore()
const workTabs = useRunnerSessionsStore()
const {
  loadConnections: loadProviderConnections,
  runningHubStatus,
  connections: providerConnections
} = useExecutionConnections()
const runningHubConnected = computed(() => runningHubStatus.value === 'connected')

// 全平台连接总览：卡片按连接列表展示，不再硬编码 RunningHub 单个提供商。
const providerRequiresApiKey = (providerId: string) =>
  providerId === 'runninghub' || providerId === 'openai-image'
const connectionReady = (connection: (typeof providerConnections.value)[number]) =>
  isConnectionReady(connection, providerRequiresApiKey(connection.providerId))
const anyConnectionReady = computed(() => providerConnections.value.some(connectionReady))
const connectionDotClass = (connection: (typeof providerConnections.value)[number]) =>
  connectionReady(connection)
    ? 'bg-success'
    : connection.detailsAvailable === false
      ? 'bg-muted-foreground'
      : 'bg-warning'
const connectionStateText = (connection: (typeof providerConnections.value)[number]) => {
  if (connectionReady(connection)) return t('connection.connected')
  if (connection.detailsAvailable === false) return t('shell.connUnreadable')
  return connection.status === 'disabled'
    ? t('shell.connDisabled')
    : connection.status === 'invalid'
      ? t('shell.connInvalid')
      : t('shell.connNotReady')
}
const { theme, toggleTheme } = useTheme()
const { t } = useLocale()
const navigation = computed(() => [
  { to: '/', label: t('nav.workspace'), icon: Sparkles },
  { to: '/library', label: t('nav.library'), icon: LibraryBig },
  { to: '/tasks', label: t('nav.tasks'), icon: Clock3 },
  { to: '/connections', label: t('nav.connections'), icon: Link2 }
])
const isNavigationActive = (to: string) =>
  to === '/' ? route.path === '/' : route.path === to || route.path.startsWith(`${to}/`)
const isRunnerRoute = computed(() => route.path.startsWith('/library/'))
const pageTitle = computed(() =>
  isRunnerRoute.value
    ? t('shell.runApp')
    : (navigation.value.find((entry) => entry.to === route.path)?.label ??
      (route.path === '/connections'
        ? t('nav.connections')
        : route.path === '/settings'
          ? t('nav.settings')
          : 'RenderLoft'))
)
const themeLabel = computed(() =>
  theme.value === 'light' ? t('shell.switchDark') : t('shell.switchLight')
)
const openWorkTab = (appId: string) => router.push({ path: `/library/${appId}` })
const closeWorkTab = (appId: string) => {
  const index = workTabs.tabs.findIndex((tab) => tab.appId === appId)
  const isCurrent = String(route.params.id) === appId
  workTabs.close(appId)
  if (!isCurrent) return
  const next = workTabs.tabs[index] ?? workTabs.tabs[index - 1]
  if (next) openWorkTab(next.appId)
  else router.push({ name: 'Library' })
}
onMounted(async () => {
  await Promise.allSettled([settingsStore.loadSettings(), loadProviderConnections()])
})
// 这里原来有两条 watch：RunningHub 就绪后启动队列的 15 秒轮询，并让余额跟着那个节拍刷新。
// 两条都是为侧栏那两格（「RH 币 / 钱包」）服务的，而它们已经删了——面的家改到连接页的
// **凭证那一行**（见 features/connections/views/Connections.vue 与 shared/lib/connectionFacet.ts）。
// 于是：
//   · `/api/v1/runninghub/queue` 每 15 秒一次的那个请求**一次都不再发**。它取回来的数
//     此前没有任何地方渲染（全仓库只有本文件读 taskSyncStore，而只读了 `lastRefreshedAt`），
//     等于每 15 秒换一个看不见的数字；
//   · 余额只在连接页打开 / 测试通过 / 保存之后查，并且**带上 connectionId**。原来不带，
//     后端于是取列表里第一条 RunningHub 连接（`application/settings.rs` 的
//     `runninghub_revision`）——配了国内站 + 国际站两条连接时，显示的是**另一个账号**的数。
// 顺带消失的还有启动时那次重复的账户查询（原先注释里写明的"刻意不去重"）。
// 路由切换时刷新连接缓存：左下角状态按最新健康检查重新判定，
// 避免启动时一次失败的检查让连接一直显示橙色警告。
watch(
  () => route.path,
  () => {
    void loadProviderConnections(true).catch(() => undefined)
  }
)
</script>
<style scoped>
/*
 * 这里曾有一层「大气层」：两团径向色光 + 一层网格，固定铺满视口。
 * 已按光学预算删除（见 style.css 的预算表）：它纯装饰、与内容争注意力，
 * 而且让"主按钮是主行动"这件事失效 —— 满屏都在发光时，没有焦点。
 * 若将来要做"空态/首启"这类没有内容的页面，可以在那一页局部再加回来。
 */
/* 侧边栏右侧的分隔线由 .glass-hairline-r 提供（见 style.css 的共享类） */
.brand-mark {
  display: grid;
  width: 2rem;
  height: 2rem;
  place-items: center;
}
.brand-logo {
  width: 2rem;
  height: 2rem;
  object-fit: contain;
}
.work-tabs {
  position: sticky;
  top: 4rem;
  z-index: var(--z-work-tabs);
  /* 实色：全站只允许顶栏磨砂（见 style.css 的光学预算） */
  background: var(--glass-chrome-bg);
}
/* 底边分隔线由 .glass-hairline-b 提供（见 style.css 的共享类） */
.work-tab {
  display: flex;
  flex: 0 0 min(13rem, 42vw);
  align-items: center;
  gap: 0.25rem;
  min-width: 0;
  height: 2rem;
  border: 1px solid var(--border);
  padding-left: 0.625rem;
  color: var(--muted-foreground);
  font-size: 0.75rem;
}
.work-tab:hover {
  background: var(--accent);
  color: var(--accent-foreground);
}
.work-tab.is-active {
  border-color: var(--primary);
  background: var(--background);
  color: var(--foreground);
  font-weight: 600;
}
.work-tab-close {
  display: grid;
  flex: none;
  width: 1.75rem;
  height: 1.75rem;
  place-items: center;
  color: var(--muted-foreground);
}
.work-tab-close:hover {
  background: var(--muted);
  color: var(--foreground);
}
.nav-item,
.mobile-nav-item {
  color: var(--app-muted-foreground);
}
.nav-item {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  min-height: 2.5rem;
  padding: 0 0.75rem;
  border-radius: var(--radius-md);
  font-size: 0.875rem;
  transition:
    background-color 150ms ease,
    color 150ms ease;
}
.nav-item:hover,
.nav-item.is-active {
  background: var(--app-accent);
  color: var(--app-accent-foreground);
}
.nav-item.is-active {
  box-shadow: inset 2px 0 0 var(--app-info);
}
.mobile-nav-item {
  display: flex;
  flex: 1;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 0.25rem;
  font-size: 10px;
}
.mobile-nav-item.is-active {
  color: var(--app-info);
}
.app-header-content {
  width: 100%;
  padding: 0 1rem;
}
@media (min-width: 640px) {
  .app-header-content {
    padding: 0 1.5rem;
  }
}
@media (min-width: 1024px) {
  .app-header-content {
    padding: 0 2rem;
  }
}
/**
 * 让位阈值 1200 是任务中心布局契约的一部分，与下面几处同源：
 *   - `shared/config/layout.ts` 的 TASKCENTER_OVERLAY_MAX_WIDTH（JS 侧权威定义）
 *   - `components/TaskCenter.vue` 的浮层判定（含 1024–1199 的浮层样式）
 *   - `shared/stores/task-center.ts` 的默认展开判定
 *
 * 语义：≥1200 时右栏常驻，主区让出 `--task-rail-width`；<1200 时右栏转为浮层
 * （浮层由 TaskCenter 自行处理遮罩与 dialog 语义），主区不让位、内容吃满宽度。
 * 阈值取 1200 = 桌面外壳的默认窗口宽度，好让**默认尺寸下右栏就是常驻的**（理由见 layout.ts）。
 * CSS 媒体查询读不到 JS 常量，所以断点只能是字面量；但**让位量本身是 token**
 * （--task-rail-width / --task-rail-peek），与 TaskCenter 的平移同源，别再写死。
 */
@media (min-width: 1200px) {
  .task-center-is-open .app-main,
  .task-center-is-open > header,
  .task-center-is-open > .work-tabs {
    margin-right: var(--task-rail-width);
  }
  /*
   * 收起态也要让位：面板按设计只平移 (宽度 − 窄条)，右缘会留下 --task-rail-peek 宽的
   * 窄条（toggle 轨道）。不让位的话这条窄条会压在内容右缘上 —— 实测会盖住
   * 「刷新」按钮 12px、最后一个统计格 7px，以及归档网格最右列。
   */
  .task-center-is-closed .app-main,
  .task-center-is-closed > header,
  .task-center-is-closed > .work-tabs {
    margin-right: var(--task-rail-peek);
  }
}
</style>
