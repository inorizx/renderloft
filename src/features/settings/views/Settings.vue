<template>
  <PageContainer class="max-w-5xl">
    <PageHeader
      :eyebrow="t('settings.eyebrow')"
      :title="t('settings.title')"
      :description="t('settings.description')"
    />

    <form class="mt-5 space-y-5" @submit.prevent="handleSave">
      <Card v-if="isDesktop" class="p-5">
        <SectionHeader :title="t('settings.downloads')" />
        <div class="mt-6 space-y-5">
          <FormField
            :label="t('settings.downloadDir')"
            :description="t('settings.downloadDirDescription')"
          >
            <div class="flex gap-2">
              <Input
                v-model.trim="downloadDirInput"
                :placeholder="t('settings.downloadDirPlaceholder')"
              />
              <Button type="button" variant="outline" class="shrink-0" @click="openDownloadDir">{{
                t('settings.openDir')
              }}</Button>
            </div>
          </FormField>
          <div class="flex justify-end border-t border-border pt-5">
            <Button class="shrink-0" type="button" :disabled="isSaving" @click="handleSave"
              ><LoaderCircle v-if="isSaving" class="size-4 animate-spin" /><Save
                v-else
                class="size-4"
              />{{ isSaving ? t('settings.saving') : t('settings.save') }}</Button
            >
          </div>
          <NoticePanel v-if="saveSuccess" variant="success">{{
            t('settings.savedConfigHint')
          }}</NoticePanel>
          <NoticePanel v-if="saveError" variant="destructive">{{ saveError }}</NoticePanel>
        </div>
      </Card>

      <Card class="p-5">
        <SectionHeader :title="t('settings.language')" />
        <div class="mt-5">
          <FormField :label="t('settings.language')" :description="t('settings.languageHint')">
            <Select :model-value="locale" @update:model-value="changeLocale">
              <option value="zh-CN">{{ t('settings.languageZh') }}</option>
              <option value="en-US">{{ t('settings.languageEn') }}</option>
            </Select>
          </FormField>
        </div>
      </Card>

      <Card class="p-5">
        <SectionHeader :title="t('settings.theme')">
          <template #actions
            ><button
              class="relative h-6 w-11 rounded-full p-0.5 transition-colors duration-150"
              :class="theme === 'dark' ? 'bg-primary' : 'bg-muted'"
              type="button"
              role="switch"
              :aria-label="t('settings.theme')"
              :aria-checked="theme === 'dark'"
              @click="setTheme(theme === 'dark' ? 'light' : 'dark')"
            >
              <span
                class="block size-5 rounded-full bg-white transition-transform duration-150"
                :class="theme === 'dark' ? 'translate-x-5' : 'translate-x-0'"
              /></button
          ></template>
        </SectionHeader>
      </Card>

      <Card class="p-5">
        <SectionHeader :title="t('settings.updateApps')">
          <template #actions>
            <button
              class="relative h-6 w-11 rounded-full p-0.5 transition-colors duration-150"
              :class="store.autoRefreshApps ? 'bg-primary' : 'bg-muted'"
              type="button"
              role="switch"
              :aria-label="t('settings.updateApps')"
              :aria-checked="store.autoRefreshApps"
              @click="store.setAutoRefreshApps(!store.autoRefreshApps)"
            >
              <span
                class="block size-5 rounded-full bg-white transition-transform duration-150"
                :class="store.autoRefreshApps ? 'translate-x-5' : 'translate-x-0'"
              />
            </button>
          </template>
        </SectionHeader>
        <p class="mt-3 text-sm text-muted-foreground">{{ t('settings.updateAppsHint') }}</p>
      </Card>

      <Card class="p-5">
        <SectionHeader
          :title="t('settings.diagnostics')"
          :description="t('settings.diagnosticsHint')"
        >
          <template #actions
            ><Button
              variant="outline"
              type="button"
              :disabled="isCopyingDiagnostics || !isDesktop"
              @click="handleCopyDiagnostics"
              ><LoaderCircle v-if="isCopyingDiagnostics" class="size-4 animate-spin" /><Clipboard
                v-else
                class="size-4"
              />{{
                isCopyingDiagnostics
                  ? t('settings.copyingDiagnostics')
                  : t('settings.copyDiagnostics')
              }}</Button
            ></template
          >
        </SectionHeader>
        <!--
          运行参数：**这些值全都来自环境变量或程序位置**，所以"出问题能不能自己改"完全
          取决于它们看不看得见——只写在 NEXT.md（维护者文件）里等于没有。
          出网代理必须在里面：reqwest 默认会用它，代理软件关掉之后应用表现为"连不上平台 /
          key 好像坏了"，而界面上原本没有任何线索（2026-10-12 的排查）。
          每一行都带「改哪里」：只说"现在是什么"帮不上忙。
        -->
        <div v-if="runtime" class="mt-4 border-t border-border pt-3">
          <p class="text-xs font-medium">{{ t('settings.runtimeParams') }}</p>
          <p class="mt-1 text-xs leading-5 text-muted-foreground">
            {{ t('settings.runtimeParamsHint') }}
          </p>
          <dl class="mt-3 grid gap-2 text-xs">
            <div
              v-for="row in runtimeRows"
              :key="row.label"
              class="flex flex-wrap items-baseline gap-x-2 gap-y-1"
            >
              <dt class="w-32 shrink-0 text-muted-foreground">{{ row.label }}</dt>
              <dd class="min-w-0 flex-1">
                <code class="break-all">{{ row.value }}</code>
              </dd>
              <dd v-if="row.where" class="shrink-0 text-muted-foreground">
                {{ t('settings.runtimeWhere') }}: <code>{{ row.where }}</code>
              </dd>
            </div>
          </dl>
          <p v-if="defaults" class="mt-3 text-xs leading-5 text-muted-foreground">
            {{ t('settings.defaultRequestTimeout') }}:
            <code>{{ t('settings.secondsValue', { value: defaults.requestTimeoutSeconds }) }}</code>
            · {{ t('settings.defaultTrackingDeadline') }}:
            <code>{{
              t('settings.secondsValue', { value: defaults.trackingDeadlineSeconds })
            }}</code>
            —— {{ t('settings.defaultsHint') }}
          </p>
          <!--
            日志：给"出问题以后把目录交给 agent 去发掘"用的。**必须报"最后写入时间"**——
            daemon 的输出是启动它的那一方重定向的，手工/CLI 启动时可能丢进 `NUL`，
            那时目录里会留着一个陈旧的日志，只看路径会以为"有日志"从而读错东西。
          -->
          <div v-if="logs?.directory" class="mt-3 border-t border-border pt-3 text-xs">
            <p class="font-medium">{{ t('settings.logs') }}</p>
            <p class="mt-1 leading-5 text-muted-foreground">{{ t('settings.logsHint') }}</p>
            <p class="mt-0.5 leading-5 text-muted-foreground">
              {{ t('settings.logsTimezone', { offset: utcOffsetLabel }) }}
            </p>
            <p class="mt-1 break-all">
              <code>{{ logs.directory }}</code>
            </p>
            <p v-if="logs.files.length" class="mt-1 leading-5 text-muted-foreground">
              <span v-for="file in logs.files" :key="file.name" class="mr-3 inline-block">
                <code>{{ file.name }}</code> {{ formatFileSize(file.bytes) }} ·
                {{ formatRelative(file.modifiedAtMillis) }}
              </span>
            </p>
            <p v-else class="mt-1 leading-5 text-warning">{{ t('settings.logsMissing') }}</p>
          </div>
        </div>
      </Card>
    </form>
  </PageContainer>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Clipboard, LoaderCircle, Save } from '@lucide/vue'
import PageContainer from '@/components/layout/PageContainer.vue'
import PageHeader from '@/components/PageHeader.vue'
import SectionHeader from '@/components/shared/SectionHeader.vue'
import FormField from '@/components/shared/FormField.vue'
import NoticePanel from '@/components/shared/NoticePanel.vue'
import { Button, Card, Input, Select } from '@/components/ui'
import { useSettingsStore } from '@/features/settings/stores'
import { useAppStore } from '@/shared/stores/app'
import { isDesktopRuntime } from '@/shared/lib/runtime'
import type { DiagnosticInfo } from '@/shared/types/runninghub'
import { useTheme } from '@/shared/composables/useTheme'
import { useLocale, type AppLocale } from '@/shared/composables/useLocale'
import { v2 } from '@/shared/api/v2'
import { useExecutionConnections } from '@/shared/composables/useExecutionConnections'

const store = useSettingsStore()
const appStore = useAppStore()
const { theme, setTheme } = useTheme()
const { locale, setLocale, t } = useLocale()
const isSaving = ref(false)
const isCopyingDiagnostics = ref(false)
const saveError = ref('')
const saveSuccess = ref(false)
const isDesktop = isDesktopRuntime()
const downloadDirInput = ref('')
/**
 * daemon **当前生效**的运行参数与默认值（诊断卡片里显示）。
 *
 * 取的是诊断接口报的值，不是这里再猜一遍——"界面说 X、daemon 用 Y"比不显示更糟。
 */
const diagnostics = ref<DiagnosticInfo>()
const runtime = computed(() => diagnostics.value?.runtime)
const defaults = computed(() => diagnostics.value?.defaults)
const logs = computed(() => diagnostics.value?.logs)
/** 日志文件大小：给人看的一行（KB/MB），不追求精确。 */
const formatFileSize = (bytes: number): string =>
  bytes >= 1024 * 1024
    ? `${(bytes / 1024 / 1024).toFixed(1)} MB`
    : `${Math.max(1, Math.round(bytes / 1024))} KB`
/**
 * 本机相对 UTC 的偏移（如 `UTC+8`）。
 *
 * 日志里是 **UTC**（末尾 `Z`），而用户脑子里的时间是他自己的墙上时钟——这两个数差几个小时时
 * 会把"昨晚 11 点那次失败"读成"今天下午 3 点"。浏览器知道偏移，所以在这里把它说清楚：
 * 一句话消除歧义，比在日志里塞本地时间便宜得多（本地时间需要平台相关的时区查询，
 * 在多线程 daemon 里还不安全）。
 */
const utcOffsetLabel = computed(() => {
  const minutes = -new Date().getTimezoneOffset()
  const sign = minutes < 0 ? '-' : '+'
  const absolute = Math.abs(minutes)
  const hours = Math.floor(absolute / 60)
  const rest = absolute % 60
  return rest ? `UTC${sign}${hours}:${String(rest).padStart(2, '0')}` : `UTC${sign}${hours}`
})
/**
 * "最后写入"用**相对时间**（"3 分钟前"）：绝对时间戳对"它还在写吗"这个判断没有帮助，
 * 而那个判断正是这一行存在的理由（陈旧的日志 = 日志没在写）。
 */
const formatRelative = (millis: number): string => {
  if (!millis) return t('settings.logsUnknownTime')
  const seconds = Math.max(0, Math.round((Date.now() - millis) / 1000))
  if (seconds < 90) return t('settings.logsJustNow')
  const minutes = Math.round(seconds / 60)
  if (minutes < 90) return t('settings.logsMinutesAgo', { n: minutes })
  return t('settings.logsHoursAgo', { n: Math.round(minutes / 60) })
}
/**
 * 出网代理：它是"连不上平台 / key 好像坏了"的常见原因，而它**默认是不可见的**。
 *
 * 2026-10-26 起 daemon 自己读 Windows 系统代理（reqwest 不读）。界面**不为它单开一行**：
 * 它是环境的事，不是一个可以在设置页里调的参数。这里只留一份文本，供"复制诊断"用——
 * 出问题时贴给别人的那一段里，它往往才是决定性的那一句。
 *
 * 没配时显示**空态的含义**（"未设置：直连"），而不是留一格空白——空白看起来像"读不到"。
 */
const outboundProxyText = computed(
  () => diagnostics.value?.outboundProxy ?? t('settings.runtimeOutboundProxyEmpty')
)
/**
 * 运行参数那张表：每行 = 叫什么 / 现在是什么 / **改哪个变量**。
 *
 * 第三列是这一块存在的理由：这些值全都来自环境变量或程序位置，"出问题能不能自己改"
 * 完全取决于用户知不知道变量名。只说"现在是什么"帮不上忙。
 */
const runtimeRows = computed(() => {
  const current = runtime.value
  if (!current) return []
  const rows = [
    { label: t('settings.runtimeBind'), value: current.bind, where: 'RENDERLOFT_BIND' },
    {
      label: t('settings.runtimeAllowedHosts'),
      value: current.allowedHosts.length
        ? current.allowedHosts.join(', ')
        : t('settings.runtimeAllowedHostsEmpty'),
      where: 'RENDERLOFT_ALLOW_HOST'
    },
    {
      label: t('settings.runtimeUiOrigin'),
      value: current.uiOrigin ?? t('settings.runtimeUiOriginEmpty'),
      where: 'RENDERLOFT_UI_ORIGIN'
    },
    { label: t('settings.runtimeDistDir'), value: current.distDir, where: 'RENDERLOFT_DIST' }
  ]
  return rows
})
/*
 * 这一页只管**本地偏好**：界面、主题、更新与诊断。
 * 连接与凭据一律在「连接」页管理——本页不再放那两张卡片，它们在那里只剩一句说明 + 一个
 * 跳转按钮，是纯粹的复读。
 * loadConnections 保留：进这一页顺手刷新共享连接缓存，左下角状态才不会发旧。
 */
const { loadConnections } = useExecutionConnections()
const changeLocale = (value: string) => {
  if (value === 'zh-CN' || value === 'en-US') setLocale(value as AppLocale)
}
const clearSaveStatus = () => {
  saveSuccess.value = false
  saveError.value = ''
}
const openDownloadDir = async () => {
  try {
    await v2.post('/api/v1/settings/open-download-dir')
  } catch (error) {
    appStore.showNotification(
      'error',
      t('settings.openDownloadDirFailed', {
        message: error instanceof Error ? error.message : String(error)
      })
    )
  }
}
const handleCopyDiagnostics = async () => {
  if (isCopyingDiagnostics.value) return
  isCopyingDiagnostics.value = true
  try {
    const loaded = await v2.get<DiagnosticInfo>('/api/v1/diagnostics')
    diagnostics.value = loaded
    const text = [
      t('settings.diagnosticsTitle'),
      `${t('settings.version')}: ${loaded.appVersion}`,
      `${t('settings.system')}: ${loaded.systemOs} (${loaded.systemFamily}/${loaded.systemArch})`,
      `${t('settings.serviceRegion')}: ${loaded.serviceRegion}`,
      `${t('settings.serviceHost')}: ${loaded.serviceHost}`,
      `${t('settings.apiKeyConfigured')}: ${loaded.apiKeyConfigured ? t('common.yes') : t('common.no')}`,
      // 运行参数一起复制：贴给别人的诊断信息里，**"你那边实际生效的是什么"**往往才是
      // 决定性的那几行（绑定地址、静态目录、界面来源、代理）。
      ...(loaded.runtime
        ? [
            `${t('settings.runtimeBind')}: ${loaded.runtime.bind}`,
            `${t('settings.runtimeAllowedHosts')}: ${loaded.runtime.allowedHosts.join(', ') || '-'}`,
            `${t('settings.runtimeUiOrigin')}: ${loaded.runtime.uiOrigin ?? '-'}`,
            `${t('settings.runtimeDistDir')}: ${loaded.runtime.distDir}`
          ]
        : []),
      `${t('settings.outboundProxy')}: ${outboundProxyText.value}`
    ].join('\n')
    await navigator.clipboard.writeText(text)
    appStore.showNotification('success', t('settings.diagnosticsCopied'))
  } catch (error) {
    appStore.showNotification(
      'error',
      t('settings.copyDiagnosticsFailed', {
        message: error instanceof Error ? error.message : String(error)
      })
    )
  } finally {
    isCopyingDiagnostics.value = false
  }
}
const handleSave = async () => {
  if (isSaving.value) return
  isSaving.value = true
  clearSaveStatus()
  try {
    await store.saveSettings(downloadDirInput.value)
    saveSuccess.value = true
    appStore.showNotification('success', t('settings.saveSuccess'))
  } catch (error) {
    saveError.value = t('settings.saveFailed', {
      message: error instanceof Error ? error.message : String(error)
    })
    appStore.showNotification('error', saveError.value)
  } finally {
    isSaving.value = false
  }
}
onMounted(async () => {
  // 各初始化步骤相互独立，单个失败不阻断其余配置加载。
  try {
    await store.loadSettings()
  } catch (error) {
    saveError.value = t('settings.loadFailed', {
      message: error instanceof Error ? error.message : String(error)
    })
  }
  try {
    await loadConnections()
  } catch (error) {
    saveError.value = t('settings.connectionsLoadFailed', {
      message: error instanceof Error ? error.message : String(error)
    })
  }
  // 诊断信息为"运行参数那张表"预取：拿不到就整块不显示，不打扰用户（复制按钮还会再取一次）。
  try {
    diagnostics.value = await v2.get<DiagnosticInfo>('/api/v1/diagnostics')
  } catch {
    diagnostics.value = undefined
  }
  downloadDirInput.value = store.downloadDir
})
</script>
