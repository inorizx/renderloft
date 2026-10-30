import { computed, ref } from 'vue'

export type AppLocale = 'zh-CN' | 'en-US'

const LOCALE_STORAGE_KEY = 'renderloft-locale'
const isClient = typeof window !== 'undefined'
const savedLocale = isClient ? localStorage.getItem(LOCALE_STORAGE_KEY) : null
const systemLocale =
  isClient && navigator.language.toLowerCase().startsWith('zh') ? 'zh-CN' : 'en-US'
const locale = ref<AppLocale>(
  savedLocale === 'zh-CN' || savedLocale === 'en-US' ? savedLocale : systemLocale
)
if (isClient) document.documentElement.lang = locale.value

const messages = {
  'zh-CN': {
    'nav.workspace': '工作台',
    // 与页面标题（`library.title` = 「应用库」）统一：此前导航叫「资源库」、标题叫「应用库」，
    // 同一个页面两个名字。而页面里数的是「共 N 个应用」，所以「应用库」是对的。
    'nav.library': '应用库',
    'nav.tasks': '任务',
    'nav.connections': '连接',
    'nav.settings': '服务配置',
    'shell.localWorkspace': '本地 AI 工作台',
    'shell.workspaceGroup': '工作区',
    'shell.systemGroup': '系统',
    'shell.mainNav': '主导航',
    'shell.mobileNav': '移动端主导航',
    'shell.connected': '服务已连接',
    'shell.configure': '配置服务',
    'shell.switchDark': '切换至深色主题',
    'shell.switchLight': '切换至浅色主题',
    'shell.runningTasks': '{count} 个任务进行中',
    // 「RH 币」与「钱包」两条已删：那是**平台账号**的事实，不是工作台的。
    // 侧栏那张卡说的是「连接」，里面却只对 RunningHub 成立、还把平台的钱写成"钱包"
    // （看起来像 RenderLoft 收钱）。面的家改到连接页的**凭证那一行**，见 connectionFacet.ts。
    'shell.connectionUnknown': '无法读取连接状态',
    'shell.connections': '连接',
    'shell.noConnections': '未配置连接，点击管理',
    'home.eyebrow': '工作区',
    'home.title': '工作台',
    'home.description': '集中处理资产、执行连接与近期任务。',
    'home.addApp': '添加应用',
    'home.allApps': '全部应用',
    'home.startWithApp': '从一个应用 ID 开始',
    'home.startWithAppHint': '添加后会在这里显示最近使用的应用。',
    'home.recentTasks': '最近任务',
    'home.goToApps': '前往应用库',
    'home.quickStart': '快速开始',
    'home.setupEyebrow': '首次使用',
    'home.setupHint': '三步开始生成',
    'home.setupTitle': '先配置一次，之后直接使用应用',
    'home.setupDescription':
      'RenderLoft 会把平台应用变成简单的参数表单。先完成连接，再导入一个应用即可开始。',
    'home.setupSkip': '稍后再做',
    'home.setupConnectionTitle': '配置平台连接',
    'home.setupConnectionDescription': '添加 RunningHub API Key，或配置本地 ComfyUI / 模型 API。',
    'home.setupConfigure': '去配置连接',
    'home.setupViewConnection': '查看连接',
    'home.setupAppTitle': '导入一个应用',
    'home.setupAppDescription':
      '从 RunningHub 应用链接或 ID 开始，或粘贴 ComfyUI「保存(API 格式)」的 JSON；导入后给字段写中文备注名。',
    'home.setupImport': '导入应用',
    'home.setupViewApps': '查看应用',
    'home.setupRunTitle': '运行并查看结果',
    'home.setupRunDescription': '填写必要参数，提交后在任务中心查看进度和结果。',
    'home.setupOpenApp': '打开应用库',
    'home.setupViewTasks': '查看任务',
    'home.quickStartRunningHub': 'RunningHub 应用',
    'home.quickStartRunningHubHint': '添加并运行云端应用。',
    'home.quickStartComfyui': 'ComfyUI 工作流',
    'home.quickStartComfyuiHint':
      'ComfyUI 连接可用于运行你自己的工作流：粘贴「保存(API 格式)」的 JSON 即可导入，并给字段写中文备注名。',
    'home.quickStartOpenAiImage': 'OpenAI 图像',
    'home.quickStartOpenAiImageHint': '保存图像模板并通过已配置的图像连接运行。',
    'home.openLibrary': '打开应用库',
    'home.loadingWorkspace': '正在加载工作台…',
    'home.loadWorkspaceFailed': '无法加载部分工作台数据，请重试。',
    'home.recentAssets': '最近使用的应用',
    'connection.runningHub': 'RunningHub',
    'connection.comfyui': 'ComfyUI',
    'connection.openAiImage': 'OpenAI 图像',
    'connection.connected': '已连接',
    'connection.notConfigured': '未配置',
    'connection.unknown': '未知',
    'connection.configure': '配置连接',
    'status.success': '已完成',
    'status.running': '执行中',
    'status.failed': '失败',
    'status.queued': '排队中',
    'status.partial': '部分完成',
    'status.cancelled': '已取消',
    'status.submitting': '正在提交',
    'status.preparing': '准备中',
    'status.submissionUncertain': '提交待确认',
    'status.accepted': '已受理',
    'status.retryWait': '等待重试',
    'history.all': '全部',
    'history.active': '进行中',
    'task.liveActivity': '实时任务',
    'task.eyebrow': '实时动态',
    'task.expand': '展开实时任务',
    'task.collapse': '收起实时任务',
    'task.allHistory': '全部历史',
    'task.refresh': '立即刷新任务状态',
    'task.refreshing': '正在刷新任务状态',
    'task.close': '关闭实时任务',
    'task.autoSync': '自动同步中',
    'task.synced': '已同步',
    'task.syncError': '同步暂时异常',
    'task.activeCount': '进行中 {count}',
    'task.updatedAt': '更新于 {time}',
    'task.empty': '提交任务后，进度和结果会持续显示在这里',
    'task.runs': '个 Run',
    // 用量只**转述平台报的数**（耗时用 task.duration* 那组既有文案）。
    //
    // 「币」是**平台的计价单位**，所以词条按单位命名、不按用途命名：余额（连接页的凭证行）
    // 与任务用量（`runUsage.ts`）说的是同一个单位，两处必须同步改，留两个键迟早打架。
    'unit.coins': '{value} 币',
    'task.done': '已完成',
    'task.openDetails': '打开任务详情',
    'task.cancel': '取消任务',
    'task.cancelBatch': '取消批次',
    'task.cancelling': '正在取消…',
    'task.unknownTime': '时间未知',
    'task.justNow': '刚刚',
    'task.operationLabel': '操作 #{id}',
    'task.noActive': '暂无实时任务',
    'task.noBatch': '暂无批次记录',
    'task.reload': '刷新',
    'task.clearFinished': '清空已结束',
    'task.clearing': '清空中…',
    'task.rerun': '重新运行',
    'task.deleteHistory': '删除历史',
    'task.confirmDelete': '确认删除任务 #{id} 的历史与产物？产物删除后无法恢复。',
    'task.confirmClear': '确认清空 {count} 个已结束任务？它们的记录与产物会一并删除，且无法恢复。',
    'task.rerunFailedDetail': '重新运行失败：{message}',
    'task.rerunFailed': '重新运行失败，请稍后重试。',
    'task.deleteFailedDetail': '删除失败：{message}',
    'task.deleteFailed': '删除失败，请稍后重试。',
    'task.clearFailedDetail': '清空失败：{message}',
    'task.clearFailed': '清空失败，请稍后重试。',
    'task.downloadFailedDetail': '下载失败：{message}',
    'task.downloadFailed': '下载失败，请稍后重试。',
    // 成功要报**完整路径**：桌面外壳（WebView2）不弹下载提示，不报用户就以为"点了没反应"。
    'task.downloaded': '已下载 {name} → {path}',
    'task.showInFolder': '在文件夹中显示',
    // 一个操作（一批）可能有几十上百个产物：逐个下载必须说清"下了几个、失败几个"。
    // 静默少几张是最坏的收尾——与 CLI 落点冲突策略同一个口径。
    'task.downloadAll': '下载全部产物',
    'task.downloadingMany': '正在下载 {count} 个文件…',
    'task.downloadedMany': '已下载 {count} 个文件 → {path}',
    'task.downloadPartial': '成功 {done} 个 · 失败 {failed} 个：{message}',
    'task.downloadNothing': '这一批没有可下载的产物',

    // ---- 任务中心（TaskCenter）----
    'common.close': '关闭',
    'task.appIdentity': '应用 · {name}',
    'task.batchRunning': '执行中',
    'task.batchPartial': '部分失败',
    'task.batchSucceeded': '已完成',
    'task.batchFailed': '失败',
    'task.batchInProgress': '批次执行中',
    'task.batchMeta': '批次 · {count} 条任务',
    'task.batchSummaryRuns': '完成 {completed} · 进行中 {running} · 未完成 {failed}',
    'task.batchSummaryDone': '完成 {count} · 进行中 0 · 未完成 0',
    'task.batchSummaryPartial': '部分完成 · 共 {count} 条',
    'task.batchSummaryFailed': '全部失败 · 共 {count} 条',
    'task.batchSummaryRunning': '进行中 · 共 {count} 条',
    'task.mediaGenerating': '正在生成',
    'task.mediaWaitingResource': '等待资源',
    'task.mediaWaitingRetry': '等待重试',
    'task.mediaSubmitting': '正在提交',
    'task.mediaVerifying': '正在核对',
    'task.mediaTracking': '正在追踪',
    'task.trackQueued': '服务端已接收，正在排队',
    'task.trackSubmitted': '已提交，等待服务确认',
    'task.trackUncertain': '提交结果未知，正在核对',
    'task.trackGenerating': '生成中，结果即将出现',
    'task.trackRetryWait': '等待自动重试',
    'task.trackAuto': '自动追踪中',
    'task.durationSeconds': '{n} 秒',
    'task.durationMinutes': '{n} 分',
    'task.durationMinSec': '{m} 分 {s} 秒',
    'task.durationHourMin': '{h} 小时 {m} 分',
    'task.notifyTitle': '任务完成',
    'task.reconcileInactive': '等待调度',
    'task.reconcileActive': '对账中',
    'task.reconcilePaused': '已暂停',
    'task.reconcileExhausted': '已停止',
    'task.unreadCount': '未读任务数',
    'task.viewTabs': '任务中心视图',
    'task.tabTasks': '任务',
    'task.tabBatches': '批次',
    'task.viewBatch': '查看批次',
    'task.taskMeta': '任务 · {id}',
    'task.recentTasks': '最近任务',
    'task.viewDetail': '查看任务详情',
    'task.previewNamed': '预览 {name}',
    'task.resultCount': '{count} 项',
    'task.download': '下载',
    'task.details': '详情',
    'task.recentBatches': '最近批次',
    'task.olderBatches': '历史批次',
    'task.taskCount': '{count} 条任务',
    'task.viewBatchDetail': '查看批次详情',
    'task.detailTitle': '{name} · 任务详情',
    'task.batchDetailTitle': '{name} · 批次详情',
    'task.fieldApp': '应用',
    'task.fieldStatus': '状态',
    'task.fieldTaskId': '任务编号',
    'task.fieldStartedAt': '开始时间',
    'task.fieldDuration': '运行时长',
    'task.fieldResults': '生成结果',
    'task.fieldBatchStatus': '批次状态',
    'task.fieldTaskCount': '任务数量',
    'task.fieldRunStatus': '任务状态',
    'task.queuedAhead': '前面还有 {count} 条',
    'task.noRunsRecorded': '此操作尚未记录 run。',
    'task.failureReasons': '失败原因',
    // 「它为什么没在动」：排队 / 退避重试 / 提交结果不明，状态**都显示「进行中」**，
    // 所以这几句是"它其实没在动"的唯一解释（原因码 → 文案，见 shared/lib/attention.ts）。
    'attention.title': '它其实没在动',
    'attention.uncertain': '等确认（提交结果不明）',
    'attention.retryBackoff': '退避重试中',
    'attention.submitting': '正在提交',
    'attention.queued': '排队中',
    'attention.deadline': '{time} 判失败',
    'attention.noDeadline': '没有死线（会一直等下去）',
    'attention.whyUncertain':
      '提交时连接断了或超时，平台到底收没收到不知道，所以不会自动重投。到死线仍确认不了就判失败。',
    'attention.whyUncertainNoDeadline':
      '提交时连接断了或超时，平台到底收没收到不知道，正在对账确认。',
    'attention.whyRetryBackoff':
      '上一次提交没成功（平台拒了，或连接被拒这类"字节没出去"的失败），正在自动重试——不需要你做什么。',
    'attention.whySubmitting': '正在提交：进程若在这里中断，超过恢复窗口会自动转成「等确认」。',
    'attention.whyQueued':
      '排队中：这条连接同时只跑一个（与平台账号的实际并发一致），前面还有没结束的任务。',
    'task.confirmRetry': '重试会新建一个任务（沿用原任务的参数），并且会再计费一次。要继续吗？',
    // 批量里**只重试失败的那几条**（愿景点名的批量能力）：一次点按会建 N 个任务，所以
    // 确认框必须把"几个任务、几次计费"说成数字 —— 含糊的确认框在花钱的动作上等于没确认。
    'task.retryFailedRuns': '只重试失败的 {count} 条',
    'task.retryingFailed': '正在新建任务…',
    'task.confirmRetryFailed':
      '会为这 {count} 条失败各新建一个任务（沿用它们原来的参数），并再计费 {count} 次。要继续吗？',
    'task.retryFailed': '重试失败，请稍后重试。',
    'task.retryFailedDetail': '重试失败：{message}',
    // 「用这套参数再跑一次（可改）」：打开那个应用的表单并预填，**不立刻提交**。
    // 与「重新运行」（立刻再投一次）刻意分成两个入口，所以文案要能让人分清。
    'task.reuseParams': '用这套参数再跑一次（可改）',
    'task.confirmReuseOverwrite':
      '这个应用的表单里已经有内容了，用这次任务的参数覆盖它吗？（覆盖后原内容不再保留）',
    'task.reuseFailed': '打不开那个应用的表单，请稍后重试。',
    'task.reuseFailedDetail': '打不开那个应用的表单：{message}',
    // 素材名字取不到时的**如实**说法（不编一个文件名）。
    'task.assetFallbackName': '素材 #{id}',
    // 表单草稿会**活过重启**（见 `runner-sessions` 的 drafts），所以要有出口与说明。
    'runner.clearForm': '清空表单',
    'runner.clearFormTitle': '清空这张表？',
    'runner.clearFormDescription':
      '会把当前填的内容全部清掉（含批量行草稿）。清掉后不再保留——注意：已经提交过的任务不受影响，它的参数在任务里还能「用这套参数再跑一次」。',

    'common.create': '创建',
    'common.creating': '创建中…',

    // ---- 共享组件 + Home / Settings（收尾批次）----
    'common.loading': '加载中',
    'common.dialog': '对话框',
    'common.closeDialog': '关闭对话框',
    'common.confirm': '确认',
    'common.processing': '处理中…',
    'home.quickStartEyebrow': '接入',
    'home.recentAppsEyebrow': '资产',
    'field.required': '此项为必填',
    'field.mustBeInteger': '需为整数',
    'field.mustBeNumber': '需为数字',
    'field.requiredMark': '（必填）',
    'field.selectedFile': '已选文件',
    'field.uploading': '上传中…',
    'field.uploaded': '已上传',
    'field.remove': '移除',
    'field.clickToUpload': '点击上传',
    'field.dragHint': '或拖拽文件到此处',
    'shell.closeTab': '关闭 {title}',
    'shell.connUnreadable': '无法读取',
    'shell.connDisabled': '已停用',
    'shell.connInvalid': '配置无效',
    'shell.connNotReady': '未就绪',
    'shell.runApp': '运行应用',
    'shell.notProvided': '未提供',
    'previewDialog.downloadResult': '下载结果 {n}',
    'previewDialog.closePreview': '关闭预览',
    'previewDialog.prevResult': '上一张结果',
    'previewDialog.nextResult': '下一张结果',
    'previewDialog.resultAlt': '{title} 结果 {n}',
    'previewDialog.noText': '无可预览文本',
    'previewDialog.resultLabel': '结果 {n}',
    'previewDialog.download': '下载',
    'api.timeout': '请求超时（{seconds}s）',
    'api.nonJsonResponse': '服务返回了非 JSON 响应（HTTP {status}）：{excerpt}',
    'api.unknownError': '未知错误',
    'api.downloadFailedWithDetail': '下载失败（HTTP {status}）：{excerpt}',
    'api.downloadFailed': '下载失败（HTTP {status}）',
    'settings.eyebrow': '系统',
    'settings.title': '服务配置',
    'settings.description': '连接与凭据都在「连接」页管理；这里只管界面、主题与本地偏好。',
    'settings.language': '界面语言',
    'settings.languageHint': '语言仅影响 RenderLoft 界面，不影响 RunningHub 服务区域或应用参数。',
    'settings.languageZh': '简体中文',
    'settings.languageEn': 'English',
    'settings.theme': '主题',
    'settings.save': '保存配置',
    'settings.saving': '正在保存',
    'settings.diagnostics': '诊断信息',
    'settings.diagnosticsHint':
      '仅包含版本、系统、服务区域和 API Key 是否已配置，不包含密钥、提示词或任务内容。',
    'settings.copyDiagnostics': '复制诊断信息',
    'settings.copyingDiagnostics': '正在复制',
    'settings.downloads': '下载',
    'settings.saveSuccess': '设置已保存',
    'settings.downloadDir': '下载目录',
    'settings.downloadDirDescription':
      '桌面版会将结果保存到这里；浏览器访问时由浏览器下载设置决定。',
    'settings.downloadDirPlaceholder': '例如：D:\\RenderLoft\\Downloads',
    'settings.historyRetention': '历史保留（天）',
    'settings.historyRetentionDescription':
      '已结束的任务超过这个天数会被清理（连带产物记录与事件）；填 0 = 永久保留。',
    'settings.historyRetentionPlaceholder': '默认 30',
    'settings.openDir': '打开目录',
    'settings.savedConfigHint': '下载目录已保存。',
    'settings.openDownloadDirFailed': '打开下载目录失败：{message}',
    'settings.diagnosticsCopied': '诊断信息已复制',
    'settings.copyDiagnosticsFailed': '复制诊断信息失败：{message}',
    'settings.diagnosticsTitle': 'RenderLoft 诊断信息',
    'settings.version': '版本',
    'settings.system': '系统',
    'settings.serviceRegion': '服务区域',
    'settings.serviceHost': '服务主机',
    'settings.apiKeyConfigured': 'API Key 已配置',
    'settings.outboundProxy': '出网代理',
    /*
     * 运行参数：这一块的存在理由只有一个——**出问题时用户能自己改**。
     * 所以每一条都必须回答三件事：叫什么、现在生效的值是什么、**改哪个变量**。
     * 只说"值是什么"等于没帮上忙（这些值本来就是 daemon 自己算的）。
     */
    'settings.runtimeParams': '运行参数',
    'settings.runtimeParamsHint':
      '这些是 daemon 当前生效的值（来自环境变量或程序位置）。出问题时按「改哪里」那一列设置，改完重启 daemon。',
    'settings.runtimeWhere': '改哪里',
    'settings.runtimeBind': '监听地址',
    'settings.runtimeAllowedHosts': 'Host 白名单',
    'settings.runtimeAllowedHostsEmpty': '（未设置：只接受本机回环）',
    'settings.runtimeUiOrigin': '界面来源',
    'settings.runtimeUiOriginEmpty': '（未设置：使用程序自带的界面）',
    'settings.runtimeDistDir': '界面资源目录',
    'settings.runtimeOutboundProxyEmpty': '（未设置：直连）',
    'settings.defaultRequestTimeout': '默认请求超时',
    'settings.defaultTrackingDeadline': '默认跟踪上限',
    'settings.secondsValue': '{value} 秒',
    'settings.defaultsHint': '可在「连接」页的高级项里按连接改（留空即用这两个默认值）。',
    'settings.logs': '日志',
    'settings.logsHint':
      '出问题时把这个目录整个交给你的 agent（或贴给别人）——里面是 daemon 与外壳的运行日志（不含密钥与提示词）。',
    'settings.logsMissing': '（这个目录里现在没有日志文件：daemon 的输出可能被重定向到别处了）',
    'settings.logsTimezone': '日志里的时间是 UTC（本机是 {offset}）——末尾的 Z 就是"这是 UTC"。',
    'settings.logsJustNow': '刚刚还在写',
    'settings.logsMinutesAgo': '{n} 分钟前',
    'settings.logsHoursAgo': '{n} 小时前',
    'settings.logsUnknownTime': '写入时间未知',
    'settings.loadFailed': '读取配置失败：{message}',
    'settings.connectionsLoadFailed': '连接加载失败：{message}',
    'settings.saveFailed': '保存失败：{message}',
    'settings.updateApps': '应用更新',
    'settings.updateAppsHint': '打开“应用库”时检查已保存应用的参数更新。',
    'library.assetTypeAll': '全部类型',
    // 标签说的是「**这是什么**」，不是「在哪跑」——后者由卡片上的连接名回答（`connectionLabel`）。
    // 种类由后端从**绑定的形状**派生（`webappId` / `workflowId` / 内联 `workflow`），
    // 所以「RH 应用」与「RH 工作流」分得开。
    // ⚠️ 2026-10-11 曾写成"只照实说驱动名"，理由是"我们判不出应用与工作流"——那条判断是错的：
    // 驱动确实判不出，绑定判得出（见 `transports/http/handlers.rs::recipe_kind`）。
    'library.kindRhApp': 'RH 应用',
    'library.kindRhWorkflow': 'RH 工作流',
    'library.kindComfyWorkflow': 'ComfyUI 工作流',
    'library.kindOpenAiImage': 'OpenAI 图像',
    'library.kindOther': '其他',
    'library.goToLibrary': '前往应用库',
    // 应用库的导出 / 导入：只搬应用本身（名字、分组、备注名、控件、平台类型），
    // 不含任务历史、产物、连接与密钥——所以文案里不说"备份"（那暗示整机/定期）。
    'library.exportLibrary': '导出应用库',
    'library.importLibrary': '导入应用库',
    'library.exporting': '导出中…',
    'library.exportEmpty': '库里还没有应用，没什么可导出的。',
    'library.exportDone': '已导出 {count} 个应用到 {path}',
    'library.importDone': '导入 {imported} 个 · 跳过 {skipped} 个 · 失败 {failed} 个',
    'library.importNothing': '这个文件里没有应用。',
    'common.retry': '重试',
    'history.eyebrow': '任务归档',
    'history.title': '任务历史',
    /*
     * 不写平台名：这一页装的是**所有**驱动生成的任务（RunningHub、ComfyUI、模型 API），
     * 而旧文案写死了 RunningHub —— 页面上明明摆着 ComfyUI 的卡片，标题却说只整理
     * RunningHub 的，图文自相矛盾。写"所有生成任务"才是这页真正在做的事。
     */
    'history.description': '集中浏览、筛选和整理所有生成任务。',
    'history.stats': '任务统计',
    'history.search': '搜索任务 ID 或提示词',
    'history.archivedCount': '{count} 个归档任务',
    'history.loadMore': '加载更多（还有 {count} 条）',
    'history.results': '结果与任务',
    'history.sorted': '按创建时间排序',
    'history.resultCount': '{count} 个结果',
    'history.download': '下载首个结果到本地',
    'history.noMatches': '没有符合条件的任务',
    'history.empty': '还没有任务历史',
    'history.noMatchesHint': '调整搜索词或状态筛选，查看其他归档任务。',
    'history.emptyHint': '从“应用库”运行一个应用后，生成任务和结果会保存在这里。',

    // ---- 通用词（跨页面复用；原 settings.cancel / settings.yes / settings.no 已收口到这里）----
    'common.cancel': '取消',
    'common.yes': '是',
    'common.no': '否',
    'common.listSeparator': '、',
    'common.all': '全部',
    'common.eyebrowApp': 'AI 应用',
    // 应用库那一页**装着**应用与工作流两类 ⇒ 不能用「AI 应用」当它的抬头（那会把工作流盖掉）。
    'common.eyebrowLibrary': '应用与工作流',
    // 认不出它是哪一种时用的**伞词**（我们自己的概念）。平台自己的词优先：
    // RH 应用 / RH 工作流 / ComfyUI 工作流 —— 那是三种不同的东西，别用一个词盖掉。
    'common.eyebrowRecipe': '应用',

    // ---- 应用运行器（V2Runner）----
    'status.pending': '待提交',
    'runner.uploadTooLarge': '文件不能超过 100 MB',
    'runner.revisionExpired': '应用版本已失效，请返回应用库后重试。',
    'runner.rowTag': '第 {n} 行的',
    'runner.invalidNumbers': '请修正{row}数值字段：{fields}',
    'runner.missingRequired': '请先填写{row}必填项：{fields}',
    'runner.submitted': '任务 #{id} 已提交，进度请在右侧「实时任务」查看。',
    /*
     * 运行页那行"字段名可以改"的提示：只在**看得出还没整理过**的时候显示
     * （撞名，或一个备注名都没写过，见 `V2Runner.vue` 的 `fieldNamingHint`）。
     * 措辞按中性书面：说清"名字从哪来、你能做什么"，不讲我们怎么合并 schema。
     */
    'runner.fieldNamingHint': '字段名来自平台，可按用途改名。',
    'runner.editFields': '编辑字段',
    'runner.backToLibrary': '← 返回应用库',
    'runner.backToLibraryPlain': '返回应用库',
    'runner.openFailedTitle': '无法打开应用',
    'runner.openFailedDescription': '应用不存在或加载失败，请返回应用库后重试。',
    'runner.paramsTitle': '填写参数',
    'runner.paramsHint': '先完成必要输入，再选择一次生成或批量生成。',
    'runner.modeGroup': '运行模式',
    'runner.modeSingle': '单次运行',
    'runner.modeBatch': '批量运行',
    'runner.batchUniformTitle': '本批次统一设置',
    'runner.batchUniformHint':
      '先在这里填写一次，所有任务行都会使用这些值；需要逐行不同的字段，勾选「单独填写」后在下方表格设置。',
    'runner.fieldToggle': '单独填写',
    'runner.noParamsTitle': '无参数',
    'runner.noParamsDescription': '该 Recipe 不需要输入。',
    'runner.batchSummary': '批次摘要',
    'runner.totalRows': '共 {count} 行',
    'runner.rowsPanelTitle': '逐条填写',
    'runner.rowsPanelHint': '每一行都会创建一个独立任务。',
    'runner.rowsPanelHintNoVariable':
      '每一行都会创建一个独立任务；勾选上方字段的「单独填写」后可在此逐行设置。',
    'runner.addRow': '新增一条任务',
    'runner.addRows': '一次添加多条',
    'runner.clearPending': '清空待处理',
    'runner.colStatus': '状态',
    'runner.colActions': '操作',
    'runner.replace': '替换',
    'runner.choose': '选择',
    'runner.remove': '移除',
    'runner.uploading': '上传中…',
    'runner.dropHint': '选择或拖拽文件到此处',
    'runner.unifiedFile': '统一：{name}',
    'runner.duplicate': '复制',
    'runner.delete': '删除',
    'runner.willCreate': '将创建 {count} 项任务',
    'runner.willCreateOne': '将创建 1 项任务',
    // 提交那一行报的是**身份**（打到哪个账号），不是余额——理由见 V2Runner 的 decisionLine。
    'runner.boundConnection': '连接 {name}',
    'runner.submitting': '提交中…',
    'runner.startBatch': '开始批量生成',
    'runner.start': '开始生成',
    'runner.dialogAddTitle': '一次添加多条任务',
    'runner.dialogAddDescription': '将以当前统一参数创建新的待提交任务行，可稍后在表格中逐行调整。',
    'runner.namePrefix': '命名',
    'runner.namePrefixPlaceholder': '如 SKU135（产物文件名）',
    'runner.colName': '名称',
    'runner.rowNamePlaceholder': '留空＝前缀-序号',
    'runner.duplicateNames': '名称重复：{names}',
    'runner.countLabel': '任务数量',
    'runner.addCount': '添加 {count} 条',
    'runner.dialogClearTitle': '清空待处理任务？',
    'runner.dialogClearDescription': '将删除 {count} 条尚未提交的任务行，此操作不可撤销。',
    'runner.blankCellsTitle': '有单独填写的格子是空的',
    'runner.recipeFallback': '应用 #{id}',
    'runner.blankCellsDescription':
      '下面这些格子为空。单独填写的字段不会回退到统一参数，空着就会按空值提交。',
    'runner.blankCellsItem': '{field}：第 {rows} 行',
    'runner.blankCellsConfirm': '仍要提交',
    'runner.preSubmitTitle': '提交前确认一下',
    'runner.emptyMediaTitle': '没有选任何图片',
    'runner.emptyMediaDescription':
      '这次提交没有选任何图片。工作流可能失败，也可能用它自带的默认图——两种都会照常计费。',
    'runner.resubmitTitle': '这批已经提交过',
    'runner.resubmitDescription':
      '这 {total} 行上次已经提交过（其中 {done} 行已完成）。再点一次会把全部行重新生成，并再计费一次。只想重跑失败的几条，请用任务页的「只重试失败的」。',
    'runner.resubmitConfirm': '全部重新提交',

    // ---- 应用库（V2Recipes）----
    'library.title': '应用库',
    /*
     * ⚠️ 这一页**同时装着**平台上的「AI 应用」和「工作流」（还有 ComfyUI 的工作流）——
     * 所以这里的伞词只能说"应用与工作流"，不能说"应用"：后者会把工作流盖掉，
     * 而用户恰恰要靠"这是我那个 RH 应用"还是"我导进来的那个工作流"来认自己的东西
     *（每一张卡片上都有如实的类型标签，运行页左上那行也按类型说）。
     */
    'library.description': '把平台上的应用与工作流变成简单的参数表单，单次或批量运行。',
    'library.noConnection': '未指定连接',
    'library.driverRunningHub': 'RunningHub（云端 AI 应用 / 工作流）',
    'library.driverComfyui': 'ComfyUI（本地 / 远程工作流）',
    'library.driverOpenAiImage': 'OpenAI 图片（文生图接口）',
    'library.driverAsyncTask': '通用异步任务（云端）',
    'library.errNoConnection': '请选择默认 Connection',
    'library.errInvalidSchema': 'inputSchema 不是有效 JSON',
    'library.errSelectRhConnection': '请选择 RunningHub Connection',
    'library.errEnterRhSource': '请输入 RunningHub 链接或 ID（AI 应用与工作流都行）',
    'library.comfyPromptPlaceholder':
      '从 ComfyUI 工作流里选择「保存(API 格式)」导出的 JSON，\n或复制节点对象粘贴到这里。例如：',
    'library.errNoRhConnection': '还没有可用的 RunningHub 连接，请先到「连接」页配置 API Key',
    'library.errEnterRhWorkflow': '请粘贴 RunningHub 工作流链接或 ID',
    'library.errSelectComfyConnection': '请选择 ComfyUI Connection',
    'library.errEnterComfyPrompt': '请粘贴 ComfyUI API Prompt JSON，或用「从文件读入」',
    'library.errInvalidComfyJson': 'API Prompt 不是有效 JSON',
    'library.chooseJsonFile': '从文件读入',
    'library.widgetDefault': '控件：默认',
    'library.widgetText': '单行文本',
    'library.widgetTextarea': '多行文本',
    'library.widgetNumber': '数字',
    'library.widgetSwitch': '开关',
    'library.widgetDropdown': '下拉（自己填选项）',
    'library.widgetFile': '图片或文件',
    'library.widgetFor': '{name} 的控件',
    'library.widgetOptionsPlaceholder': '候选项，逗号分隔（例：4, 7, 10）',
    'library.widgetKeep': '控件：保持现状',
    'library.widgetYours': '你定的',
    'library.widgetFromEngine': '引擎给的',
    'library.promptInputHint':
      '可以直接粘贴，也可以「从文件读入」——ComfyUI 里要选「保存(API 格式)」，不是「保存」。',
    'library.errComfyUiFormat':
      '这是 ComfyUI 的「工作流」格式（顶层有 nodes/links），不是「API 格式」——请在 ComfyUI 里改用「保存(API 格式)」重新导出一次。',
    'library.errComfyFileTooLarge':
      '这个文件超过 {mb} MB，多半不是 API Prompt——请确认导出的是「保存(API 格式)」。',
    'library.enrichmentOk':
      '已从 ComfyUI 读到 {known}/{used} 个节点类的定义：枚举字段是下拉、数字带范围。',
    'library.enrichmentMissing':
      '没读到 ComfyUI 的节点定义（{reason}），所以控件用的是通用类型：本该是下拉的会是文本框、数字没有范围。把 ComfyUI 起起来再导入一次即可。',
    'library.enrichmentUnknown': '原因不明',
    'library.errParseComfyFirst': '请先解析 API Prompt',
    'library.errSelectField': '请至少勾选一个字段',
    'library.addFromComfy': '从 ComfyUI API Prompt 添加',
    'library.configureRunningHub': '配置 RunningHub',
    'library.addFromRunningHub': '从 RunningHub 添加',
    'library.advancedCreate': '高级创建',
    'library.noticeTitle': '从公开应用开始，逐步建立你的工作区',
    'library.noticeBody':
      '可以通过 RunningHub 应用链接或 ID 添加，也可以从 ComfyUI 导出的 API Prompt 自动识别参数并添加。',
    'library.count': '共 {count} 个应用',
    'library.searchPlaceholder': '搜索名称、备注、分组或标签',
    'library.scopeFavorites': '仅收藏',
    'library.sortRecent': '按最近使用',
    'library.sortAdded': '按添加时间',
    'library.sortName': '按名称',
    'library.groupFilter': '分组',
    'library.allGroups': '全部分组',
    'library.noNote': '还没有添加说明。打开应用开始填写参数。',
    'library.recentlyUsed': '最近使用',
    'library.neverOpened': '尚未打开',
    'library.open': '打开',
    'library.favorite': '收藏 {name}',
    'library.unfavorite': '取消收藏 {name}',
    'library.editNamed': '编辑 {name}',
    'library.removeNamed': '移除 {name}',
    'library.restoreNamed': '恢复 {name}',
    'library.showRemoved': '显示已移除',
    'library.hideRemoved': '隐藏已移除',
    'library.removedSection': '已移除（{count}）',
    'library.removedAt': '移除于 {time}',
    'library.removedBadge': '已移除',
    'library.removedEmpty': '没有已移除的应用。移除的应用会先放在这里，随时可以恢复。',
    'library.removedLoading': '正在读取已移除的应用…',
    'library.restore': '恢复',
    'library.emptyFilteredTitle': '没有符合筛选条件的应用',
    'library.emptyTitle': '还没有 AI 应用',
    'library.emptyFilteredDescription': '试试放宽搜索或清除筛选条件。',
    'library.emptyDescription': '先配置一个平台连接，再从 RunningHub 添加一个你想使用的公开应用。',
    'library.clearFilters': '清除筛选',
    'library.addRhApp': '从 RunningHub 添加',
    'library.configureRhFirst': '先配置 RunningHub',
    'library.dialogAddRh': '从 RunningHub 添加',
    /*
     * 这个对话框的文案口径：**中性书面**。
     *
     * 它前三版分别是三种典型的写坏方式，值得留在这里当反例：
     *   1. 报告腔："是「AI 应用」还是「工作流」由平台判定 —— 字段、参数约束与默认值会自动导入，
     *      无需手写 Schema。" 它在**解释我们的设计、替自己辩护**，不是在告诉他下一步做什么。
     *      这是"人机味"的真正来源。
     *   2. 口语腔："粘进来就行 / 都能导 / 试一下"。亲切过头，放在配置类工具里显得不严谨。
     *   3. 公文腔："应…须…特此…"。又太硬，像在发通知。
     *
     * 逐句问两个问题：
     *   · 这句说的是"**你该干嘛**"，还是"**我们怎么做的**"？后者留在注释里。
     *   · 有没有语气词、破折号插入语、括号补充？有就删。
     *
     * 术语一律沿用平台自己的词（AI 应用 / 工作流 / 「打开AI应用」），不自造。
     *（「打开AI应用」是人眼能看见的按钮名，可以照抄；它页面里的结构不许程序去读 ——
     *  名字是抄的，结构是耦合。见 V2Recipes.vue 里那段同源注释。）
     */
    'library.dialogAddRhHint': '粘贴链接或 ID 即可，AI 应用与工作流均可导入。',
    'library.rhKindLabel': '导入方式',
    'library.rhKindAuto': '自动判断',
    'library.rhKindApp': 'AI 应用',
    'library.rhKindWorkflow': '工作流',
    'library.rhKindMismatch': '与链接识别结果不一致，将按所选方式导入。',
    'library.rhKindHintApp': '已识别为 AI 应用。如需自行填写提示词，请改用作品页链接。',
    'library.rhKindHintWorkflow':
      '已识别为工作流。如需作者配置的版本，请在页面点击「打开AI应用」后粘贴该链接。',
    'library.rhKindHintUnknown': '无法判断类型，将依次尝试两种方式。',
    'library.rhNamePlatformOwned': '名称由平台提供，导入后可修改。',
    'library.errRhChosenAppMissing':
      '该 ID 不是 AI 应用（平台返回：{message}）。如需应用版本，请在页面点击「打开AI应用」后粘贴该链接；或将导入方式改回自动。',
    'library.errRhChosenWorkflowMissing':
      '该 ID 不是工作流（平台返回：{message}）。作品页链接对应的是工作流；或将导入方式改回自动。',
    'library.selectPlaceholder': '请选择',
    'library.noRhConnectionHint':
      '还没有可用的 RunningHub 连接，请先到「连接」页配置 Base URL 和 API Key。',
    'library.incompleteConnectionsHidden': '有连接尚未完成配置，已从导入列表中隐藏。',
    'library.goConfigureConnection': '去配置连接 →',
    'library.appLinkOrId': '链接或 ID',
    'library.rhSourcePlaceholder': 'https://www.runninghub.cn/post/…',
    // 这个名字框只在**工作流**那条路显示（应用的名字由平台给），所以只说工作流那条的默认值。
    'library.rhNamePlaceholder': '留空则使用「RunningHub 工作流 + ID」',
    'library.errRhSourceNeither':
      '两种方式均未成功：按应用导入失败（{app}），按工作流导入失败（{workflow}）。',
    'library.importing': '导入中…',
    'library.import': '导入',
    'library.dialogAddComfy': '添加 ComfyUI API Prompt',
    'library.dialogSelectFields': '选择要暴露的参数',
    'library.dialogAddComfyDescription':
      '粘贴从 ComfyUI「保存(API 格式)」导出的 workflow JSON，下一步可以自行勾选要暴露成表单的字段并写上备注名。',
    'library.noComfyConnectionHint': '还没有可用的 ComfyUI 连接，请先到「连接」页配置 Base URL。',
    'library.appNameOptional': '应用名称（可选）',
    'library.appNamePlaceholder': '例如：我的文生图工作流',
    'library.fetchFromRhOptional': '从 RunningHub 工作流拉取（可选）',
    'library.workflowSourcePlaceholder': '粘贴工作流链接或 ID，如 https://www.runninghub.cn/post/…',
    'library.fetching': '拉取中…',
    'library.fetch': '拉取',
    'library.fetchHint':
      '只有当你想把这张图跑在「你自己的」ComfyUI 上时才走这里（比如本地那台，或「RunningHub 云端」这类预设）。只想跑在 RunningHub 上，用「从 RunningHub 添加」——那条路不需要 ComfyUI 连接。',
    'library.comfyFieldsFound':
      '共发现 {count} 个可参数化字段，已按常用程度预勾选。勾选要暴露给运行表单的字段，可在右侧写一句备注作为表单标签。',
    'library.node': '节点 {id}',
    'library.currentValue': '当前值：{value}',
    'library.file': '文件',
    'library.fieldLabelPlaceholder': '备注名，如：{name}',
    'library.positivePrompt': '正向提示词',
    'library.parsing': '解析中…',
    'library.nextSelectFields': '下一步：选择参数',
    'library.previousStep': '上一步',
    'library.importSelected': '导入（已选 {count} 项）',
    // `Recipe` 是领域模型名词（README 明说「代码中的 Recipe 是 AI 应用的领域模型名称」），
    // 不该出现在用户看得见的按钮上。
    'library.createTitle': '新建应用',
    'library.formName': '名称',
    'library.formNamePlaceholder': '例如：文本转图像',
    'library.formCapability': '能力',
    'library.formDriver': '执行驱动（Driver）',
    'library.formDefaultConnection': '默认 Connection',
    'library.formConnectionPlaceholder': '请选择（已按驱动筛选）',
    'library.connectionDetail': '{name}（{vendor}/{protocol}）',
    'library.noCompatibleConnections': '当前驱动下没有可用的连接，请先到「连接」页配置。',
    'library.formDescription': '描述',
    'library.optional': '可选',
    'library.editTitle': '编辑 {name}',
    'library.editHint': '备注与分组仅保存在本机，不影响远端应用。',
    /*
     * 刚导入完自动打开时的额外说明。这里要把"为什么现在"讲清楚：平台**没给语义**
     *（AI 应用的 `description` 实测就等于字段名，工作流只有 `3::cfg` 这种键），
     * 名字只能人补一次，而刚导完、还看着平台那一页时最该补。跳过也随意：不是必填项。
     *
     * ⚠️ 但**别把上面这段注释抄进界面**——它曾经被抄进去过，于是界面上出现了
     * 「AI 应用常是 text / batch_size，工作流则是 3::cfg、5::width 这种键（作者填过描述的
     * 应用不会弹这一步）」：两种来源的键长什么样、什么情况下不会弹这一步，都是**我们的**
     * 知识，不是站在这一步的人要做的决定。他只想知道两件事：为什么问我、填了有什么用。
     */
    'library.editHintAfterImport':
      '平台只提供字段名，不提供含义。填写名称后，运行表单与 AI 调用都会使用它；留空也可以，之后随时补充。',
    'library.formNote': '备注',
    'library.formNotePlaceholder': '记录用途、参数习惯等',
    'library.formGroup': '分组',
    'library.formFieldLabels': '字段备注',
    'library.formFieldLabelsHint':
      '平台描述通常与字段名相同，无法据此判断用途。填写的名称会同时用于运行表单与 AI 调用。',
    'library.formFieldLabelsEmpty': '这个应用没有可填参数。',
    'library.formFieldLabelsLoadFailed':
      '字段列表没读到（{message}）。这次保存不会改动字段备注；请稍后重试。',
    'library.formFieldLabelPlaceholder': '留空 = 没有备注',
    'library.formGroupPlaceholder': '例如：电商 / 工作流',
    'library.save': '保存',
    'library.removeTitle': '移除这个应用？',
    'library.removeDescription':
      '“{name}”会从应用库里移开，Agent 也不再能调用它。历史任务照旧可查，而且随时能在「显示已移除」里恢复——这里不会删掉任何记录，远端应用也不受影响。',
    'library.remove': '移除',

    // ---- 连接页（Connections）----
    'provider.runningHub.description':
      '云端生图 / 生视频（国内 .cn 与国际 .ai 数据独立，可各配一个）',
    'provider.runningHub.scope':
      '云端运行 AI 应用与工作流。配好一个连接，既能导入「AI 应用」，也能运行「RH 工作流」。',
    /*
     * ⚠️ 这里的路径必须与**平台上真实看到的入口**一致（2026-10-12 按平台实际入口更正过一次：
     * 原来的"个人中心 → API 管理 → 创建"是错的）。这是给用户照着点的说明书，
     * 写错就是让人在平台里瞎找——正是"边界"那条：我们的字要对得上他那边的界面。
     */
    'provider.runningHub.apiKeyHint':
      '在 runninghub.cn（或 .ai）登录后 → 打开「API」页面 → 进「控制台」→ 复制 API Key，粘到这里。',
    /*
     * 邀请码。**数字与措辞取自平台的分享文案**（2026-10-12 从平台页面抄录），规则由平台定、会变，
     * 所以末尾写明"以平台页面为准"。
     *
     * ⚠️ 只说**他能得到什么**。不写"填谁的码你都拿这么多"，更不写"奖励会支持到这个项目"——
     * 我们从中拿到什么与他的决定无关：写出来就把一行帮助变成了一次募捐，而"你是不是在拿我
     * 赚钱"那点疑虑本来不该由他来担。留"不填只有 100 币/天"是因为那是**他的**信息（填与不填
     * 差在哪），留"不填不影响任何功能"是因为那是**我们**的承诺（这行字不挡任何操作）。
     */
    'provider.runningHub.inviteHint':
      '还没有账号？注册时填邀请码 {code}（{site}，注册领 {coins} 币；不填只有 100 币/天）。不填不影响任何功能，规则以平台页面为准。',
    'connections.inviteSiteCn': '国内站',
    'connections.inviteSiteAi': '国际站',
    /*
     * ComfyUI 跑在 RunningHub 云端（地址带 /proxy）时，用的**就是 RunningHub 那把 Key**。
     *
     * 原来这两种情况共用一句"ComfyUI 不做鉴权，通常留空"，对 /proxy 那条是**误导**：
     * 照做就会留空，然后在运行时 401。所以按**地址**分开说。
     * ⚠️ 顺带记一条边界：**密钥不出凭据库** ⇒ 这里不可能有"从 RunningHub 连接填入"的按钮
     *（daemon 从不回传密钥）。真正的解法是"引用已有连接"——ComfyUI 这条连接选一条
     * RunningHub 连接，执行前借用它的密钥槽位（界面上的「API Key 来源」）。
     */
    'connections.apiKeyRunningHub': 'RunningHub API Key',
    // 「协议」与「平台」是两个维度：协议由连接类型回答，平台由地址回答。分开说，用户才不会
    // 看到「ComfyUI 连接 + runninghub.cn/proxy」就问"这不就是 RunningHub 吗"。
    'connections.protocolLabel': '协议',
    'connections.platformLabel': '平台',
    'connections.platformRunningHubCloud': 'RunningHub 云端',
    'connections.platformLocal': '本机',
    'connections.platformLan': '局域网',
    'connections.platformRemote': '远程 / 自建',
    // 输入框的显示/隐藏：只对**正在输入、还没提交**的值生效，不涉及回显。
    'connections.showApiKey': '显示',
    'connections.hideApiKey': '隐藏',
    'provider.comfyui.apiKeyHintRunningHub':
      '这条连接跑在 RunningHub 云端，要填的就是 RunningHub 的 API Key——你在 RunningHub 连接里填过的那把（在「API」页面 →「控制台」复制）。留空会在运行时失败。',
    'provider.runningHub.baseUrlHint': '默认使用平台官方地址，一般不需要改。',
    'provider.comfyui.description': '本地或远程工作流服务',
    'provider.comfyui.scope': '运行你自己的 ComfyUI 工作流。本地默认端口已预设，也可填远程地址。',
    /*
     * 只写**约束**，不写"你该怎么搭"。
     *
     * ⚠️ 这里一度写过「远程要用反向代理或隧道（Cloudflare Tunnel / ngrok / tailscale）」——
     * 那是在替用户设想他的网络拓扑：我们并不知道他把 ComfyUI 放在哪、怎么暴露的，
     * 那种清单既写不全也会过期，还把"我们的约束"和"他的方案"混成一锅。
     * 约束是稳定的（远程必须 https），方案是他的事。
     */
    'provider.comfyui.baseUrlHint': '本地为 http://127.0.0.1:8188。远程地址必须使用 https。',
    /*
     * ComfyUI 的 API Key 是**可选**的（`requiresApiKey: false`），但字段一直显示、
     * 又没有任何说明 —— 本地用户会以为"连本地也要 key？"（实测问过）。
     *
     * ⚠️ 只说**什么条件下需要**，不按"本地/远程"分类：ComfyUI 自己不做鉴权，
     * 本地远程一个样，按位置讲会让人以为"远程就要填"（云电脑就是反例）。
     * 也不列举"云电脑 / 显卡机 / 隧道"这类场景——那是替用户设想他的部署。
     */
    'provider.comfyui.apiKeyHint':
      'ComfyUI 不做鉴权，通常留空。只有地址里带 /proxy（RunningHub 云端），或你在它前面自己加了鉴权时才需要。',
    'provider.openaiImages.description': '兼容 OpenAI 生图接口',
    'provider.openaiImages.scope': '使用 OpenAI 或兼容接口做文生图（gpt-image 系列）。',
    'provider.openaiImages.apiKeyHint':
      '在 platform.openai.com → API keys 创建；自建兼容网关填它自己的地址与 Key。',
    'provider.openaiImages.baseUrlHint':
      'OpenAI 官方为 https://api.openai.com/v1；兼容网关填对应地址。',
    'connections.eyebrow': '配置',
    'connections.title': '平台连接',
    'connections.description':
      '配置你要使用的生成源连接（RunningHub、ComfyUI、OpenAI 图像）。应用会显示它的来源与执行连接。',
    'connections.loadFailed': '加载连接失败：{message}',
    'connections.step1Title': '第 1 步：接入生成源',
    'connections.step1Body':
      '添加 RunningHub 连接后即可导入应用并运行任务。国内（.cn）与国际（.ai）平台数据独立，可各添加一个。',
    'connections.addRhConnection': '添加 RunningHub 连接',
    'connections.groupGeneration': '生成渠道',
    'connections.groupGenerationHint': '三类生成来源，各配好一个连接即可运行对应应用。',
    'connections.addConnection': '添加连接',
    'connections.testing': '测试中…',
    'connections.test': '测试',
    'connections.edit': '编辑',
    'connections.delete': '删除',
    'connections.emptyHint': '尚未配置{label}连接，点击右上角「添加连接」开始。',
    'connections.drawerCreate': '添加 {label} 连接',
    'connections.drawerEdit': '编辑 {label} 连接',
    'connections.displayName': '显示名称',
    'connections.displayNamePlaceholder': '连接显示名称',
    'connections.platform': '平台',
    'connections.endpointCn': '国内 · www.runninghub.cn',
    'connections.endpointAi': '国际 · www.runninghub.ai',
    'connections.defaultNameDomestic': 'RunningHub（国内）',
    'connections.defaultNameGlobal': 'RunningHub（国际）',
    'connections.customAddress': '自定义地址',
    'connections.execEnv': '执行环境',
    'connections.manage': '管理',
    'connections.rhCloudNoteBefore': 'RunningHub 云端 ComfyUI：Base URL 将使用',
    'connections.rhCloudNoteAfter': '，实际请求时自动拼接 API Key。',
    'connections.allowPrivateNetwork': '允许访问局域网地址',
    'connections.allowPrivateNetworkHint':
      'ComfyUI 在另一台机器上（显卡机）时打开，例如 http://192.168.1.50:8188。默认只放行本机 127.0.0.1 —— 这是防止 daemon 被指向内网任意地址的守卫，所以按连接单独开。',
    'connections.apiKey': 'API Key',
    // 驱动声明 `requiresApiKey: false` 时用这个标签：字段照样显示（rh-proxy / 带鉴权的远程要用），
    // 但不能让人以为"必填"——本地 ComfyUI 就是个典型。
    'connections.apiKeyOptional': 'API Key（可选）',
    'connections.apiKeyEditHint': 'API Key（留空则不修改）',
    // 编辑态标签旁那枚指纹徽章：只给尾号，不给密钥本体（daemon 也只回派生信息）。
    'connections.apiKeySaved': '已保存 · 尾号 {tail}',
    'connections.apiKeyWillReplace': '将替换 · 原尾号 {tail}',
    'connections.apiKeySavedDetail': '已保存的密钥尾号 {tail}，共 {length} 位',
    'connections.apiKeyPlaceholder': '输入 API Key',
    // 凭据引用：ComfyUI 跑在 RunningHub 云端时，密钥**借用**一条 RunningHub 连接。
    'connections.credentialSource': 'API Key 来源',
    'connections.credentialSourceOwn': '自己填 API Key（不使用引用）',
    'connections.credentialSourceHint':
      '借用所选 RunningHub 连接的密钥：本连接不再存一份，所以那边轮换自动生效、删除后这里会立刻报错。',
    'connections.credentialSourceMissing':
      '选中的 RunningHub 连接已经不在了，请重新选择或改为自己填。',
    'connections.credentialSourceDisabled': '选中的 RunningHub 连接已停用，请启用它或改选别的。',
    'connections.credentialSourceNone': '还没有 RunningHub 连接：先在上面添加一条，才能用引用。',
    'connections.apiKeyStorageNote':
      '只通过本地认证连接发送给 Daemon，并保存在系统凭据库，不会写入工作区数据库。',
    'connections.defaultModelOptional': '默认模型（可选）',
    // 模型发现 + 连接级模型清单（通用协议层那两件）。
    'connections.defaultModelHint':
      '运行/配方都没点名模型时用它。留空就交给平台自己的默认——我们不替你编一个。',
    'connections.models': '可用模型',
    'connections.fetchModels': '拉取可用模型',
    'connections.fetchingModels': '拉取中…',
    'connections.fetchModelsNeedsSave': '新建时还拉不了清单：先保存这条连接，再从「编辑」里拉取。',
    'connections.fetchModelsEmpty': '平台没有返回任何模型（清单为空）。',
    'connections.modelsHint':
      '勾选这条连接能跑的模型；运行时表单的「模型」字段会只给这些选项。不勾 = 不限制（照旧自由填写）。',
    'connections.modelsDeclared': '已声明：{models}',
    'connections.saving': '保存中…',
    'connections.save': '保存',
    'connections.saved': '{label} 连接已保存',
    'connections.testOk': '{name} 连接正常',
    'connections.testFailed': '连接测试失败',
    'connections.deleted': '连接已删除',
    'connections.confirmDelete': '确定删除「{name}」连接吗？',
    'connections.presetTitle': 'ComfyUI 端点预设',
    'connections.presetDescription': '预设会保存在本机，供创建 ComfyUI 连接时快速选择。',
    'connections.noCustomPresets':
      '还没有自定义预设。内置「本地 ComfyUI」与「RunningHub 云端」始终可用。',
    'connections.presetNamePlaceholder': '预设名称，如：我的 GPU 服务器',
    'connections.presetUrlPlaceholder': 'Base URL，如：http://192.168.1.5:8188',
    'connections.addPreset': '添加预设',
    'connections.done': '完成',
    'connections.presetLocal': '本地 ComfyUI（默认端口）',
    'connections.presetRhCloud': 'RunningHub 云端（国内）',
    'connections.presetRhCloudAi': 'RunningHub 云端（国际）',
    'connections.presetNameRequired': '请填写预设名称',
    'connections.baseUrlRequired': '请填写 Base URL',
    'connections.presetUrlExists': '该地址已存在',
    'connections.statusUnknown': '无法读取状态',
    'connections.statusDisabled': '连接已停用',
    'connections.statusTestFailed': '连接测试失败',
    'connections.statusTested': '连接正常',
    'connections.statusIncomplete': '配置不完整',
    'connections.statusPaused': '已暂停接单',
    'connections.statusPausedHint': '已暂停接单：{reason}',
    'connections.statusMissingKey': '缺少密钥',
    'connections.statusConfigured': '已配置',
    /*
     * 连接的**状态面**（形状与判据见 shared/lib/connectionFacet.ts）。
     *
     * 措辞规则：数字永远带主语（「余额」「并发」「排队」）。别写成没有主语的裸数字——
     * 那正是原来侧栏「RH 币」那一格的问题：说了数，没说这是谁的、在哪花的。
     * 「不可读」只用于**这一次查不到**；平台不提供这个面时什么都不显示。
     */
    'connections.facetBalance': '余额 {value}',
    'connections.facetBalanceUnavailable': '余额不可读',
    'connections.facetRunning': '运行中 {running}',
    'connections.facetRunningOf': '并发 {running}/{limit}',
    'connections.facetQueued': '排队 {queued}',
    'connections.facetQueueUnavailable': '队列不可读',
    /*
     * 高级项：只有**用户环境**才知道的两个量（详见 src-tauri 的 domain/connection_options.rs）。
     * 提示里的默认值与范围都由 daemon 报（诊断接口的 `defaults`），**不在前端写死**——
     * 前端抄一份，改的时候就漂了。
     */
    'connections.advanced': '高级（可选，留空即用默认值）',
    'connections.requestTimeout': '请求超时（秒）',
    'connections.requestTimeoutHint':
      '提交/上传这类请求最多等多久。留空用默认（{default} 秒），范围 {range}；越界按边界值算。',
    'connections.trackingDeadline': '跟踪上限（秒）',
    'connections.trackingDeadlineHint':
      '远端任务超过这个时长还没结束就本地判失败。留空用默认（{default} 秒），范围 {range}；跑长视频/长工作流时调大它。'
  },
  'en-US': {
    'nav.workspace': 'Workspace',
    'nav.library': 'Library',
    'nav.tasks': 'Tasks',
    'nav.connections': 'Connections',
    'nav.settings': 'Service settings',
    'shell.localWorkspace': 'Local AI workspace',
    'shell.workspaceGroup': 'WORKSPACE',
    'shell.systemGroup': 'SYSTEM',
    'shell.mainNav': 'Main navigation',
    'shell.mobileNav': 'Mobile navigation',
    'shell.connected': 'Connected',
    'shell.configure': 'Configure service',
    'shell.switchDark': 'Switch to dark theme',
    'shell.switchLight': 'Switch to light theme',
    'shell.runningTasks': '{count} tasks in progress',
    // "RH coins" and "Wallet" were removed here: that is a fact about the **platform account**,
    // not about the workbench. See the matching note in the zh-CN block.
    'shell.connectionUnknown': 'Unable to read connection status',
    'shell.connections': 'Connections',
    'shell.noConnections': 'No connections configured, click to manage',
    'home.eyebrow': 'WORKSPACE',
    'home.title': 'Workspace',
    'home.description': 'Work with assets, execution connections, and recent tasks in one place.',
    'home.addApp': 'Add app',
    'home.allApps': 'All apps',
    'home.startWithApp': 'Start with an app ID',
    'home.startWithAppHint': 'Recently used apps appear here after you add one.',
    'home.recentTasks': 'Recent tasks',
    'home.goToApps': 'Go to Library',
    'home.quickStart': 'Quick start',
    'home.setupEyebrow': 'FIRST RUN',
    'home.setupHint': 'Three steps to start',
    'home.setupTitle': 'Configure once, then run apps directly',
    'home.setupDescription':
      'RenderLoft turns platform capabilities into simple parameter forms. Configure a connection, import an app, and start creating.',
    'home.setupSkip': 'Do this later',
    'home.setupConnectionTitle': 'Configure a connection',
    'home.setupConnectionDescription':
      'Add a RunningHub API key, or configure local ComfyUI / model APIs.',
    'home.setupConfigure': 'Configure connection',
    'home.setupViewConnection': 'View connections',
    'home.setupAppTitle': 'Import an app',
    'home.setupAppDescription':
      'Start with a RunningHub app link or ID, or paste a ComfyUI "Save (API format)" JSON; then give the fields Chinese labels.',
    'home.setupImport': 'Import an app',
    'home.setupViewApps': 'View apps',
    'home.setupRunTitle': 'Run and see results',
    'home.setupRunDescription':
      'Fill the required inputs, submit, then follow progress and results in Tasks.',
    'home.setupOpenApp': 'Open Library',
    'home.setupViewTasks': 'View tasks',
    'home.quickStartRunningHub': 'RunningHub app',
    'home.quickStartRunningHubHint': 'Add and run a cloud app.',
    'home.quickStartComfyui': 'ComfyUI workflow',
    'home.quickStartComfyuiHint':
      'Use a ComfyUI connection to run your own workflows: paste a "Save (API format)" JSON to import, then label the fields.',
    'home.quickStartOpenAiImage': 'OpenAI image',
    'home.quickStartOpenAiImageHint':
      'Save an image template and run it through a configured image connection.',
    'home.openLibrary': 'Open Library',
    'home.loadingWorkspace': 'Loading workspace…',
    'home.loadWorkspaceFailed': 'Some workspace data could not be loaded. Try again.',
    'home.recentAssets': 'Recently used apps',
    'connection.runningHub': 'RunningHub',
    'connection.comfyui': 'ComfyUI',
    'connection.openAiImage': 'OpenAI image',
    'connection.connected': 'Connected',
    'connection.notConfigured': 'Not configured',
    'connection.unknown': 'Unknown',
    'connection.configure': 'Configure connection',
    'status.success': 'Completed',
    'status.running': 'Running',
    'status.failed': 'Failed',
    'status.queued': 'Queued',
    'status.partial': 'Partially completed',
    'status.cancelled': 'Cancelled',
    'status.submitting': 'Submitting',
    'status.preparing': 'Preparing',
    'status.submissionUncertain': 'Submission uncertain',
    'status.accepted': 'Accepted',
    'status.retryWait': 'Waiting to retry',
    'history.all': 'All',
    'history.active': 'Active',
    'task.liveActivity': 'Live activity',
    'task.eyebrow': 'LIVE ACTIVITY',
    'task.expand': 'Expand live activity',
    'task.collapse': 'Collapse live activity',
    'task.allHistory': 'All history',
    'task.refresh': 'Refresh task status',
    'task.refreshing': 'Refreshing task status',
    'task.close': 'Close live activity',
    'task.autoSync': 'Auto sync active',
    'task.synced': 'Synced',
    'task.syncError': 'Sync temporarily unavailable',
    'task.activeCount': '{count} active',
    'task.updatedAt': 'Updated {time}',
    'task.empty': 'Task progress and results will appear here after submission',
    'task.runs': 'runs',
    'unit.coins': '{value} coins',
    'task.done': 'done',
    'task.openDetails': 'Open task details',
    'task.cancel': 'Cancel task',
    'task.cancelBatch': 'Cancel batch',
    'task.cancelling': 'Cancelling…',
    'task.unknownTime': 'Unknown time',
    'task.justNow': 'Just now',
    'task.operationLabel': 'Operation #{id}',
    'task.noActive': 'No active tasks',
    'task.noBatch': 'No batch records',
    'task.reload': 'Refresh',
    'task.clearFinished': 'Clear finished',
    'task.clearing': 'Clearing…',
    'task.rerun': 'Run again',
    'task.deleteHistory': 'Delete history',
    'task.confirmDelete':
      'Delete task #{id} history and artifacts? Deleted artifacts cannot be recovered.',
    'task.confirmClear':
      'Clear {count} finished tasks? Their records and artifacts are deleted permanently.',
    'task.rerunFailedDetail': 'Rerun failed: {message}',
    'task.rerunFailed': 'Rerun failed. Please try again later.',
    'task.deleteFailedDetail': 'Delete failed: {message}',
    'task.deleteFailed': 'Delete failed. Please try again later.',
    'task.clearFailedDetail': 'Clear failed: {message}',
    'task.clearFailed': 'Clear failed. Please try again later.',
    'task.downloadFailedDetail': 'Download failed: {message}',
    'task.downloadFailed': 'Download failed. Please try again later.',
    'task.downloaded': 'Saved {name} → {path}',
    'task.showInFolder': 'Show in folder',
    // One operation (a batch) can hold dozens of artifacts: say how many were saved and how many
    // failed — silently saving fewer is the worst ending (same rule as the CLI conflict policy).
    'task.downloadAll': 'Download all artifacts',
    'task.downloadingMany': 'Downloading {count} files…',
    'task.downloadedMany': 'Saved {count} files → {path}',
    'task.downloadPartial': '{done} saved · {failed} failed: {message}',
    'task.downloadNothing': 'Nothing to download in this batch',

    // ---- Task center (TaskCenter) ----
    'common.close': 'Close',
    'task.appIdentity': 'App · {name}',
    'task.batchRunning': 'Running',
    'task.batchPartial': 'Partially failed',
    'task.batchSucceeded': 'Completed',
    'task.batchFailed': 'Failed',
    'task.batchInProgress': 'Batch running',
    'task.batchMeta': 'Batch · {count} tasks',
    'task.batchSummaryRuns': '{completed} done · {running} running · {failed} incomplete',
    'task.batchSummaryDone': '{count} done · 0 running · 0 incomplete',
    'task.batchSummaryPartial': 'Partially completed · {count} tasks',
    'task.batchSummaryFailed': 'All failed · {count} tasks',
    'task.batchSummaryRunning': 'Running · {count} tasks',
    'task.mediaGenerating': 'Generating',
    'task.mediaWaitingResource': 'Waiting for resources',
    'task.mediaWaitingRetry': 'Waiting to retry',
    'task.mediaSubmitting': 'Submitting',
    'task.mediaVerifying': 'Verifying',
    'task.mediaTracking': 'Tracking',
    'task.trackQueued': 'Accepted by the service, queued',
    'task.trackSubmitted': 'Submitted, waiting for confirmation',
    'task.trackUncertain': 'Submission outcome unknown, verifying',
    'task.trackGenerating': 'Generating — results coming soon',
    'task.trackRetryWait': 'Waiting for an automatic retry',
    'task.trackAuto': 'Auto-tracking',
    'task.durationSeconds': '{n}s',
    'task.durationMinutes': '{n}m',
    'task.durationMinSec': '{m}m {s}s',
    'task.durationHourMin': '{h}h {m}m',
    'task.notifyTitle': 'Task completed',
    'task.reconcileInactive': 'Waiting to schedule',
    'task.reconcileActive': 'Reconciling',
    'task.reconcilePaused': 'Paused',
    'task.reconcileExhausted': 'Stopped',
    'task.unreadCount': 'Unread task count',
    'task.viewTabs': 'Task center views',
    'task.tabTasks': 'Tasks',
    'task.tabBatches': 'Batches',
    'task.viewBatch': 'View batch',
    'task.taskMeta': 'Task · {id}',
    'task.recentTasks': 'Recent tasks',
    'task.viewDetail': 'View task details',
    'task.previewNamed': 'Preview {name}',
    'task.resultCount': '{count} results',
    'task.download': 'Download',
    'task.details': 'Details',
    'task.recentBatches': 'Recent batches',
    'task.olderBatches': 'Earlier batches',
    'task.taskCount': '{count} tasks',
    'task.viewBatchDetail': 'View batch details',
    'task.detailTitle': '{name} · Task details',
    'task.batchDetailTitle': '{name} · Batch details',
    'task.fieldApp': 'App',
    'task.fieldStatus': 'Status',
    'task.fieldTaskId': 'Task ID',
    'task.fieldStartedAt': 'Started',
    'task.fieldDuration': 'Duration',
    'task.fieldResults': 'Results',
    'task.fieldBatchStatus': 'Batch status',
    'task.fieldTaskCount': 'Tasks',
    'task.fieldRunStatus': 'Run status',
    'task.queuedAhead': '{count} ahead',
    'task.noRunsRecorded': 'No runs recorded for this operation.',
    'task.failureReasons': 'Failure reasons',
    // See the Chinese block: queued / retry-backoff / submission-uncertain all *look* like
    // "in progress", so these lines are the only explanation that it is not actually moving.
    'attention.title': 'It is not actually moving',
    'attention.uncertain': 'Waiting for confirmation (submission outcome unknown)',
    'attention.retryBackoff': 'Waiting to retry',
    'attention.submitting': 'Submitting',
    'attention.queued': 'Queued',
    'attention.deadline': 'gives up at {time}',
    'attention.noDeadline': 'no deadline (it keeps waiting)',
    'attention.whyUncertain':
      'The connection dropped or timed out while submitting, so we do NOT know whether the platform received it — which is why it is never re-submitted automatically. If it is still unconfirmed at the deadline, it is marked failed.',
    'attention.whyUncertainNoDeadline':
      'The connection dropped or timed out while submitting; we do not know whether the platform received it and are reconciling.',
    'attention.whyRetryBackoff':
      'The last submission did not go through (the platform refused it, or the failure proved that no bytes left this machine). It is retrying automatically — nothing for you to do.',
    'attention.whySubmitting':
      'Submitting now: if the process is interrupted here, it becomes "waiting for confirmation" once the recovery window passes.',
    'attention.whyQueued':
      'Queued: this connection runs one task at a time (matching the account’s real concurrency) and an earlier run has not finished.',
    'task.confirmRetry':
      'Retrying creates a NEW task (reusing this task’s parameters) and charges your account again. Continue?',
    'task.retryFailedRuns': 'Retry only the {count} failed',
    'task.retryingFailed': 'Creating tasks…',
    'task.confirmRetryFailed':
      'This creates one NEW task for each of the {count} failed runs (reusing their own parameters) and charges your account {count} more times. Continue?',
    'task.retryFailed': 'Retry failed. Please try again.',
    'task.retryFailedDetail': 'Retry failed: {message}',
    'task.reuseParams': 'Run again with these parameters (editable)',
    'task.confirmReuseOverwrite':
      'This app’s form already has content. Replace it with this task’s parameters? (The current content is not kept.)',
    'task.reuseFailed': 'Could not open that app’s form. Please try again.',
    'task.reuseFailedDetail': 'Could not open that app’s form: {message}',
    'task.assetFallbackName': 'asset #{id}',
    'runner.clearForm': 'Clear form',
    'runner.clearFormTitle': 'Clear this form?',
    'runner.clearFormDescription':
      'Clears everything currently filled in, including batch row drafts. Submitted tasks are not affected — their parameters are still available via “Run again with these parameters”.',

    'common.create': 'Create',
    'common.creating': 'Creating…',
    // ---- Shared components + Home / Settings (final batch) ----
    'common.loading': 'Loading',
    'common.dialog': 'Dialog',
    'common.closeDialog': 'Close dialog',
    'common.confirm': 'Confirm',
    'common.processing': 'Processing…',
    'home.quickStartEyebrow': 'QUICK START',
    'home.recentAppsEyebrow': 'RECENT APPS',
    'field.required': 'This field is required',
    'field.mustBeInteger': 'Must be an integer',
    'field.mustBeNumber': 'Must be a number',
    'field.requiredMark': '(required)',
    'field.selectedFile': 'Selected file',
    'field.uploading': 'Uploading…',
    'field.uploaded': 'Uploaded',
    'field.remove': 'Remove',
    'field.clickToUpload': 'Click to upload',
    'field.dragHint': ' or drag a file here',
    'shell.closeTab': 'Close {title}',
    'shell.connUnreadable': 'Unreadable',
    'shell.connDisabled': 'Disabled',
    'shell.connInvalid': 'Invalid config',
    'shell.connNotReady': 'Not ready',
    'shell.runApp': 'Run app',
    'shell.notProvided': 'Not provided',
    'previewDialog.downloadResult': 'Download result {n}',
    'previewDialog.closePreview': 'Close preview',
    'previewDialog.prevResult': 'Previous result',
    'previewDialog.nextResult': 'Next result',
    'previewDialog.resultAlt': '{title} result {n}',
    'previewDialog.noText': 'No previewable text',
    'previewDialog.resultLabel': 'Result {n}',
    'previewDialog.download': 'Download',
    'api.timeout': 'Request timed out ({seconds}s)',
    'api.nonJsonResponse': 'The service returned a non-JSON response (HTTP {status}): {excerpt}',
    'api.unknownError': 'Unknown error',
    'api.downloadFailedWithDetail': 'Download failed (HTTP {status}): {excerpt}',
    'api.downloadFailed': 'Download failed (HTTP {status})',
    'settings.eyebrow': 'SERVICE SETTINGS',
    'settings.title': 'Service settings',
    'settings.description':
      'Connections and credentials live on the Connections page; this page only covers interface, theme and local preferences.',
    'settings.language': 'Interface language',
    'settings.languageHint':
      'This changes RenderLoft only. It does not change your RunningHub service region or app parameters.',
    'settings.languageZh': 'Simplified Chinese',
    'settings.languageEn': 'English',
    'settings.theme': 'Theme',
    'settings.save': 'Save settings',
    'settings.saving': 'Saving',
    'settings.diagnostics': 'Diagnostics',
    'settings.diagnosticsHint':
      'Includes only version, system, service region, and API key status. It excludes keys, prompts, and task content.',
    'settings.copyDiagnostics': 'Copy diagnostics',
    'settings.copyingDiagnostics': 'Copying',
    'settings.downloads': 'Downloads',
    'settings.saveSuccess': 'Settings saved',
    'settings.downloadDir': 'Download directory',
    'settings.downloadDirDescription':
      'The desktop app saves results here. Browser access follows the browser download settings.',
    'settings.downloadDirPlaceholder': 'e.g. D:\\RenderLoft\\Downloads',
    'settings.historyRetention': 'History retention (days)',
    'settings.historyRetentionDescription':
      'Finished tasks older than this are cleaned up (including their artifacts and events); 0 = keep forever.',
    'settings.historyRetentionPlaceholder': 'default 30',
    'settings.openDir': 'Open directory',
    'settings.savedConfigHint': 'Download directory saved.',
    'settings.openDownloadDirFailed': 'Failed to open download directory: {message}',
    'settings.diagnosticsCopied': 'Diagnostics copied',
    'settings.copyDiagnosticsFailed': 'Failed to copy diagnostics: {message}',
    'settings.diagnosticsTitle': 'RenderLoft diagnostics',
    'settings.version': 'Version',
    'settings.system': 'System',
    'settings.serviceRegion': 'Service region',
    'settings.serviceHost': 'Service host',
    'settings.apiKeyConfigured': 'API key configured',
    'settings.outboundProxy': 'Outbound proxy',
    'settings.runtimeParams': 'Runtime parameters',
    'settings.runtimeParamsHint':
      'These are the values the daemon is actually using (from environment variables or program location). When something breaks, set the variable in the "Change it here" column and restart the daemon.',
    'settings.runtimeWhere': 'Change it here',
    'settings.runtimeBind': 'Listen address',
    'settings.runtimeAllowedHosts': 'Allowed hosts',
    'settings.runtimeAllowedHostsEmpty': '(unset: loopback only)',
    'settings.runtimeUiOrigin': 'UI origin',
    'settings.runtimeUiOriginEmpty': '(unset: the bundled UI)',
    'settings.runtimeDistDir': 'UI asset directory',
    'settings.runtimeOutboundProxyEmpty': '(unset: direct connection)',
    'settings.defaultRequestTimeout': 'Default request timeout',
    'settings.defaultTrackingDeadline': 'Default tracking limit',
    'settings.secondsValue': '{value} s',
    'settings.defaultsHint':
      'Per-connection overrides live in the connection drawer under Advanced (leave blank to use these defaults).',
    'settings.logs': 'Logs',
    'settings.logsHint':
      'When something breaks, hand this directory to your agent (or paste it to someone) — it holds the daemon and shell logs (no keys, no prompts).',
    'settings.logsMissing':
      '(no log files here right now: the daemon output may be redirected elsewhere)',
    'settings.logsTimezone':
      'Log lines are UTC (this machine is {offset}) — the trailing Z means UTC.',
    'settings.logsJustNow': 'written just now',
    'settings.logsMinutesAgo': '{n} min ago',
    'settings.logsHoursAgo': '{n} h ago',
    'settings.logsUnknownTime': 'write time unknown',
    'settings.loadFailed': 'Failed to load settings: {message}',
    'settings.connectionsLoadFailed': 'Failed to load connections: {message}',
    'settings.saveFailed': 'Save failed: {message}',
    'settings.updateApps': 'App updates',
    'settings.updateAppsHint': 'Check for saved app parameter updates when opening Library.',
    'library.assetTypeAll': 'All types',
    'library.kindRhApp': 'RH app',
    'library.kindRhWorkflow': 'RH workflow',
    'library.kindComfyWorkflow': 'ComfyUI workflow',
    'library.kindOpenAiImage': 'OpenAI image',
    'library.kindOther': 'Other',
    'library.goToLibrary': 'Go to Library',
    'library.exportLibrary': 'Export library',
    'library.importLibrary': 'Import library',
    'library.exporting': 'Exporting…',
    'library.exportEmpty': 'The library is empty — nothing to export.',
    'library.exportDone': 'Exported {count} app(s) to {path}',
    'library.importDone': 'Imported {imported} · skipped {skipped} · failed {failed}',
    'library.importNothing': 'This file contains no apps.',
    'common.retry': 'Retry',
    'history.eyebrow': 'TASK ARCHIVE',
    'history.title': 'Task history',
    'history.description': 'Browse, filter, and organize every generated task.',
    'history.stats': 'Task statistics',
    'history.search': 'Search task ID or prompt',
    'history.archivedCount': '{count} archived tasks',
    'history.loadMore': 'Load more ({count} remaining)',
    'history.results': 'Results and tasks',
    'history.sorted': 'Sorted by creation time',
    'history.resultCount': '{count} results',
    'history.download': 'Download first result',
    'history.noMatches': 'No tasks match this filter',
    'history.empty': 'No task history yet',
    'history.noMatchesHint': 'Adjust your search or status filter to view other tasks.',
    'history.emptyHint':
      'Generated tasks and results appear here after you run an asset from Library.',

    // ---- Shared words (cross-page; settings.cancel / yes / no now live here) ----
    'common.cancel': 'Cancel',
    'common.yes': 'Yes',
    'common.no': 'No',
    'common.listSeparator': ', ',
    'common.all': 'All',
    'common.eyebrowApp': 'AI APP',
    'common.eyebrowLibrary': 'APPS & WORKFLOWS',
    'common.eyebrowRecipe': 'APP',

    // ---- App runner (V2Runner) ----
    'status.pending': 'Pending',
    'runner.uploadTooLarge': 'Files must be 100 MB or smaller',
    'runner.revisionExpired':
      'This app version is no longer valid. Return to Library and try again.',
    'runner.rowTag': ' in row {n}',
    'runner.invalidNumbers': 'Fix invalid number fields{row}: {fields}',
    'runner.missingRequired': 'Fill in required fields{row}: {fields}',
    'runner.submitted': 'Task #{id} submitted. Track progress in "Live tasks" on the right.',
    'runner.fieldNamingHint':
      'These names come from the platform; rename them to match what they do.',
    'runner.editFields': 'Edit fields',
    'runner.backToLibrary': '← Back to Library',
    'runner.backToLibraryPlain': 'Back to Library',
    'runner.openFailedTitle': 'Cannot open app',
    'runner.openFailedDescription':
      'The app does not exist or failed to load. Return to Library and try again.',
    'runner.paramsTitle': 'Fill in parameters',
    'runner.paramsHint': 'Complete the required inputs, then generate once or in batch.',
    'runner.modeGroup': 'Run mode',
    'runner.modeSingle': 'Single run',
    'runner.modeBatch': 'Batch run',
    'runner.batchUniformTitle': 'Shared settings for this batch',
    'runner.batchUniformHint':
      'Fill these once; every task row uses them. For per-row values, tick "Fill individually" and edit in the table below.',
    'runner.fieldToggle': 'Fill individually',
    'runner.noParamsTitle': 'No parameters',
    'runner.noParamsDescription': 'This recipe takes no input.',
    'runner.batchSummary': 'Batch summary',
    'runner.totalRows': '{count} rows',
    'runner.rowsPanelTitle': 'Fill row by row',
    'runner.rowsPanelHint': 'Each row creates a separate task.',
    'runner.rowsPanelHintNoVariable':
      'Each row creates a separate task. Tick "Fill individually" above to set values per row here.',
    'runner.addRow': 'Add a task',
    'runner.addRows': 'Add multiple',
    'runner.clearPending': 'Clear pending',
    'runner.colStatus': 'Status',
    'runner.colActions': 'Actions',
    'runner.replace': 'Replace',
    'runner.choose': 'Choose',
    'runner.remove': 'Remove',
    'runner.uploading': 'Uploading…',
    'runner.dropHint': 'Choose or drop a file here',
    'runner.unifiedFile': 'Shared: {name}',
    'runner.duplicate': 'Duplicate',
    'runner.delete': 'Delete',
    'runner.willCreate': '{count} tasks will be created',
    'runner.willCreateOne': '1 task will be created',
    'runner.boundConnection': 'Connection {name}',
    'runner.submitting': 'Submitting…',
    'runner.startBatch': 'Start batch generation',
    'runner.start': 'Generate',
    'runner.dialogAddTitle': 'Add multiple tasks',
    'runner.dialogAddDescription':
      'Creates new pending rows from the current shared settings; adjust them row by row afterwards.',
    'runner.namePrefix': 'Name',
    'runner.countLabel': 'Task count',
    'runner.namePrefixPlaceholder': 'e.g. SKU135 (file name)',
    'runner.colName': 'Name',
    'runner.rowNamePlaceholder': 'Blank = prefix-N',
    'runner.duplicateNames': 'Duplicate names: {names}',
    'runner.addCount': 'Add {count}',
    'runner.dialogClearTitle': 'Clear pending tasks?',
    'runner.dialogClearDescription':
      'This deletes {count} unsubmitted task rows and cannot be undone.',
    'runner.blankCellsTitle': 'Some per-row cells are empty',
    'runner.recipeFallback': 'Recipe #{id}',
    'runner.blankCellsDescription':
      'These cells are empty. Per-row fields do not fall back to the shared value, so they will be submitted empty.',
    'runner.blankCellsItem': '{field}: row {rows}',
    'runner.blankCellsConfirm': 'Submit anyway',
    'runner.preSubmitTitle': 'Check before submitting',
    'runner.emptyMediaTitle': 'No image selected',
    'runner.emptyMediaDescription':
      'No image was selected for this submission. The workflow may fail, or fall back to its own default image — either way you are charged.',
    'runner.resubmitTitle': 'This batch was already submitted',
    'runner.resubmitDescription':
      '{total} rows were submitted before ({done} of them finished). Submitting again regenerates every row and charges again. To retry only the failed ones, use “Retry failed” on the Tasks page.',
    'runner.resubmitConfirm': 'Resubmit everything',

    // ---- App library (V2Recipes) ----
    'library.title': 'App library',
    'library.description':
      'Turn platform apps and workflows into simple parameter forms, then run them once or in batch.',
    'library.noConnection': 'No connection',
    'library.driverRunningHub': 'RunningHub (cloud AI apps / workflows)',
    'library.driverComfyui': 'ComfyUI (local / remote workflows)',
    'library.driverOpenAiImage': 'OpenAI Images (text-to-image API)',
    'library.driverAsyncTask': 'Generic async task (cloud)',
    'library.errNoConnection': 'Choose a default connection',
    'library.errInvalidSchema': 'inputSchema is not valid JSON',
    'library.errSelectRhConnection': 'Choose a RunningHub connection',
    'library.errEnterRhSource': 'Enter a RunningHub link or id (AI app or workflow)',
    'library.comfyPromptPlaceholder':
      'Pick the JSON exported by "Save (API format)" from a ComfyUI workflow,\nor paste a node object here. For example:',
    'library.errNoRhConnection':
      'No RunningHub connection is available. Configure an API key in Connections first.',
    'library.errEnterRhWorkflow': 'Paste a RunningHub workflow link or ID',
    'library.errSelectComfyConnection': 'Choose a ComfyUI connection',
    'library.errEnterComfyPrompt': 'Paste the ComfyUI API Prompt JSON, or read it from a file',
    'library.errInvalidComfyJson': 'API Prompt is not valid JSON',
    'library.chooseJsonFile': 'Read from file',
    'library.widgetDefault': 'Control: default',
    'library.widgetText': 'Single-line text',
    'library.widgetTextarea': 'Multi-line text',
    'library.widgetNumber': 'Number',
    'library.widgetSwitch': 'Toggle',
    'library.widgetDropdown': 'Dropdown (enter options)',
    'library.widgetFile': 'Image or file',
    'library.widgetFor': 'Control for {name}',
    'library.widgetOptionsPlaceholder': 'Options, comma-separated (e.g. 4, 7, 10)',
    'library.widgetKeep': 'Control: keep as is',
    'library.widgetYours': 'Yours',
    'library.widgetFromEngine': 'From engine',
    'library.promptInputHint':
      'Paste it here, or read it from a file — in ComfyUI pick “Save (API Format)”, not “Save”.',
    'library.errComfyUiFormat':
      'This is ComfyUI’s workflow format (top-level nodes/links), not the API format — export again with “Save (API Format)”.',
    'library.errComfyFileTooLarge':
      'This file is over {mb} MB — that is almost certainly not an API Prompt. Check that you used “Save (API Format)”.',
    'library.enrichmentOk':
      'Read {known}/{used} node definitions from ComfyUI: enum fields became dropdowns and numbers carry real ranges.',
    'library.enrichmentMissing':
      'Could not read ComfyUI’s node definitions ({reason}), so the controls are generic: fields that should be dropdowns are text boxes and numbers have no range. Start ComfyUI and import again to get them.',
    'library.enrichmentUnknown': 'unknown reason',
    'library.errParseComfyFirst': 'Parse the API Prompt first',
    'library.errSelectField': 'Select at least one field',
    'library.addFromComfy': 'Add from ComfyUI API Prompt',
    'library.configureRunningHub': 'Configure RunningHub',
    'library.addFromRunningHub': 'Add from RunningHub',
    'library.advancedCreate': 'Advanced create',
    'library.noticeTitle': 'Start from a public app and build your workspace',
    'library.noticeBody':
      'Add apps by RunningHub link or ID, or let RenderLoft detect parameters from a ComfyUI API Prompt export.',
    'library.count': '{count} apps',
    'library.searchPlaceholder': 'Search name, note, group, or label',
    'library.scopeFavorites': 'Favorites only',
    'library.sortRecent': 'Recently used',
    'library.sortAdded': 'Date added',
    'library.sortName': 'Name',
    'library.groupFilter': 'Groups',
    'library.allGroups': 'All groups',
    'library.noNote': 'No description yet. Open the app to start filling in parameters.',
    'library.recentlyUsed': 'Recently used',
    'library.neverOpened': 'Not opened yet',
    'library.open': 'Open',
    'library.favorite': 'Favorite {name}',
    'library.unfavorite': 'Remove {name} from favorites',
    'library.editNamed': 'Edit {name}',
    'library.removeNamed': 'Remove {name}',
    'library.restoreNamed': 'Restore {name}',
    'library.showRemoved': 'Show removed',
    'library.hideRemoved': 'Hide removed',
    'library.removedSection': 'Removed ({count})',
    'library.removedAt': 'Removed {time}',
    'library.removedBadge': 'Removed',
    'library.removedEmpty':
      'Nothing removed. Apps you remove wait here, and you can restore them at any time.',
    'library.removedLoading': 'Loading removed apps…',
    'library.restore': 'Restore',
    'library.emptyFilteredTitle': 'No apps match this filter',
    'library.emptyTitle': 'No AI apps yet',
    'library.emptyFilteredDescription': 'Try a broader search or clear the filters.',
    'library.emptyDescription':
      'Configure a platform connection first, then add a public app from RunningHub.',
    'library.clearFilters': 'Clear filters',
    'library.addRhApp': 'Add from RunningHub',
    'library.configureRhFirst': 'Configure RunningHub first',
    'library.dialogAddRh': 'Add from RunningHub',
    'library.dialogAddRhHint': 'Paste a link or id — AI apps and workflows are both supported.',
    'library.rhKindLabel': 'Import as',
    'library.rhKindAuto': 'Auto-detect',
    'library.rhKindApp': 'AI app',
    'library.rhKindWorkflow': 'Workflow',
    'library.rhKindMismatch': 'Does not match the detected kind; the selected kind will be used.',
    'library.rhKindHintApp':
      'Detected as an AI app. To write the prompt yourself, use the post-page link.',
    'library.rhKindHintWorkflow':
      'Detected as a workflow. For the version the author configured, click “Open AI app” on the page and paste that link.',
    'library.rhKindHintUnknown': 'Kind could not be detected; both will be tried in turn.',
    'library.rhNamePlatformOwned':
      'The name comes from the platform and can be changed after importing.',
    'library.errRhChosenAppMissing':
      'This id is not an AI app (the platform returned: {message}). For the app version, click “Open AI app” on the page and paste that link; or set the import kind back to auto-detect.',
    'library.errRhChosenWorkflowMissing':
      'This id is not a workflow (the platform returned: {message}). The post-page link is the workflow; or set the import kind back to auto-detect.',
    'library.selectPlaceholder': 'Choose',
    'library.noRhConnectionHint':
      'No RunningHub connection is available. Configure the Base URL and API key in Connections first.',
    'library.incompleteConnectionsHidden':
      'Some connections are not fully configured and are hidden from this list.',
    'library.goConfigureConnection': 'Configure connection →',
    'library.appLinkOrId': 'Link or ID',
    'library.rhSourcePlaceholder': 'https://www.runninghub.cn/post/…',
    'library.rhNamePlaceholder': 'Leave empty to use “RunningHub workflow + id”',
    'library.errRhSourceNeither':
      'Neither attempt succeeded: as an app ({app}), as a workflow ({workflow}).',
    'library.importing': 'Importing…',
    'library.import': 'Import',
    'library.dialogAddComfy': 'Add ComfyUI API Prompt',
    'library.dialogSelectFields': 'Choose parameters to expose',
    'library.dialogAddComfyDescription':
      'Paste the workflow JSON exported by "Save (API format)" from ComfyUI. Next you can pick which fields to expose as form inputs and give each a label.',
    'library.noComfyConnectionHint':
      'No ComfyUI connection is available. Configure the Base URL in Connections first.',
    'library.appNameOptional': 'App name (optional)',
    'library.appNamePlaceholder': 'e.g. My text-to-image workflow',
    'library.fetchFromRhOptional': 'Fetch from a RunningHub workflow (optional)',
    'library.workflowSourcePlaceholder':
      'Paste a workflow link or ID, e.g. https://www.runninghub.cn/post/…',
    'library.fetching': 'Fetching…',
    'library.fetch': 'Fetch',
    'library.fetchHint':
      'Only if you want to run this graph on a ComfyUI of your own (local, or a preset like “RunningHub cloud”). To run it on RunningHub itself, use “Add from RunningHub” — that path needs no ComfyUI connection.',
    'library.comfyFieldsFound':
      'Found {count} parameterizable fields, pre-selected by how commonly they are used. Tick the fields to expose in the run form, and add a short label on the right.',
    'library.node': 'node {id}',
    'library.currentValue': 'Current: {value}',
    'library.file': 'File',
    'library.fieldLabelPlaceholder': 'Label, e.g. {name}',
    'library.positivePrompt': 'Positive prompt',
    'library.parsing': 'Parsing…',
    'library.nextSelectFields': 'Next: choose parameters',
    'library.previousStep': 'Back',
    'library.importSelected': 'Import ({count} selected)',
    'library.createTitle': 'New app',
    'library.formName': 'Name',
    'library.formNamePlaceholder': 'e.g. Text to image',
    'library.formCapability': 'Capability',
    'library.formDriver': 'Driver',
    'library.formDefaultConnection': 'Default connection',
    'library.formConnectionPlaceholder': 'Choose (filtered by driver)',
    'library.connectionDetail': '{name} ({vendor}/{protocol})',
    'library.noCompatibleConnections':
      'No connection is available for this driver. Configure one in Connections first.',
    'library.formDescription': 'Description',
    'library.optional': 'Optional',
    'library.editTitle': 'Edit {name}',
    'library.editHint':
      'Notes and groups are stored on this device only; the remote app is not affected.',
    'library.editHintAfterImport':
      'The platform provides field names but no meaning. Names you fill in here are used by both the run form and AI calls; leaving them empty is fine, you can add them later.',
    'library.formNote': 'Note',
    'library.formNotePlaceholder': 'Purpose, parameter habits, etc.',
    'library.formGroup': 'Group',
    'library.formFieldLabels': 'Field labels',
    'library.formFieldLabelsHint':
      'The platform description is usually the same as the field name, so it does not tell us what a field is for. Names set here are used by both the run form and AI calls.',
    'library.formFieldLabelsEmpty': 'This app has no inputs.',
    'library.formFieldLabelsLoadFailed':
      'Could not load the field list ({message}). Saving will leave field labels untouched — please try again.',
    'library.formFieldLabelPlaceholder': 'Empty = no label',
    'library.formGroupPlaceholder': 'e.g. E-commerce / Workflow',
    'library.save': 'Save',
    'library.removeTitle': 'Remove this app?',
    'library.removeDescription':
      '"{name}" leaves the library and agents can no longer run it. Past tasks stay readable, and you can bring it back any time under "Show removed" — nothing is deleted here, and the remote app is untouched.',
    'library.remove': 'Remove',

    // ---- Connections page ----
    'provider.runningHub.description':
      'Cloud image / video generation (China .cn and international .ai hold separate data)',
    'provider.runningHub.scope':
      'Run cloud AI apps and workflows. One connection is enough to import "AI apps" and run "RH workflows".',
    'provider.runningHub.apiKeyHint':
      'Sign in to runninghub.cn (or .ai) → open the "API" page → go to the console → copy the API key and paste it here.',
    'provider.runningHub.inviteHint':
      'No account yet? Use invite code {code} when signing up ({site}: {coins} coins on registration; without a code it is 100 coins/day). Skipping it changes nothing, and the platform page is authoritative.',
    'connections.inviteSiteCn': 'China site',
    'connections.inviteSiteAi': 'International site',
    'connections.apiKeyRunningHub': 'RunningHub API key',
    'connections.protocolLabel': 'Protocol',
    'connections.platformLabel': 'Platform',
    'connections.platformRunningHubCloud': 'RunningHub cloud',
    'connections.platformLocal': 'This machine',
    'connections.platformLan': 'Local network',
    'connections.platformRemote': 'Remote / self-hosted',
    'connections.showApiKey': 'Show',
    'connections.hideApiKey': 'Hide',
    'provider.comfyui.apiKeyHintRunningHub':
      'This connection runs on RunningHub cloud, so it needs the RunningHub API key — the one you already entered for the RunningHub connection (copy it from the "API" page → console). Leaving it empty fails at run time.',
    'provider.runningHub.baseUrlHint':
      'Defaults to the official platform address; usually no need to change it.',
    'provider.comfyui.description': 'Local or remote workflow service',
    'provider.comfyui.scope':
      'Run your own ComfyUI workflows. The local default port is preset; a remote address also works.',
    'provider.comfyui.baseUrlHint':
      'Locally http://127.0.0.1:8188. A remote address must use https.',
    'provider.comfyui.apiKeyHint':
      'ComfyUI does no authentication of its own — usually leave this empty. Only needed when the address contains /proxy (RunningHub Cloud) or you put your own auth in front of it.',
    'provider.openaiImages.description': 'Compatible with the OpenAI image API',
    'provider.openaiImages.scope':
      'Text-to-image through OpenAI or a compatible API (gpt-image series).',
    'provider.openaiImages.apiKeyHint':
      'Create one at platform.openai.com → API keys; for a self-hosted compatible gateway use its own address and key.',
    'provider.openaiImages.baseUrlHint':
      'OpenAI official is https://api.openai.com/v1; for a compatible gateway use its address.',
    'connections.eyebrow': 'CONNECTIONS',
    'connections.title': 'Platform connections',
    'connections.description':
      'Configure the generation-source connections you want to use (RunningHub, ComfyUI, OpenAI Images). Apps show where they come from and which connection runs them.',
    'connections.loadFailed': 'Failed to load connections: {message}',
    'connections.step1Title': 'Step 1: connect a generation source',
    'connections.step1Body':
      'Add a RunningHub connection to import apps and run tasks. The China (.cn) and international (.ai) platforms hold separate data — add one for each if you need both.',
    'connections.addRhConnection': 'Add RunningHub connection',
    'connections.groupGeneration': 'Generation sources',
    'connections.groupGenerationHint':
      'Three kinds of generation source — configure one connection for each to run the matching apps.',
    'connections.addConnection': 'Add connection',
    'connections.testing': 'Testing…',
    'connections.test': 'Test',
    'connections.edit': 'Edit',
    'connections.delete': 'Delete',
    'connections.emptyHint':
      'No {label} connection yet. Use "Add connection" at the top right to start.',
    'connections.drawerCreate': 'Add {label} connection',
    'connections.drawerEdit': 'Edit {label} connection',
    'connections.displayName': 'Display name',
    'connections.displayNamePlaceholder': 'Connection display name',
    'connections.platform': 'Platform',
    'connections.endpointCn': 'China · www.runninghub.cn',
    'connections.endpointAi': 'International · www.runninghub.ai',
    'connections.defaultNameDomestic': 'RunningHub (China)',
    'connections.defaultNameGlobal': 'RunningHub (Global)',
    'connections.customAddress': 'Custom address',
    'connections.execEnv': 'Execution environment',
    'connections.manage': 'Manage',
    'connections.rhCloudNoteBefore': 'RunningHub cloud ComfyUI: the Base URL will use',
    'connections.rhCloudNoteAfter': '; the API key is appended to each request automatically.',
    'connections.allowPrivateNetwork': 'Allow LAN addresses',
    'connections.allowPrivateNetworkHint':
      'Turn on when ComfyUI runs on another machine (your GPU box), e.g. http://192.168.1.50:8188. By default only 127.0.0.1 is allowed — that guard keeps the daemon from being pointed at arbitrary internal addresses, so it is opt-in per connection.',
    'connections.apiKey': 'API Key',
    'connections.apiKeyOptional': 'API key (optional)',
    'connections.apiKeyEditHint': 'API Key (leave blank to keep unchanged)',
    'connections.apiKeySaved': 'Saved · ends in {tail}',
    'connections.apiKeyWillReplace': 'Replacing · ends in {tail}',
    'connections.apiKeySavedDetail': 'Saved key ends in {tail}, {length} characters long',
    'connections.apiKeyPlaceholder': 'Enter API key',
    'connections.credentialSource': 'API key source',
    'connections.credentialSourceOwn': 'Enter an API key on this connection (no reference)',
    'connections.credentialSourceHint':
      'Borrows the key from the selected RunningHub connection: nothing is stored twice here, so rotating it there takes effect immediately and deleting it makes this connection fail loudly.',
    'connections.credentialSourceMissing':
      'The selected RunningHub connection is gone — pick another one or enter a key here.',
    'connections.credentialSourceDisabled':
      'The selected RunningHub connection is disabled — enable it or pick another one.',
    'connections.credentialSourceNone':
      'No RunningHub connection yet: add one above before you can reference it.',
    'connections.apiKeyStorageNote':
      'Sent to the daemon only over the local authenticated connection and stored in the system credential store — never written to the workspace database.',
    'connections.defaultModelOptional': 'Default model (optional)',
    'connections.defaultModelHint':
      'Used when neither the run nor the recipe names a model. Leave it empty to let the platform pick — we do not invent one.',
    'connections.models': 'Available models',
    'connections.fetchModels': 'Fetch available models',
    'connections.fetchingModels': 'Fetching…',
    'connections.fetchModelsNeedsSave':
      'Cannot fetch yet on a new connection: save it first, then fetch from Edit.',
    'connections.fetchModelsEmpty': 'The platform returned no models (empty list).',
    'connections.modelsHint':
      'Tick the models this connection can run; the run form then offers exactly these in its "model" field. Nothing ticked = no filtering (free text as before).',
    'connections.modelsDeclared': 'Declared: {models}',
    'connections.saving': 'Saving…',
    'connections.save': 'Save',
    'connections.saved': '{label} connection saved',
    'connections.testOk': '{name} connection is healthy',
    'connections.testFailed': 'Connection test failed',
    'connections.deleted': 'Connection deleted',
    'connections.confirmDelete': 'Delete connection "{name}"?',
    'connections.presetTitle': 'ComfyUI endpoint presets',
    'connections.presetDescription':
      'Presets are stored on this device so you can pick them quickly when creating a ComfyUI connection.',
    'connections.noCustomPresets':
      'No custom presets yet. The built-in "Local ComfyUI" and "RunningHub cloud" are always available.',
    'connections.presetNamePlaceholder': 'Preset name, e.g. My GPU server',
    'connections.presetUrlPlaceholder': 'Base URL, e.g. http://192.168.1.5:8188',
    'connections.addPreset': 'Add preset',
    'connections.done': 'Done',
    'connections.presetLocal': 'Local ComfyUI (default port)',
    'connections.presetRhCloud': 'RunningHub cloud (China)',
    'connections.presetRhCloudAi': 'RunningHub cloud (International)',
    'connections.presetNameRequired': 'Enter a preset name',
    'connections.baseUrlRequired': 'Enter a Base URL',
    'connections.presetUrlExists': 'That address already exists',
    'connections.statusUnknown': 'Status unavailable',
    'connections.statusDisabled': 'Connection disabled',
    'connections.statusTestFailed': 'Connection test failed',
    'connections.statusTested': 'Connection healthy',
    'connections.statusIncomplete': 'Incomplete configuration',
    'connections.statusPaused': 'Paused',
    'connections.statusPausedHint': 'Paused: {reason}',
    'connections.statusMissingKey': 'Missing API key',
    'connections.statusConfigured': 'Configured',
    'connections.facetBalance': 'Balance {value}',
    'connections.facetBalanceUnavailable': 'Balance unavailable',
    'connections.facetRunning': 'Running {running}',
    'connections.facetRunningOf': 'Concurrency {running}/{limit}',
    'connections.facetQueued': 'Queued {queued}',
    'connections.facetQueueUnavailable': 'Queue unavailable',
    'connections.advanced': 'Advanced (optional; leave blank for defaults)',
    'connections.requestTimeout': 'Request timeout (seconds)',
    'connections.requestTimeoutHint':
      'How long submit/upload requests may take. Blank uses the default ({default} s); range {range}, out-of-range values are clamped.',
    'connections.trackingDeadline': 'Tracking limit (seconds)',
    'connections.trackingDeadlineHint':
      'A remote task that has not finished within this time is failed locally. Blank uses the default ({default} s); range {range}. Raise it for long videos or long workflows.'
  }
} as const

type MessageKey = keyof (typeof messages)['zh-CN']

export function useLocale() {
  const t = (key: MessageKey, values: Record<string, string | number> = {}) => {
    const template = messages[locale.value][key]
    // 缺失的翻译直接回退 key，避免 undefined.replaceAll 崩溃渲染。
    if (template == null) return String(key)
    // 显式标成 string：messages 的值是一堆字面量类型的联合，
    // 不标注时 `let text` 会继承那个窄类型，下面 replaceAll 的结果（string）就赋不进去。
    let text: string = template
    for (const [name, value] of Object.entries(values))
      text = text.replaceAll(`{${name}}`, String(value))
    return text
  }

  const setLocale = (nextLocale: AppLocale) => {
    locale.value = nextLocale
    if (isClient) {
      localStorage.setItem(LOCALE_STORAGE_KEY, nextLocale)
      document.documentElement.lang = nextLocale
    }
  }

  return {
    locale,
    setLocale,
    t,
    isEnglish: computed(() => locale.value === 'en-US'),
    dateLocale: computed(() => locale.value)
  }
}
