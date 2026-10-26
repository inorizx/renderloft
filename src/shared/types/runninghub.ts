// 仍被 V2 代码引用的 RunningHub 状态类型（账户/队列/诊断/任务结果）。
// 其余 legacy 直连 RunningHub 的类型已随 V2 迁移删除。

export interface AccountStatus {
  remainCoins: string
  currentTaskCounts: string
  remainMoney: string | null
  currency: string | null
  apiType: string
}
export interface QueueStatus {
  apiKeyType: 'EXCLUSIVE' | 'SHARED' | 'NORMAL' | string
  concurrentLimit: number
  runningCount: string
  queuedCount: string
  totalCurrentTasks: string
}
export interface DiagnosticInfo {
  appVersion: string
  systemOs: string
  systemFamily: string
  systemArch: string
  serviceRegion: string
  serviceHost: string
  apiKeyConfigured: boolean
  /**
   * 实际会用的出网代理（没配就是 null）。
   *
   * 为什么值得显示：reqwest 默认会读 `HTTP(S)_PROXY` 并静默使用它；而 Windows 的
   * **系统代理**（注册表）reqwest 根本不读，所以 2026-10-26 起由 daemon 自己读、
   * 自己装到客户端上。代理软件关掉之后，应用表现为"连不上平台 / key 好像坏了"，
   * 而界面上看不出任何与代理有关的线索。
   */
  outboundProxy?: string | null
  /**
   * **当前生效的运行参数**（daemon 实际用的那些值，不是界面自己猜的）。
   *
   * 全部来自环境变量或程序位置，所以"出问题能不能自己改"取决于它们**看不看得见**。
   */
  runtime?: {
    bind: string
    allowedHosts: string[]
    /** 设了它 `/bootstrap` 就跳到那个 origin（开发时是 vite）——"外壳里为什么是旧界面"的答案。 */
    uiOrigin: string | null
    /** daemon 实际在供的静态资源目录（重铺 `dist/` 之后该核对的就是它）。 */
    distDir: string
  }
  /**
   * **日志在哪、还在不在写**。
   *
   * 这一块是给"出问题以后把目录交给 agent 去发掘"用的：只报路径不够——daemon 的输出可能是
   * 别人重定向的（外壳写文件，CLI/手工可能丢进 `NUL`），那时目录里会留着一个**陈旧**的日志，
   * 看 `modifiedAtMillis` 才知道它没在写。
   */
  logs?: {
    directory: string | null
    files: Array<{ name: string; bytes: number; modifiedAtMillis: number }>
  }
  /** 没在连接上填时生效的默认值，以及 daemon 夹紧时用的范围。
   *
   * 范围也由 daemon 报：界面提示里那句"范围 10–3600"必须与真正夹紧时用的是同一组数，
   * 前端自己抄一份必然漂（这个仓库的老毛病）。
   */
  defaults?: {
    requestTimeoutSeconds: number
    trackingDeadlineSeconds: number
    requestTimeoutMin: number
    requestTimeoutMax: number
    trackingDeadlineMin: number
    trackingDeadlineMax: number
  }
}
export interface TaskResult {
  url: string | null
  outputType: string | null
  nodeId?: string
  text?: string | null
}
