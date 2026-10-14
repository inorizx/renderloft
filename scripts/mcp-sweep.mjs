// MCP 工具真跑：驱动**真的** `renderloft-mcp.exe`（stdio），把其中 12 个工具逐个调一遍
// （当前动词表里 MCP 侧共 14 个；元层那两个——catalog 与 call——不在这里扫）。
//
// 为什么要有它：这个仓库最贵的教训是「静态测试全绿，真跑才发现」。第一次做全工具扫描时
// 就抓到两个单测覆盖不到的东西——一个动词的方法写错了（GET vs POST，拿到 405 却被当成
// "服务端就这样"），以及一个动词**只做了一半**（工具描述承诺了效果，实际只走了第一步）。
// 两次都是"请求被受理"这一层测过、真正的效果没人测。所以改完动词表值得跑一次它：
// 它验的是**真进程 + 真 HTTP + 真参数**，而不是"函数被调用了"。
//
// ⚠️ **安全边界（这个脚本不许花钱）**：
//   * `run_recipe` 只跑错误路径（不存在的 revision），并断言"没有新 Operation 被创建"；
//   * `import_recipe` 用一个**不存在**的 webapp id（真网络往返，但不落库）；
//   * 其余都是只读，或在**终态**对象上做幂等写（cancel 一个已结束的任务）。
//   * 唯一留下的痕迹是一次真实上传（一个 1x1 PNG 资产）——那正是要验的路径。
//
// 用法（需要 daemon 与 `renderloft-mcp.exe` 都是**当前**构建）：
//   cargo build --bin renderloft-mcp
//   node scripts/mcp-sweep.mjs
//
// 它**不在 `pnpm verify` 里**：要真 daemon、真网络、真 LLM 一轮，不适合当门禁。
import { spawn } from 'node:child_process'
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs'
import { join, resolve } from 'node:path'

const repoRoot = resolve(import.meta.dirname, '..')
const mcpExe = join(
  repoRoot,
  'src-tauri',
  'target',
  'debug',
  process.platform === 'win32' ? 'renderloft-mcp.exe' : 'renderloft-mcp'
)
const outDir = join(process.env.TEMP ?? '.', 'sg-mcp-sweep')
mkdirSync(outDir, { recursive: true })

// 数据目录名与 daemon 同源（`daemon.rs` 是唯一所有者，`check-layers.mjs` 会盯着这几个消费者，
// 这个文件也在名单里）。⚠️ 写错名字的后果不是"报错"，而是**一跑就 ENOENT**——所以失败时要把
// 找过的路径打出来，而不是甩一个 readFileSync 的堆栈。
const endpointFile = join(
  process.env.LOCALAPPDATA ?? '',
  'renderloft-workspace',
  'daemon-endpoint.json'
)
let token
try {
  token = JSON.parse(readFileSync(endpointFile, 'utf8')).token
} catch (error) {
  console.error(`读不到 daemon 端点：${endpointFile}`)
  console.error(`  ${error.message}`)
  console.error('  先让 daemon 跑起来（例如启动一次应用，或 renderloft-cli daemon status）。')
  process.exit(1)
}
const base = 'http://127.0.0.1:31337/api/v1'
const headers = { authorization: `Bearer ${token}`, 'content-type': 'application/json' }
const api = async (method, path, body) => {
  const response = await fetch(`${base}${path}`, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body)
  })
  return { status: response.status, body: await response.json() }
}

const results = []
const record = (name, ok, detail) => {
  results.push({ name, ok, detail })
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}  ${detail}`)
}
// 前置状态不满足时用 SKIP：**不算失败，但必须说出来**。把"前置不满足"混进 FAIL 会让人
// 去查一个没坏的东西；让它崩（曾经如此）更糟——看起来像工具坏了。
const skip = (name, detail) => {
  results.push({ name, ok: true, detail, skipped: true })
  console.log(`SKIP  ${name}  ${detail}`)
}

// ---------- MCP 进程（stdio 是换行分隔的 JSON-RPC）----------
const proc = spawn(mcpExe, [], { stdio: ['pipe', 'pipe', 'inherit'] })
let buffer = ''
const pending = new Map()
proc.stdout.on('data', (chunk) => {
  buffer += chunk.toString('utf8')
  let index
  while ((index = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, index).trim()
    buffer = buffer.slice(index + 1)
    if (!line) continue
    let message
    try {
      message = JSON.parse(line)
    } catch {
      continue
    }
    if (pending.has(message.id)) {
      pending.get(message.id)(message)
      pending.delete(message.id)
    }
  }
})
let nextId = 0
const request = (method, params) =>
  new Promise((resolve) => {
    const id = ++nextId
    pending.set(id, resolve)
    proc.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
  })
const tool = async (name, args = {}) => {
  const response = await request('tools/call', { name, arguments: args })
  const result = response.result ?? {}
  return {
    isError: result.isError === true,
    data: result.structuredContent,
    error: result.structuredContent?.error
  }
}

// ---------- 前置：库与任务现状 ----------
const recipesBefore = (await api('GET', '/recipes?limit=200')).body.data.items
const operationsBefore = (await api('GET', '/operations?limit=50')).body.data.items
const terminalOperation = operationsBefore.find((item) => item.status === 'FAILED')
const realArtifact = (await api('GET', '/artifacts')).body.data.items[0]
const connectionId = recipesBefore[0]?.defaultConnectionId
console.log(
  `前置：recipes=${recipesBefore.length} operations=${operationsBefore.length} ` +
    `artifact=${realArtifact?.artifactId} connection=${connectionId}`
)

// 1) tools/list —— 形状断言：**常驻只有元层那两个**（catalog + call）。
//
// ⚠️ 这里曾经断言 `=== 12`（扁平工具时代的数字）：元层落地后它一直是红的，而"全工具真跑"
// 一旦长期红着，就没人再看它的输出了——一个永远失败的门禁等于没有门禁。
// 现在钉**形状**；至于下面那 12 个动词，用**旧名字**（`renderloft_*`）照样调得通，
// 那是给缓存过工具列表的调用方留的兼容路径（`mcp.rs` 的 `unwrap_or(verb_id)`）——
// 所以这个脚本本身也在验兼容路径没断。
const listed = await request('tools/list', {})
const toolNames = listed.result.tools.map((item) => item.name)
record(
  'tools/list',
  toolNames.length === 2 &&
    toolNames.includes('renderloft_catalog') &&
    toolNames.includes('renderloft_call'),
  `${toolNames.length} 个工具：${toolNames.join(', ')}`
)

// 2) list_recipes
const listRecipes = await tool('renderloft_list_recipes', { limit: 5 })
record(
  'list_recipes',
  !listRecipes.isError && (listRecipes.data?.items?.length ?? 0) > 0,
  `items=${listRecipes.data?.items?.length} 第一条=${listRecipes.data?.items?.[0]?.name}`
)

// 3) describe_recipe
const describe = await tool('renderloft_describe_recipe', { recipeId: recipesBefore[0].recipeId })
record(
  'describe_recipe',
  !describe.isError && typeof describe.data?.inputSchema === 'object',
  `字段=${Object.keys(describe.data?.inputSchema?.properties ?? {}).join(',')}`
)

// 4) import_recipe：**不存在**的 webapp id（真网络往返，不落库）
const importResult = await tool('renderloft_import_recipe', {
  connectionId,
  source: '2084540538883235842'
})
const recipesAfterImport = (await api('GET', '/recipes?limit=200')).body.data.items
record(
  'import_recipe（错误路径，不落库）',
  importResult.isError && recipesAfterImport.length === recipesBefore.length,
  `code=${importResult.error?.code} 库仍为 ${recipesAfterImport.length} 条`
)

// 5) run_recipe：只跑错误路径，且必须**没有新 Operation**
const runResult = await tool('renderloft_run_recipe', {
  recipeRevisionId: 999999,
  inputs: { prompt: 'mcp sweep - must not be submitted' }
})
const operationsAfterRun = (await api('GET', '/operations?limit=50')).body.data.items
record(
  'run_recipe（错误路径，不提交）',
  runResult.isError && operationsAfterRun.length === operationsBefore.length,
  `code=${runResult.error?.code} operations 仍为 ${operationsAfterRun.length}`
)

// 6) get_operation / 7) wait_operation / 8) cancel_operation（在**终态**任务上）
//
// ⚠️ 这三步依赖"工作区里有一个已终态（FAILED）的任务"。曾经直接取 `terminalOperation.operationId`：
// 工作区里没有失败任务时，整个脚本崩在 `TypeError: Cannot read properties of undefined` 上——
// 看起来像 MCP 坏了，实际只是**前置状态不满足**。前置不满足要 SKIP 并说明。
if (!terminalOperation) {
  skip(
    'get/wait/cancel_operation（在终态任务上）',
    '工作区里没有已终态（FAILED）的任务——先跑一个会失败的任务再扫'
  )
} else {
  const getOperation = await tool('renderloft_get_operation', {
    operationId: terminalOperation.operationId
  })
  record(
    'get_operation',
    !getOperation.isError && (getOperation.data?.runs?.length ?? 0) > 0,
    `status=${getOperation.data?.status} runs=${getOperation.data?.runs?.length}`
  )
  const waitOperation = await tool('renderloft_wait_operation', {
    operationId: terminalOperation.operationId,
    timeoutSeconds: 1
  })
  record(
    'wait_operation（终态立即返回）',
    !waitOperation.isError && waitOperation.data?.complete === true,
    `complete=${waitOperation.data?.complete}`
  )
  const cancelOperation = await tool('renderloft_cancel_operation', {
    operationId: terminalOperation.operationId
  })
  record(
    'cancel_operation（已终态 ⇒ 幂等空操作）',
    !cancelOperation.isError && Array.isArray(cancelOperation.data?.alreadyTerminal),
    `alreadyTerminal=${JSON.stringify(cancelOperation.data?.alreadyTerminal)}`
  )
}

// 9) list_artifacts / 10) get_artifact
const listArtifacts = await tool('renderloft_list_artifacts')
record(
  'list_artifacts',
  !listArtifacts.isError && Array.isArray(listArtifacts.data?.items),
  `items=${listArtifacts.data?.items?.length}`
)
const getArtifact = await tool('renderloft_get_artifact', { artifactId: realArtifact.artifactId })
record(
  'get_artifact',
  !getArtifact.isError && getArtifact.data?.filename === realArtifact.filename,
  `filename=${getArtifact.data?.filename} byteSize=${getArtifact.data?.byteSize}`
)

// 11) upload_asset：真的传一个 1x1 PNG（本机资产，不花钱）
const pngPath = join(outDir, 'mcp-probe-1x1.png')
writeFileSync(
  pngPath,
  Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
    'base64'
  )
)
const upload = await tool('renderloft_upload_asset', { path: pngPath })
record(
  'upload_asset',
  !upload.isError && typeof upload.data?.assetId === 'number',
  `assetId=${upload.data?.assetId} byteSize=${upload.data?.byteSize}`
)

// 12) list_operations：MCP 也能列出任务（"我刚才提交的那个呢"）
// ⚠️ 窗口要够大：挑出来的那条 FAILED 是"最新的一条失败"，而它上面可能已经堆了
// 若干条成功任务——`limit: 5` 会因此落空（实测过，与产品无关，是探针自己的假设太紧）。
//
// ⚠️ 没有终态任务时**不能**去取它的 id（同一个 TypeError 的第二处）：退化成"能列出任务、
// 且条数与 REST 侧一致"，这一条照样有意义（列表本身与前置状态无关）。
const listOperations = await tool('renderloft_list_operations', { limit: 20 })
const listedOperationIds = (listOperations.data?.items ?? []).map((item) => item.operationId)
record(
  'list_operations',
  !listOperations.isError &&
    listedOperationIds.length > 0 &&
    (!terminalOperation || listedOperationIds.includes(terminalOperation.operationId)),
  `items=${listedOperationIds.length} 含终态任务=${
    terminalOperation ? listedOperationIds.includes(terminalOperation.operationId) : '（无终态任务可查）'
  }`
)

// 18) list_connections：Agent 要 connectionId 时的正路（库里一条 recipe 都没有时也能拿到）
// ⚠️ 返回体是 `{items:[...]}`，与 `list_recipes` 同一个信封形状——这里曾经直接 `.map`，
// 于是崩在 "`.map` is not a function" 上（探针自己的假设错，不是工具错）。
const listConnections = await tool('renderloft_list_connections', {})
const listedConnectionIds = (listConnections.data?.items ?? []).map((item) => item.connectionId)
record(
  'list_connections',
  !listConnections.isError && listedConnectionIds.includes(connectionId),
  `items=${listedConnectionIds.length} 含前置连接=${listedConnectionIds.includes(connectionId)}`
)

// ---------- 收尾：删掉探针造出来的东西 ----------
const operationsAfterAll = (await api('GET', '/operations?limit=50')).body.data.items
record(
  '全程没有新 Operation（没有花钱）',
  operationsAfterAll.length === operationsBefore.length,
  `operations ${operationsBefore.length} → ${operationsAfterAll.length}`
)

const failed = results.filter((item) => !item.ok)
console.log(`\n=== ${results.length - failed.length}/${results.length} 通过 ===`)
if (failed.length) console.log('失败：', failed.map((item) => item.name).join(', '))
proc.kill()
process.exitCode = failed.length ? 1 : 0
