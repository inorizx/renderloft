// 收图脚本 —— **给用户的示例，不是构建工具**。
//
// 异步闭环里"任务跑完了"这一跳有两种做法：daemon 主动 POST（webhook，见 README 的验签一节），
// 或者你这边**定时来收**（本脚本）。后者不需要中间人、不需要常驻进程，Windows 任务计划里挂一条
// 就够了——这是"从通知到东西到手"最省事的做法。
//
// 它**只调 CLI**（不直连 daemon、不碰数据库）：CLI 自己会在 daemon 没跑时把它拉起来。
// 状态记在 <目录>/.collected.json：收过的产物不重复下载，报过的失败不重复念叨。
// 产物名照抄平台给的；撞名由 CLI 兜底（`870.png` → `870 (1).png`），脚本只负责转述它。
//
// 用法：
//   node scripts/collect-artifacts.mjs [--to <目录>] [--limit 20] [--cli <renderloft-cli 路径>]
//                                      [--only-if-running]
//
// 挂到任务计划（每 10 分钟收一次；`--only-if-running` = daemon 没在跑就跳过，别把它拉活）：
//   schtasks /create /tn RenderLoft收图 /sc minute /mo 10 ^
//     /tr "node D:\path\to\collect-artifacts.mjs --to D:\pics --only-if-running"
import { execFileSync } from 'node:child_process'
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'

// 与 daemon 的终态判定同一组（QUEUED / RUNNING 之外的都算结束）。
const TERMINAL = new Set(['SUCCEEDED', 'FAILED', 'CANCELLED', 'PARTIAL'])

function parseArgs(argv) {
  const options = { to: 'renderloft-artifacts', limit: 20, cli: 'renderloft-cli', onlyIfRunning: false }
  for (let index = 0; index < argv.length; index += 1) {
    const name = argv[index]
    if (name === '--to') options.to = argv[(index += 1)]
    else if (name === '--limit') options.limit = Number(argv[(index += 1)])
    else if (name === '--cli') options.cli = argv[(index += 1)]
    else if (name === '--only-if-running') options.onlyIfRunning = true
    else throw new Error(`不认识的参数：${name}`)
  }
  return options
}

// CLI 的每个回答都是一层信封（`{ok, data}` 或 `{ok:false, error}`）；失败时退出码非零，
// 但这里仍然读信封——错误信息比退出码有用。
function runCli(cli, args) {
  let stdout
  try {
    stdout = execFileSync(cli, args, { encoding: 'utf8' })
  } catch (error) {
    stdout = error.stdout ?? ''
    if (!stdout.trim()) throw new Error(`调用 ${cli} 失败：${error.message}`)
  }
  const envelope = JSON.parse(stdout)
  if (envelope.ok !== true) {
    throw new Error(`${args.join(' ')} 失败：${envelope.error?.message ?? '未知错误'}`)
  }
  return envelope.data
}

// 后台任务**不能是哑的**：挂到任务计划之后 stdout 没人看，失败了也无从得知。
// 所以每轮都在目录里追加一行（时间 + 结果），这是排查"为什么没收到图"的唯一线索。
function appendLog(line) {
  appendFileSync(LOG, `${new Date().toISOString()} ${line}\n`, 'utf8')
}

const options = parseArgs(process.argv.slice(2))
const target = resolve(options.to)
mkdirSync(target, { recursive: true })
const stateFile = join(target, '.collected.json')
const LOG = join(target, '.collect.log')
const previous = existsSync(stateFile) ? JSON.parse(readFileSync(stateFile, 'utf8')) : {}
const collected = new Set(previous.artifactIds ?? [])
const reported = new Set(previous.reportedFailures ?? [])

// 定时跑的时候用 `--only-if-running`：daemon 没在跑就跳过，**别把它拉起来**。
// 理由：`operation list` 这类普通动词会在 daemon 不在时把它启动（"启动但不拥有"），
// 而一个每 10 分钟跑一次的后台任务因此会把你**手动停掉**的 daemon 反复拉活——那不是它该管的事。
// `daemon status` 恰好是唯一不自动拉起的动词（见 README），所以这里问它。
if (options.onlyIfRunning) {
  const status = runCli(options.cli, ['daemon', 'status'])
  if (status.running !== true) {
    console.log('daemon 没在跑——跳过（--only-if-running）')
    appendLog('跳过：daemon 没在跑')
    process.exit(0)
  }
}

// ⚠️ `operation list` 给的是**摘要**（operationId / status / runCount），**没有 runs**——
// 所以要按 runId 找产物，得对已结束的任务各问一次 `operation get`（实测踩到：直接读
// `item.runs` 会得到 undefined，于是"一个产物都匹配不上"却看起来一切正常）。
const operations = runCli(options.cli, ['operation', 'list', '--limit', String(options.limit)])
  .items ?? []
const finished = operations.filter((operation) => TERMINAL.has(operation.status))
const runIds = new Set()
const failures = []
for (const operation of finished) {
  const detail = runCli(options.cli, ['operation', 'get', '--operation', String(operation.operationId)])
  for (const run of detail.runs ?? []) runIds.add(run.runId)
  if (operation.status !== 'SUCCEEDED') failures.push({ operation, detail })
}

const artifacts = runCli(options.cli, ['artifact', 'list']).items ?? []
const wanted = artifacts.filter(
  (artifact) => runIds.has(artifact.runId) && !collected.has(artifact.artifactId),
)

for (const artifact of wanted) {
  // 产物名照抄平台给的——**CLI 现在自己兜底撞名**（`--to <目录>` 默认 on-conflict=rename，
  // `870.png` → `870 (1).png`），所以这里不再加 `runId-` 前缀：那是把内部 id 塞进用户文件名，
  // 而"哪个文件属于哪个 run"该由 `.collected.json` 这类元数据回答，不是文件名。
  const result = runCli(options.cli, [
    'artifact',
    'download',
    '--artifact',
    String(artifact.artifactId),
    '--to',
    target,
  ])
  collected.add(artifact.artifactId)
  // CLI 撞名时会在 note 里说明改存到哪——**必须转述**，否则"我收的 870.png 去哪了"要人翻目录。
  const note = result.note ? `（${result.note}）` : ''
  console.log(`收到 ${result.path}（${(result.bytes / 1024 / 1024).toFixed(2)} MB，run ${artifact.runId}）${note}`)
}

// 失败的也说出来（否则"没收到图"看起来像脚本坏了），但**只报新的**：定时跑时旧失败是噪音。
for (const { operation, detail } of failures) {
  if (reported.has(operation.operationId)) continue
  reported.add(operation.operationId)
  const reason = (detail.runs ?? [])
    .map((run) => run.error?.message)
    .filter(Boolean)
    .join('；')
  console.log(`注意：任务 ${operation.operationId} 是 ${operation.status}${reason ? `——${reason}` : ''}`)
}

writeFileSync(
  stateFile,
  `${JSON.stringify({ artifactIds: [...collected], reportedFailures: [...reported] }, null, 2)}\n`,
  'utf8',
)
const summary =
  wanted.length === 0
    ? `没有新的产物（已结束 ${finished.length} 条任务，目录里累计 ${collected.size} 个）`
    : `本轮收到 ${wanted.length} 个，都在 ${target}`
console.log(summary)
appendLog(summary)
