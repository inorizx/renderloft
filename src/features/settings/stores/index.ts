import { defineStore } from 'pinia'
import { ref } from 'vue'
import { v2 } from '@/shared/api/v2'

interface SettingsSnapshot {
  downloadDir: string
}

export const useSettingsStore = defineStore('settings', () => {
  const downloadDir = ref('')
  const defaultAspectRatio = ref('1:1')
  const defaultResolution = ref('1k')
  const autoRefreshApps = ref(true)
  const isLoaded = ref(false)
  const isLoading = ref(false)
  const loadError = ref('')
  let loadPromise: Promise<void> | undefined

  const saveLocalPreferences = () =>
    localStorage.setItem(
      'renderloft-settings',
      JSON.stringify({
        defaultAspectRatio: defaultAspectRatio.value,
        defaultResolution: defaultResolution.value,
        autoRefreshApps: autoRefreshApps.value
      })
    )

  // 设置只保存本机偏好（下载目录等）；机密与小配置都归各自的连接（连接页）。
  const saveSettings = async (downloadDirValue = downloadDir.value) => {
    const persisted = await v2.put<SettingsSnapshot>('/api/v1/settings', {
      downloadDir: downloadDirValue
    })
    downloadDir.value = persisted.downloadDir
    saveLocalPreferences()
  }

  const loadSettings = async (force = false) => {
    if (!force && isLoaded.value) return
    if (loadPromise) return loadPromise
    isLoading.value = true
    loadPromise = (async () => {
      const saved = localStorage.getItem('renderloft-settings')
      if (saved) {
        const settings = JSON.parse(saved)
        defaultAspectRatio.value = settings.defaultAspectRatio || '1:1'
        defaultResolution.value = settings.defaultResolution || '1k'
        autoRefreshApps.value = settings.autoRefreshApps !== false
      }
      try {
        const result = await v2.get<SettingsSnapshot>('/api/v1/settings')
        downloadDir.value = result.downloadDir || ''
        loadError.value = ''
        isLoaded.value = true
      } catch (error) {
        isLoaded.value = false
        loadError.value = error instanceof Error ? error.message : String(error)
        throw error
      } finally {
        isLoading.value = false
        loadPromise = undefined
      }
    })()
    return loadPromise
  }

  const setAutoRefreshApps = (value: boolean) => {
    autoRefreshApps.value = value
    saveLocalPreferences()
  }

  return {
    downloadDir,
    defaultAspectRatio,
    defaultResolution,
    autoRefreshApps,
    isLoaded,
    isLoading,
    loadError,
    setAutoRefreshApps,
    saveSettings,
    loadSettings
  }
})
