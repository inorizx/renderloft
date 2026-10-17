// MCP 常驻成本的量具：驱动**真的** `renderloft-mcp.exe`（stdio），量三件事——
//
//   ① 常驻成本：`instructions` + `tools/list` 全文（每一轮都要付的那部分）
//   ② 平铺假设：把这 N 个动词当成原生工具，各自"描述 + schema"求和（当前形状省下的就是它）
//   ③ 索引体量：`catalog` 无参返回的索引（按需取，不算常驻）
//
// 为什么要有它：README 与 `docs/mcp-surface.md` 都拿"常驻多少 token"当论据，而**手抄的数字
// 会漂**（实测过一次：改了 `instructions` 与标记之后，`docs/mcp-surface.md` 里的 ≈790
// 立刻不再是真值）。
// 数字要有来源、要能复量，所以量具进仓库，而不是把结论抄进文档。
//
// ⚠️ 安全边界：**只读**——`initialize` / `tools/list` / `catalog`，不提交任何任务、不花额度。
//
// 用法（先构建；默认用 debug 产物，与 `mcp-sweep.mjs` 同一约定）：
//   cargo build --bin renderloft-mcp
//   node scripts/mcp-cost.mjs [renderloft-mcp 的路径]
//
// 口径：英文约 4 字符 / token（粗略但一致——两边用同一把尺，比较才有意义）。
import { spawn } from 'node:child_process'
import { existsSync } from 'node:fs'
import { join, resolve } from 'node:path'

const repoRoot = resolve(import.meta.dirname, '..')
const exe =
  process.argv[2] ??
  join(
    repoRoot,
    'src-tauri',
    'target',
    'debug',
    process.platform === 'win32' ? 'renderloft-mcp.exe' : 'renderloft-mcp'
  )
if (!existsSync(exe)) {
  console.error(`找不到 ${exe}\n先跑：cargo build --bin renderloft-mcp`)
  process.exit(1)
}

const tokens = (text) => Math.round(text.length / 4)
const child = spawn(exe, [], { stdio: ['pipe', 'pipe', 'inherit'] })
const replies = []
let buffer = ''
child.stdout.on('data', (chunk) => {
  buffer += chunk.toString()
  let index
  while ((index = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, index).trim()
    buffer = buffer.slice(index + 1)
    if (!line) continue
    try {
      replies.push(JSON.parse(line))
    } catch {
      /* 非 JSON 行（启动日志之类）忽略 */
    }
  }
})

const send = (message) => child.stdin.write(JSON.stringify(message) + '\n')
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
const answer = async (id, timeoutMs = 8000) => {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    const found = replies.find((reply) => reply.id === id)
    if (found) return found
    await sleep(80)
  }
  return undefined
}

send({
  jsonrpc: '2.0',
  id: 1,
  method: 'initialize',
  params: {
    protocolVersion: '2024-11-05',
    capabilities: {},
    clientInfo: { name: 'mcp-cost', version: '1' }
  }
})
const init = await answer(1)
if (!init) {
  console.error('initialize 没有回应——daemon 起来了吗？')
  child.kill()
  process.exit(1)
}
send({ jsonrpc: '2.0', method: 'notifications/initialized' })

send({ jsonrpc: '2.0', id: 2, method: 'tools/list' })
const tools = (await answer(2))?.result?.tools ?? []
send({ jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'renderloft_catalog', arguments: {} } })
const indexText = (await answer(3))?.result?.content?.[0]?.text ?? ''

// 索引里列出的动词 id = 平铺时会有多少个工具
let verbIds = []
try {
  const parsed = JSON.parse(indexText)
  const list = Array.isArray(parsed) ? parsed : (parsed.verbs ?? parsed.entries ?? [])
  verbIds = list.map((verb) => verb.id ?? verb.verb).filter(Boolean)
} catch {
  /* 退回正则 */
}
if (verbIds.length === 0) {
  verbIds = [...indexText.matchAll(/"id"\s*:\s*"([a-z]+\.[a-z-]+)"/g)].map((match) => match[1])
}

const perVerb = []
for (const [offset, id] of verbIds.entries()) {
  const requestId = 1000 + offset
  send({
    jsonrpc: '2.0',
    id: requestId,
    method: 'tools/call',
    params: { name: 'renderloft_catalog', arguments: { verb: id } }
  })
}
await sleep(1500 + verbIds.length * 120)
for (const [offset, id] of verbIds.entries()) {
  const text = (await answer(1000 + offset, 2000))?.result?.content?.[0]?.text ?? ''
  perVerb.push({ id, tokens: tokens(text) })
}

const instructions = init.result?.instructions ?? ''
const toolsText = tools
  .map((tool) => `${tool.name}\n${tool.description ?? ''}\n${JSON.stringify(tool.inputSchema ?? {})}`)
  .join('\n')
const resident = tokens(instructions) + tokens(toolsText)
const flat = perVerb.reduce((sum, verb) => sum + verb.tokens, 0)

console.log('=== 常驻（每一轮都要付）===')
console.log(`  instructions           ≈ ${tokens(instructions)} tokens`)
console.log(`  tools/list（${tools.length} 个工具） ≈ ${tokens(toolsText)} tokens`)
console.log(`  合计                   ≈ ${resident} tokens（128k 上下文的 ${((resident / 128000) * 100).toFixed(2)}%）`)
console.log(`\n=== 平铺假设（把 ${verbIds.length} 个动词做成原生工具）===`)
console.log(`  合计                   ≈ ${flat} tokens（128k 上下文的 ${((flat / 128000) * 100).toFixed(2)}%）`)
console.log(`  最贵的几个：${perVerb
  .sort((a, b) => b.tokens - a.tokens)
  .slice(0, 4)
  .map((verb) => `${verb.id} ≈${verb.tokens}`)
  .join('  ')}`)
console.log(`\n  当前形状省下           ≈ ${flat - resident} tokens`)
console.log(`\n=== 按需（不算常驻）===`)
console.log(`  catalog 索引           ≈ ${tokens(indexText)} tokens`)

child.kill()
process.exit(0)
