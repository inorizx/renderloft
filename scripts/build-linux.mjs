// Linux 构建：无头三件套 + 控制台，打成便携 tar.gz。
//
// Linux 侧的既定形态是无头：CLI、浏览器控制台（daemon 伺服）、MCP 三个入口
// 共用同一个 daemon，不需要桌面壳——所以这里就是「三个二进制 + dist」的便携包，
// 解压即用，不碰系统目录、不写 PATH。
//
// 必须在 Linux 主机上运行：产物是宿主原生二进制，不做交叉编译（与 build-macos.mjs 同理）。
//
// 为什么必须带上 dist/：daemon 按「显式覆盖 → 可执行文件旁的 dist → 源码树」找控制台。
// 包里少了 dist，用户打开控制台只会得到一屏 JSON（Windows 免安装包踩过同一个坑）。
import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { chmodSync, cpSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, unlinkSync } from 'node:fs'
import path from 'node:path'

if (process.platform !== 'linux') {
  console.error('build-linux 必须在 Linux 主机上运行（产物是宿主原生二进制，不做交叉编译）')
  process.exit(1)
}

const root = process.cwd()
const run = (command, args, cwd = root) => {
  const result = spawnSync(command, args, { cwd, stdio: 'inherit' })
  if (result.status !== 0) process.exit(result.status ?? 1)
}

// 版本号单一来源：根 package.json（与 scripts/version.mjs 同源）。
const version = JSON.parse(readFileSync(path.join(root, 'package.json'), 'utf8')).version
const arch = process.arch === 'x64' ? 'x64' : process.arch
const stage = path.join(root, 'portable-linux')
const artifact = `RenderLoft_${version}_${arch}-linux.tar.gz`

run('pnpm', ['build'])
run('cargo', ['build', '--release', '--bin', 'renderloft', '--bin', 'renderloft-cli', '--bin', 'renderloft-mcp'], path.join(root, 'src-tauri'))

// ⚠️ 先清掉上一次的暂存目录与旧产物：打包是往已有的东西里放——不清的话，
// 上一次的包会被当成这一次的（发布流程里踩过同一个坑）。
rmSync(stage, { recursive: true, force: true })
for (const name of readdirSync(root)) {
  if (/^RenderLoft_.*-linux\.tar\.gz$/.test(name)) unlinkSync(path.join(root, name))
}
mkdirSync(stage)

const releaseDir = path.join(root, 'src-tauri', 'target', 'release')
for (const binary of ['renderloft', 'renderloft-cli', 'renderloft-mcp']) {
  const target = path.join(stage, binary)
  cpSync(path.join(releaseDir, binary), target)
  chmodSync(target, 0o755) // 复制不一定带可执行位——没有的话解压出来就是"权限不够"
}
cpSync(path.join(root, 'dist'), path.join(stage, 'dist'), { recursive: true })

run('tar', ['-czf', artifact, '-C', stage, '.'])

// 回读校验：产物必须真的存在且完整——"打包静默失败"是最难查的一种。
const listing = spawnSync('tar', ['-tzf', artifact], { cwd: root, encoding: 'utf8' })
if (listing.status !== 0 || !listing.stdout.includes('renderloft-mcp') || !listing.stdout.includes('dist/')) {
  console.error('产物不完整（缺二进制或 dist），停止')
  process.exit(1)
}
const bytes = statSync(path.join(root, artifact)).size
const sha256 = createHash('sha256').update(readFileSync(path.join(root, artifact))).digest('hex')
console.log(`产物：${artifact}（${(bytes / 1024 / 1024).toFixed(1)} MB）`)
console.log(`sha256：${sha256}`)
console.log('解压即用：./renderloft-cli recipe list 会自动拉起 daemon（控制台 URL 见 daemon 日志）；MCP 客户端指向同目录的 renderloft-mcp')
