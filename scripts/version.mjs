// 版本号单一来源：根 `package.json` 的 `version` 是唯一真相，其余各处必须与它一致。
//
// 为什么需要它（不是洁癖，是一个会**静默毁掉发布**的坑）：同一个版本号同时决定三件事——
//   ① 安装包文件名与 NSIS 里的版本；
//   ② daemon `/health` 与 CLI/MCP 的 `--version`（都来自 `CARGO_PKG_VERSION`）；
//   ③ 将来 updater 判断"要不要更新"。
// 三处不一致时的症状是最难查的那种：界面提示"发现新版本"，装完还是旧版；或者 release
// 写着 v0.2.0，用户装出来是 0.1.0。而**编译、测试、打包全都不会报错**——只有人肉比对才发现。
//
// 用法：
//   node scripts/version.mjs                     打印当前版本
//   node scripts/version.mjs check               校验各处一致（`pnpm verify` 与 CI 用）
//   node scripts/version.mjs check --tag v0.1.0  连 tag 一起校验（发布用）
//   node scripts/version.mjs sync                把 package.json 的版本写进其余各处
//   node scripts/version.mjs bump patch|minor|major|1.2.3
import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

const ROOT = join(import.meta.dirname, '..')

/** 版本号的持有者（唯一真相）。 */
const SOURCE_FILE = 'package.json'

/**
 * 必须与真相一致的每一处。
 *
 * `kind` 决定"怎么只改这一处、其余字节不动"：
 * - `json`  —— `"version": "x"` 那一行（**不重新序列化**：tauri.conf.json 里有些数组是
 *   单行写的，`JSON.stringify` 会把它们摊开，diff 会从一行变十几行）；
 * - `toml`  —— **只在 `[package]` 段内**替换（`[dependencies]` 里全是 `version = "1.0"`
 *   这种依赖版本，误伤它们会让依赖被钉到应用版本号上）；
 * - `lock`  —— `Cargo.lock` 里 `name = "<我们的包>"` 紧随其后的那行 `version`。
 *   不跟着改的话，下一次 `cargo` 会自己把 lock 改回来 ⇒ 工作区莫名其妙变脏。
 */
const TARGETS = [
  { file: SOURCE_FILE, kind: 'json' },
  { file: 'desktop/tauri.conf.json', kind: 'json' },
  { file: 'src-tauri/Cargo.toml', kind: 'toml' },
  { file: 'desktop/Cargo.toml', kind: 'toml' },
  { file: 'src-tauri/Cargo.lock', kind: 'lock', pkg: 'renderloft' },
  { file: 'desktop/Cargo.lock', kind: 'lock', pkg: 'renderloft-desktop' },
]

const JSON_VERSION = /^(\s*"version"\s*:\s*")([^"]+)(")/m
const TOML_VERSION = /^(version\s*=\s*")([^"]+)(")/m

const read = (file) => readFileSync(join(ROOT, file), 'utf8')
const fail = (message) => {
  console.error(message)
  process.exit(1)
}

/** `[package]` 段（到下一个 section 为止）。 */
function tomlPackageSection(text) {
  const start = text.indexOf('[package]')
  if (start < 0) return null
  const rest = text.slice(start + '[package]'.length)
  const next = rest.search(/^\[/m)
  return next < 0 ? rest : rest.slice(0, next)
}

/** `Cargo.lock` 里某个包的条目（到 `dependencies` 那一行为止就够）。 */
function lockPackageSection(text, pkg) {
  const at = text.search(new RegExp(`^name = "${pkg}"$`, 'm'))
  return at < 0 ? null : text.slice(at)
}

function locate(target) {
  const text = read(target.file)
  if (target.kind === 'json') {
    const match = text.match(JSON_VERSION)
    return match && { text, found: match[2], replace: (v) => text.replace(JSON_VERSION, `$1${v}$3`) }
  }
  if (target.kind === 'toml') {
    const section = tomlPackageSection(text)
    const match = section?.match(TOML_VERSION)
    if (!match) return null
    const head = text.slice(0, text.indexOf('[package]') + '[package]'.length)
    const tail = text.slice(head.length)
    return { text, found: match[2], replace: (v) => head + tail.replace(TOML_VERSION, `$1${v}$3`) }
  }
  const section = lockPackageSection(text, target.pkg)
  const match = section?.match(TOML_VERSION)
  if (!match) return null
  const head = text.slice(0, text.length - section.length)
  return { text, found: match[2], replace: (v) => head + section.replace(TOML_VERSION, `$1${v}$3`) }
}

const semver = (value) => /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(value)

function currentVersion() {
  const source = locate(TARGETS[0])
  if (!source || !semver(source.found)) {
    fail(`${SOURCE_FILE}: 读不到合法的 semver 版本号（读到 ${JSON.stringify(source?.found ?? null)}）`)
  }
  return source.found
}

function check(args) {
  const expected = currentVersion()
  const tagIndex = args.indexOf('--tag')
  const tag = tagIndex < 0 ? null : args[tagIndex + 1]
  const problems = []

  for (const target of TARGETS.slice(1)) {
    const located = locate(target)
    if (!located) {
      problems.push(`${target.file}: 找不到版本号该在的位置（脚本与文件结构脱节了）`)
      continue
    }
    if (located.found !== expected) {
      problems.push(`${target.file}: ${located.found}（应为 ${expected}）`)
    }
  }

  if (tag !== null) {
    const fromTag = tag.replace(/^v/, '')
    if (!semver(fromTag)) {
      problems.push(`tag \`${tag}\` 不是 v<semver> 形态`)
    } else if (fromTag !== expected) {
      problems.push(`tag \`${tag}\` 与版本号 ${expected} 不一致`)
    }
  }

  if (problems.length > 0) {
    console.error('版本号不一致（真相来源：package.json）')
    problems.forEach((problem) => console.error('  ' + problem))
    console.error('  修法：node scripts/version.mjs sync（或 bump <patch|minor|major>）')
    process.exit(1)
  }
  const suffix = tag === null ? '' : `，tag ${tag} 匹配`
  console.log(`版本号守门通过：${expected}（${TARGETS.length} 处一致${suffix}）`)
}

function sync(quiet = false) {
  const version = currentVersion()
  const changed = []
  for (const target of TARGETS.slice(1)) {
    const located = locate(target)
    if (!located) fail(`${target.file}: 找不到版本号该在的位置`)
    if (located.found === version) continue
    writeFileSync(join(ROOT, target.file), located.replace(version))
    changed.push(`${target.file}: ${located.found} → ${version}`)
  }
  if (!quiet) {
    if (changed.length === 0) console.log(`无需同步：各处已经是 ${version}`)
    else {
      console.log(`已同步到 ${version}：`)
      changed.forEach((line) => console.log('  ' + line))
    }
  }
  return changed
}

function bump(spec) {
  if (!spec) fail('用法：node scripts/version.mjs bump <patch|minor|major|1.2.3>')
  const current = currentVersion()
  const [major, minor, patch] = current.split(/[-+]/)[0].split('.').map(Number)
  let next
  if (spec === 'patch') next = `${major}.${minor}.${patch + 1}`
  else if (spec === 'minor') next = `${major}.${minor + 1}.0`
  else if (spec === 'major') next = `${major + 1}.0.0`
  else if (semver(spec)) next = spec
  else fail(`不认识的目标版本 \`${spec}\`（用 patch|minor|major 或 1.2.3）`)

  const source = locate(TARGETS[0])
  writeFileSync(join(ROOT, SOURCE_FILE), source.replace(next))
  console.log(`${SOURCE_FILE}: ${current} → ${next}`)
  sync(true).forEach((line) => console.log('  ' + line))
  console.log(`下一步：git commit -am "chore(release): v${next}" && git tag v${next} && git push --tags`)
}

const [command, ...args] = process.argv.slice(2)
if (command === undefined) console.log(currentVersion())
else if (command === 'check') check(args)
else if (command === 'sync') sync()
else if (command === 'bump') bump(args[0])
else fail(`不认识的命令 \`${command}\`（可用：check / sync / bump）`)
