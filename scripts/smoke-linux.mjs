// Linux 冒烟：从**构建产物**（tar.gz）解压，把三入口真用一遍。
//
// 为什么必须「从解压包」验、而不是跑 src-tauri/target 里的二进制：打包路径烂掉的方式
// 是"包在、但里面少了东西"——Windows 免安装包第一版就少了 dist/，装到别的机器上
// 控制台只回一屏 JSON。所以这里从用户真正拿到手的那个包开始：
//   解压 → 件套齐全 → CLI 拉起 daemon → 控制台 200 → MCP 握手 → daemon stop → 端口释放
// 任何一步失败都红（exit 1），并尽量把 daemon.log 的尾巴打出来。
//
// 用法：在仓库根运行 `node scripts/smoke-linux.mjs [tarball 路径]`。
// 默认在仓库根找**恰好一个** RenderLoft_*_x64-linux.tar.gz（多了说明有旧产物混进来，宁可红）。
import { spawn, spawnSync } from 'node:child_process'
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs'
import { homedir, tmpdir } from 'node:os'
import path from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'

const CONSOLE_URL = 'http://127.0.0.1:31337/'
const root = process.cwd()

main().catch((err) => {
  console.error(`✗ 冒烟异常：${err?.message ?? err}`)
  process.exitCode = 1
})

async function main() {
  if (process.platform !== 'linux') {
    console.error('smoke-linux 要在 Linux 上运行（它冒烟的就是 Linux 便携包）')
    process.exitCode = 1
    return
  }

  // 0) 端口预检：31337 上已有 daemon 在响应时先停 —— 否则后面的 stop 会误关"别人的" daemon，
  // 而"包里三件套能不能用"根本没被验证。
  if (await portOccupied()) {
    console.error('31337 上已有 daemon 在响应（可能是你本机自己的）。先停掉它再跑冒烟。')
    process.exitCode = 1
    return
  }

  let tarball = process.argv[2]
  if (!tarball) {
    const candidates = readdirSync(root).filter((name) => /^RenderLoft_.*-linux\.tar\.gz$/.test(name))
    if (candidates.length !== 1) {
      console.error(`期望恰一个 RenderLoft_*_x64-linux.tar.gz，实际 ${candidates.length} 个：${candidates.join('、') || '（没有）'}`)
      process.exitCode = 1
      return
    }
    tarball = path.join(root, candidates[0])
  }
  if (!existsSync(tarball)) {
    console.error(`找不到产物：${tarball}（先跑 pnpm build:linux）`)
    process.exitCode = 1
    return
  }

  const workdir = mkdtempSync(path.join(tmpdir(), 'renderloft-smoke-'))
  const cli = (args, timeout = 60000) =>
    spawnSync(path.join(workdir, 'renderloft-cli'), args, { cwd: workdir, encoding: 'utf8', timeout })
  let daemonInvolved = false

  try {
    const untar = spawnSync('tar', ['-xzf', tarball, '-C', workdir], { encoding: 'utf8' })
    if (untar.status !== 0) throw new Error(`解压失败：${untar.stderr}`)
    console.log(`✓ 解压 ${path.basename(tarball)}`)

    // 1) 件套齐全 —— "静默烂掉"的两个主要位置：少二进制、少 dist/
    for (const name of ['renderloft', 'renderloft-cli', 'renderloft-mcp']) {
      if (!existsSync(path.join(workdir, name))) throw new Error(`包里少了 ${name}`)
    }
    if (!existsSync(path.join(workdir, 'dist', 'index.html'))) throw new Error('包里少了 dist/index.html —— 控制台会变成一屏 JSON')
    console.log('✓ 三件套 + dist/index.html 都在')

    // 2) CLI 拉起 daemon（包里第一个被真用的入口）
    daemonInvolved = true
    const listed = cli(['recipe', 'list'])
    if (listed.status !== 0) throw new Error(`renderloft-cli recipe list 退出码 ${listed.status}：${listed.stderr || listed.stdout}`)
    const listedEnvelope = JSON.parse(listed.stdout.trim())
    if (listedEnvelope.ok !== true) throw new Error(`recipe list 信封 ok≠true：${listed.stdout}`)
    console.log('✓ CLI 可用（daemon 已自动拉起）')

    // 3) daemon status
    const status = cli(['daemon', 'status'])
    let statusEnvelope = {}
    try {
      statusEnvelope = JSON.parse(status.stdout.trim())
    } catch {
      // 解析失败就按失败处理（下面的断言会红）
    }
    if (status.status !== 0 || statusEnvelope.data?.running !== true) throw new Error(`daemon 不在 running：${status.stdout || status.stderr}`)
    console.log('✓ daemon status = running')

    // 4) 控制台 200（轮询：daemon 刚起时静态资源就绪可能有几百毫秒差）
    const consoleRes = await waitConsole(30000)
    console.log(`✓ 控制台 ${CONSOLE_URL} → HTTP ${consoleRes.status}（${consoleRes.contentType}）`)

    // 5) MCP 握手（第三个入口）
    const mcpOut = await mcpHandshake(workdir)
    if (!mcpOut.includes('"tools"')) throw new Error('tools/list 回执里没有 tools')
    console.log('✓ MCP initialize + tools/list 握手成功')

    // 6) 收尾：stop → 端口必须真的释放
    const stopped = cli(['daemon', 'stop'])
    if (stopped.status !== 0) throw new Error(`daemon stop 失败：${stopped.stderr || stopped.stdout}`)
    await waitPortReleased(10000)
    console.log('✓ daemon stop 后端口已释放')

    console.log('\n全部通过：Linux 便携包可用（CLI / 控制台 / MCP）。')
  } catch (err) {
    console.error(`\n✗ 冒烟失败：${err?.message ?? err}`)
    // daemon 参与过才值得看它的日志（纯解包/件套失败时，日志是上一次运行留下的，容易带偏）
    if (daemonInvolved) dumpDaemonLogTail()
    process.exitCode = 1
  } finally {
    // 双保险：失败路径上把 daemon 收掉（成功路径已在 6) 停过，重复 stop 也无害）
    try {
      spawnSync(path.join(workdir, 'renderloft-cli'), ['daemon', 'stop'], { cwd: workdir, timeout: 15000 })
    } catch {
      // 尽力而为：这里失败不覆盖上面真正的失败原因
    }
    rmSync(workdir, { recursive: true, force: true })
  }
}

async function portOccupied() {
  try {
    await fetch(CONSOLE_URL, { signal: AbortSignal.timeout(1500) })
    return true
  } catch {
    return false // 连不上 = 端口空闲
  }
}

async function waitConsole(timeoutMs) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    try {
      const res = await fetch(CONSOLE_URL, { signal: AbortSignal.timeout(3000) })
      if (res.status === 200) return { status: res.status, contentType: res.headers.get('content-type') ?? '' }
    } catch {
      // daemon 还没就绪：继续等
    }
    await sleep(500)
  }
  throw new Error(`控制台在 ${timeoutMs / 1000}s 内没有返回 200`)
}

async function waitPortReleased(timeoutMs) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (!(await portOccupied())) return
    await sleep(300)
  }
  throw new Error(`daemon stop 后 ${timeoutMs / 1000}s 内 31337 仍有响应`)
}

function mcpHandshake(workdir) {
  return new Promise((resolve, reject) => {
    const child = spawn(path.join(workdir, 'renderloft-mcp'), [], { cwd: workdir, stdio: ['pipe', 'pipe', 'pipe'] })
    let out = ''
    const timer = setTimeout(() => {
      child.kill()
      reject(new Error('MCP 握手 25s 超时'))
    }, 25000)
    child.stdout.on('data', (chunk) => {
      out += chunk.toString()
      if (out.includes('"id":2') || out.includes('"id": 2')) {
        clearTimeout(timer)
        child.kill()
        resolve(out)
      }
    })
    child.on('error', (err) => {
      clearTimeout(timer)
      reject(err)
    })
    child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'smoke-linux', version: '1' } } })}\n`)
    child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' })}\n`)
    child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/list' })}\n`)
  })
}

function dumpDaemonLogTail() {
  const dataHome = process.env.XDG_DATA_HOME ?? path.join(homedir(), '.local', 'share')
  const logPath = path.join(dataHome, 'renderloft-workspace', 'daemon.log')
  try {
    if (!existsSync(logPath)) return
    const tail = readFileSync(logPath, 'utf8').trimEnd().split('\n').slice(-30)
    console.error(`\n--- daemon.log 末尾（${logPath}） ---`)
    console.error(tail.join('\n'))
  } catch {
    // 日志打不出来就算了，别掩盖真正的失败原因
  }
}
