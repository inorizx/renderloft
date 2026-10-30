import { v2 } from '@/shared/api/v2'
import { useLocale } from '@/shared/composables/useLocale'
import { useAppStore } from '@/shared/stores/app'

/**
 * 下载**一个操作产出的全部产物**。
 *
 * ⚠️ 它替换掉了一个说谎的版本：那个只取 `previewableArtifacts(id)[0]` —— **只下第一张**，
 * 对其余的只字不提。一批 100 张图，用户点一下就以为拿到了全部。
 *
 * 落盘由 daemon 做（`v2.saveArtifact`），所以这里只做两件本地的事：逐个调、把结果
 * **汇总报出来**。失败必须能被看见——"静默少几张"是最坏的一种收尾。
 *
 * 产物按 run 列（按 runId 查的端点不分页，一次给全），所以逐 run 取。
 *
 * 为什么抽到 shared：任务中心与任务历史页各有一份下载入口，而这个缺陷在任务中心
 * 修过一次、历史页那份没跟上。同一件事只能有一份实现。
 */
export async function downloadOperationArtifacts(runIds: number[]): Promise<void> {
  const { t } = useLocale()
  const appStore = useAppStore()
  const usable = runIds.filter((runId) => runId > 0)
  let artifactIds: number[] = []
  try {
    const pages = await Promise.all(usable.map((runId) => v2.listArtifacts(runId)))
    artifactIds = pages.flatMap((page) => page.items).map((item) => item.artifactId)
  } catch (error) {
    appStore.showNotification(
      'error',
      t('task.downloadFailedDetail', {
        message: error instanceof Error ? error.message : t('task.downloadFailed')
      })
    )
    return
  }
  if (!artifactIds.length) {
    appStore.showNotification('info', t('task.downloadNothing'))
    return
  }
  appStore.showNotification('info', t('task.downloadingMany', { count: artifactIds.length }))
  let done = 0
  let directory = ''
  let firstError = ''
  for (const artifactId of artifactIds) {
    try {
      const saved = await v2.saveArtifact(artifactId)
      done += 1
      if (!directory) directory = saved.directory
    } catch (error) {
      if (!firstError) {
        firstError = error instanceof Error ? error.message : t('task.downloadFailed')
      }
    }
  }
  const failed = artifactIds.length - done
  if (!failed) {
    appStore.showNotification(
      'success',
      t('task.downloadedMany', { count: done, path: directory }),
      {
        label: t('task.showInFolder'),
        run: () => void v2.openDownloadDir().catch(() => undefined)
      }
    )
    return
  }
  appStore.showNotification(
    done > 0 ? 'warning' : 'error',
    t('task.downloadPartial', { done, failed, message: firstError })
  )
}
