// 分层守门：application/ 与 transports/ 的生产代码（顶层 use）不得直接
// import infrastructure 或 daemon（测试模块内允许）。CI 与 `pnpm verify` 调用。
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'

const ROOT = join(import.meta.dirname, '..')
const DIRS = [
  join(ROOT, 'src-tauri', 'src', 'v2', 'application'),
  join(ROOT, 'src-tauri', 'src', 'v2', 'transports'),
]
// 顶层 use 位于列 0；测试模块内的 use 带缩进，不计入。
const PATTERNS = [/^use crate::v2::infrastructure/, /^use crate::v2::daemon::/]

function* walk(dir) {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry)
    if (statSync(full).isDirectory()) yield* walk(full)
    else if (full.endsWith('.rs')) yield full
  }
}

const violations = []
for (const dir of DIRS) {
  for (const file of walk(dir)) {
    const lines = readFileSync(file, 'utf8').split('\n')
    lines.forEach((line, idx) => {
      if (PATTERNS.some((p) => p.test(line))) {
        violations.push(
          `${file.replace(ROOT + '\\', '').replaceAll('\\', '/')}:${idx + 1}: ${line.trim()}`,
        )
      }
    })
  }
}

if (violations.length > 0) {
  console.error(
    '分层违规：application/ 与 transports/ 生产代码不得直接 import infrastructure 或 daemon（测试模块内允许）',
  )
  violations.forEach((v) => console.error('  ' + v))
  process.exit(1)
}
console.log('分层守门通过：application/ 与 transports/ 无越层 import')

// 数据目录名守门：daemon / Rust 客户端 / 桌面外壳 / vite 代理**必须**指向同一个目录名。
//
// 为什么需要机械守卫：这四处是**手写字符串**，彼此没有任何编译期联系。不一致的后果很难
// 看出病因——代理读不到 token，每个请求都以未认证发出，界面看到的是 401「缺少 Bearer
// 认证令牌」，像是鉴权坏了，实际只是两个字符串不一致。所以：daemon 是这个名字的
// **所有者**，名单里的其余消费者必须与它一致；下面退役名单里的名字一律不许再当数据目录。
//
// `renderloft` 在名单里：它**与安装目录同名**（NSIS 默认装到 `%LOCALAPPDATA%\RenderLoft`，
// Windows 路径不区分大小写）⇒ 程序文件和用户数据混在一个文件夹。现在数据目录是
// `renderloft-workspace`，改回去等于把这个坑挖回来。
const DATA_DIR_OWNER = 'src-tauri/src/v2/daemon.rs'
const DATA_DIR_CONSUMERS = [
  'src-tauri/src/v2/clients/client.rs',
  'desktop/src/main.rs',
  'vite.config.ts',
  // ⚠️ 这个脚本也是消费者，而且**曾经漂过**：它把目录名写成 `renderloft`，于是每次运行都在
  // `readFileSync` 上抛 ENOENT——"全工具真跑"这类脚本一旦跑不起来，就没人再跑它了。
  // 名单漏一个消费者，守门就等于没守。
  'scripts/mcp-sweep.mjs',
]
const RETIRED_DATA_DIRS = ['renderloft']

// 残留检查按**用法形态**匹配，不按"文件里出现过这个字符串"：
// `desktop/src/main.rs` 里合法的 `"renderloft"` 是 daemon 的**二进制名**
// （`resources.join(if cfg!(windows) { "renderloft.exe" } else { "renderloft" })`），
// 按裸字符串匹配会误报。只有 `.join("x")`（Rust）与 `'x'`（JS/TS）才是"数据目录"的形态。
const retiredLiteral = (file, retired) =>
  file.endsWith('.ts') || file.endsWith('.mjs') ? `'${retired}'` : `.join("${retired}")`

const read = (relative) => readFileSync(join(ROOT, relative), 'utf8')
const ownerNames = [...read(DATA_DIR_OWNER).matchAll(/path\.join\("([a-z0-9-]+)"\)/g)].map(
  (match) => match[1],
)
const dataDirProblems = []
if (ownerNames.length === 0 || new Set(ownerNames).size !== 1) {
  dataDirProblems.push(
    `${DATA_DIR_OWNER}: 应当只有一种 \`path.join("<目录名>")\`，实际 ${JSON.stringify(ownerNames)}`,
  )
} else {
  const name = ownerNames[0]
  for (const file of DATA_DIR_CONSUMERS) {
    const source = read(file)
    if (!source.includes(`'${name}'`) && !source.includes(`"${name}"`)) {
      dataDirProblems.push(`${file}: 没有引用数据目录名 \`${name}\``)
    }
  }
  for (const file of [DATA_DIR_OWNER, ...DATA_DIR_CONSUMERS]) {
    const source = read(file)
    for (const retired of RETIRED_DATA_DIRS) {
      if (retired !== name && source.includes(retiredLiteral(file, retired))) {
        dataDirProblems.push(
          `${file}: 仍残留旧数据目录名 \`${retired}\`（形态 \`${retiredLiteral(file, retired)}\`）`,
        )
      }
    }
  }
}

if (dataDirProblems.length > 0) {
  console.error('数据目录名不一致：daemon / client / 外壳 / vite 代理必须同源')
  dataDirProblems.forEach((problem) => console.error('  ' + problem))
  process.exit(1)
}
console.log(`数据目录名守门通过：${DATA_DIR_OWNER} 与其余 ${DATA_DIR_CONSUMERS.length} 个消费者一致（${ownerNames[0]}）`)

// 日志轮转阈值同源守门：`daemon.log` 多长算长，**只能有一个答案**。
//
// 为什么需要机器守：这个约定有**两份实现**，而且在两个 crate 里（`src-tauri` 的
// `clients/daemon_boot.rs` 是 CLI / MCP 拉起 daemon 那条路，`desktop` 的 `main.rs` 是外壳
// 那条），彼此没有编译期联系。漂了的后果不是报错，而是"日志会不会无限长取决于谁先把 daemon
// 拉起来"——CLI / MCP 那条路此前正是只 append、从不轮转，实测本机那份长到 12 MB 还在长。
//
// ⚠️ 外壳里还有**第二个** `const LIMIT`（`shell.log` 的 1 MB，另一份文件、另一个约定），
// 所以这里按**函数体**切，不按"文件里第一个 LIMIT"——那会在有人调整函数顺序时静默取错值。
const DAEMON_LOG_LIMIT_OWNER = 'src-tauri/src/v2/clients/daemon_boot.rs'
const desktopMain = read('desktop/src/main.rs')
const shellDaemonLogFn = desktopMain.slice(desktopMain.indexOf('fn daemon_log_file'))
const logLimitSources = [
  {
    file: DAEMON_LOG_LIMIT_OWNER,
    value: read(DAEMON_LOG_LIMIT_OWNER).match(/const LOG_ROTATE_BYTES: u64 = ([^;]+);/)?.[1],
  },
  {
    file: 'desktop/src/main.rs',
    value: shellDaemonLogFn.match(/const LIMIT: u64 = ([^;]+);/)?.[1],
  },
].map(({ file, value }) => ({ file, value: value?.replace(/\s+/g, '') ?? null }))

const logLimitProblems = []
for (const { file, value } of logLimitSources) {
  if (value === null) {
    logLimitProblems.push(`${file}: 没找到轮转阈值常量（改名/挪走了？守门要跟着改，别静默放过）`)
  }
}
if (logLimitProblems.length === 0 && new Set(logLimitSources.map((s) => s.value)).size !== 1) {
  logLimitProblems.push(
    `两处阈值不一致：${logLimitSources.map((s) => `${s.file} = ${s.value}`).join(' / ')}` +
      '——daemon.log 的轮转点必须同值，否则取决于谁先拉起 daemon',
  )
}

if (logLimitProblems.length > 0) {
  console.error('日志轮转阈值不一致：daemon.log 的轮转点必须同源')
  logLimitProblems.forEach((problem) => console.error('  ' + problem))
  process.exit(1)
}
console.log(`日志轮转阈值守门通过：两处一致（${logLimitSources[0].value}）`)

// 出网守门：**每个 reqwest 客户端都必须经 `providers::proxy::configure()`**。
//
// 为什么需要机器守：代理是"装了却只装一半"最危险的那种能力——提交走了代理、查询没走，
// 用户看到的是"任务提交成功，然后永远查不到状态"；而**新加一个驱动时没人会想起这件事**。
// 判据因此落在"建客户端的那一行"上：
//   * 同一行里有 `proxy::configure(` ⇒ 合格；
//   * 紧接着 `.no_proxy()` ⇒ 合格（测试与"刻意直连"的客户端都这么写）；
//   * 其余文件在例外名单里（每条都要写**为什么**）。
const OUTBOUND_WRAPPER = 'src-tauri/src/v2/infrastructure/providers/proxy.rs'
const OUTBOUND_EXCEPTIONS = new Map([
  [OUTBOUND_WRAPPER, '这里就是装配点：测试把裸 builder 交给 apply() 正是判据本身'],
  [
    'src-tauri/src/v2/infrastructure/sqlite/exec.rs',
    '完成回调**刻意直连**：回调地址是用户自己的内网服务，塞进公网代理只会让它失败',
  ],
])
const CLIENT_LINE = /Client::(builder|new)\(\)/
const outboundProblems = []
for (const file of walk(join(ROOT, 'src-tauri', 'src'))) {
  const relative = file.replace(ROOT + '\\', '').replaceAll('\\', '/')
  if (OUTBOUND_EXCEPTIONS.has(relative)) continue
  const lines = readFileSync(file, 'utf8').split('\n')
  lines.forEach((line, index) => {
    if (!CLIENT_LINE.test(line)) return
    if (line.includes('proxy::configure(')) return
    // 往后看三行：`Client::builder()` 之后紧跟 `.no_proxy()` 是"刻意直连"的写法。
    if (lines.slice(index, index + 3).join('\n').includes('.no_proxy()')) return
    outboundProblems.push(
      `${relative}:${index + 1}: 直接建了 reqwest 客户端却没有走 proxy::configure()` +
        '（系统代理会因此漏掉；真要直连就连 .no_proxy() 并写清理由）',
    )
  })
}
if (outboundProblems.length > 0) {
  console.error('出网守门失败：新建的 HTTP 客户端必须经 providers::proxy::configure()')
  outboundProblems.forEach((problem) => console.error('  ' + problem))
  process.exit(1)
}
console.log(
  `出网守门通过：${OUTBOUND_EXCEPTIONS.size} 处例外，其余客户端都经 proxy::configure()`,
)
