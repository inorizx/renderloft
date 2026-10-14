// 配色契约守门：把 style.css 里写着的「改色前请先跑对比度校验（双约束，不是只看白底）」
// 变成一条可执行的检查。CI 与 `pnpm verify` 调用。
//
// 为什么需要机械守卫：对比度是**跨 token 的隐式约束** —— 改一个 --border 的明度，
// 影响的是「卡片轮廓压在页面底上」这种没人会同时想到的组合；状态色还要同时满足
// 「卡片底」和「自身 12% 淡色底」两个背景（徽标就是同色文字压同色淡底）。
// 实测过一次：浅色 --border 88% 在白底上 1.33:1 看着没问题，压在页面底上只有 1.25:1。
//
// 检查项（阈值来自 :root 的契约注释）：
//   正文/小字文字          >= 4.5
//   实心底上的文字          >= 4.5
//   状态色文字角色         卡片底 >= 4.5  且  自身 12% 淡底 >= 4.5  且  mark 的 12% 淡底 >= 4.5
//   状态色标记角色         >= 2.0（圆点与文字冗余，不适用 1.4.11 的 3:1，只要求看得见）
//   装饰图标 chip          >= 3.0（非文字图形）
//   表面分离度             卡片/页面底 >= 1.05，边框 >= 1.3，输入框描边 >= 2.0，焦点环 >= 3.0
//   logo 渐变              在两种主题的卡片底上，全程 >= 3.0（非文字图形）
//   logo 品牌色相          平均色相落在 --primary 色相 ±12° 内
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

const ROOT = join(import.meta.dirname, '..')
const STYLE = readFileSync(join(ROOT, 'src', 'style.css'), 'utf8')
const LOGO = readFileSync(join(ROOT, 'src', 'assets', 'renderloft-logo.svg'), 'utf8')

const TEXT = 4.5
const GRAPHIC = 3.0

// ---------- token 解析 ----------

/** 取某个选择器块里的自定义属性（去掉注释，避免注释里的例子被当成真值）。 */
function tokenBlock(selector) {
  const start = STYLE.indexOf(`${selector} {`)
  if (start < 0) throw new Error(`style.css 里找不到 ${selector} 块`)
  const end = STYLE.indexOf('\n}', start)
  const body = STYLE.slice(start, end).replaceAll(/\/\*[\s\S]*?\*\//g, '')
  const tokens = new Map()
  for (const [, name, value] of body.matchAll(/--([\w-]+)\s*:\s*([^;]+);/g)) {
    tokens.set(name, value.trim())
  }
  return tokens
}

/** hsl(249 78% 60%) / #6149e9 → [r, g, b]（分量 0-1）。 */
function parseColor(value) {
  const hsl = value.match(/^hsl\(\s*([\d.]+)\s+([\d.]+)%\s+([\d.]+)%\s*\)$/)
  if (hsl) return hslToRgb(+hsl[1], +hsl[2], +hsl[3])
  const hex = value.match(/^#([0-9a-f]{6})$/i)
  if (hex) {
    const h = hex[1]
    return [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16) / 255)
  }
  throw new Error(`看不懂的颜色写法：${value}`)
}

function hslToRgb(h, s, l) {
  const a = (s / 100) * Math.min(l / 100, 1 - l / 100)
  const channel = (n) => {
    const k = (n + h / 30) % 12
    return (l / 100 - a * Math.max(-1, Math.min(k - 3, 9 - k, 1))) * 255
  }
  return [channel(0), channel(8), channel(4)].map((v) => v / 255)
}

/** 解析 token，跟随 var(--x) 间接引用。 */
function color(tokens, name) {
  let value = tokens.get(name)
  if (value === undefined) throw new Error(`style.css 里没有 --${name}`)
  const ref = value.match(/^var\(--([\w-]+)\)$/)
  if (ref) return color(tokens, ref[1])
  return parseColor(value)
}

const luminance = (rgb) =>
  rgb
    .map((c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4))
    .reduce((sum, c, i) => sum + c * [0.2126, 0.7152, 0.0722][i], 0)

const contrast = (a, b) => {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x)
  return (hi + 0.05) / (lo + 0.05)
}

/** 把带透明度的前景压到不透明背景上（徽标的 12% 淡底就是这么来的）。 */
const over = (fg, bg, alpha) => fg.map((c, i) => c * alpha + bg[i] * (1 - alpha))

const toHex = (rgb) =>
  '#' + rgb.map((c) => Math.round(c * 255).toString(16).padStart(2, '0')).join('')

// ---------- 契约 ----------

const failures = []
let checks = 0

function check(theme, label, fg, bg, min) {
  checks += 1
  const ratio = contrast(fg, bg)
  if (ratio < min) {
    failures.push(
      `${theme} ${label}: ${toHex(fg)} on ${toHex(bg)} = ${ratio.toFixed(2)}:1（需 ≥${min}）`,
    )
  }
}

/** 一个主题的全部 token 契约。 */
function checkTheme(theme, selector) {
  const t = tokenBlock(selector)
  const c = (name) => color(t, name)
  const card = c('card')
  const bg = c('background')

  // 文字：卡片底与页面底都要过
  for (const name of [
    'foreground',
    'muted-foreground',
    'secondary-foreground',
    'primary',
    'accent-foreground',
    'destructive',
  ]) {
    check(theme, `${name} on card`, c(name), card, TEXT)
    check(theme, `${name} on background`, c(name), bg, TEXT)
  }
  // 文字：压在带色的中性/强调底上
  for (const [fg, surface] of [
    ['muted-foreground', 'muted'],
    ['accent-foreground', 'accent'],
    ['secondary-foreground', 'secondary'],
  ]) {
    check(theme, `${fg} on ${surface}`, c(fg), c(surface), TEXT)
  }
  // eyebrow 标签：11px 大写，按正文标准
  check(theme, 'glass-label on background', c('glass-label'), bg, TEXT)
  check(theme, 'glass-label on card', c('glass-label'), card, TEXT)

  // 实心底上的文字
  for (const [fg, fill] of [
    ['primary-foreground', 'primary'],
    ['destructive-foreground', 'destructive'],
    ['success-foreground', 'success'],
    ['warning-foreground', 'warning'],
    ['info-foreground', 'info'],
  ]) {
    check(theme, `${fg} on ${fill}`, c(fg), c(fill), TEXT)
  }

  // 状态色文字角色：卡片底 + 两种 12% 淡底。
  // 前者仍在用（NoticePanel 的 bg-success/10 一类取的是状态色本身），
  // 后者是 StatusBadge 的实际组合（淡底取自 mark）。
  // 状态色标记角色：圆点/描边与文字冗余（徽标里文字永远在），不承担"只靠颜色区分状态"
  // 的语义 ⇒ 不适用 1.4.11 的 3:1；但必须"看得见"，用 2:1 兜住"淡到看不见"的退化。
  for (const name of ['success', 'warning', 'info', 'destructive']) {
    const mark = c(`${name}-mark`)
    check(theme, `${name} on card`, c(name), card, TEXT)
    check(theme, `${name} on 自身 12% 淡底`, c(name), over(c(name), card, 0.12), TEXT)
    check(theme, `${name} on ${name}-mark 的 12% 淡底`, c(name), over(mark, card, 0.12), TEXT)
    check(theme, `${name}-mark on card`, mark, card, 2)
    check(theme, `${name}-mark on 自身 12% 淡底`, mark, over(mark, card, 0.12), 2)
  }

  // 表面与控件
  check(theme, 'card vs background', card, bg, 1.05)
  check(theme, 'border vs card', c('border'), card, 1.3)
  check(theme, 'border vs background', c('border'), bg, 1.3)
  check(theme, 'input vs card', c('input'), card, 2)
  check(theme, 'input vs background', c('input'), bg, 2)
  check(theme, 'ring vs background', c('ring'), bg, GRAPHIC)
  check(theme, 'ring vs card', c('ring'), card, GRAPHIC)

  // 装饰图标 chip：同色图标压自身 14% 淡底（QuickStart 的图标块）
  check(theme, 'glass-accent on 自身 14% 淡底', c('glass-accent'), over(c('glass-accent'), card, 0.14), GRAPHIC)

  return { card, primaryHue: hue(c('primary')) }
}

function hue(rgb) {
  const [r, g, b] = rgb
  const max = Math.max(r, g, b)
  const min = Math.min(r, g, b)
  if (max === min) return 0
  const d = max - min
  const h = max === r ? ((g - b) / d + (g < b ? 6 : 0)) : max === g ? (b - r) / d + 2 : (r - g) / d + 4
  return h * 60
}

const light = checkTheme('浅色', ':root')
const dark = checkTheme('深色', '.dark')

// ---------- logo 渐变 ----------

const stops = [...LOGO.matchAll(/stop-color="(#[0-9a-f]{6})"/gi)].map((m) => parseColor(m[1]))
if (stops.length < 2) {
  failures.push('src/assets/renderloft-logo.svg: 找不到两个渐变停靠点')
} else {
  const [from, to] = stops
  const samples = Array.from({ length: 201 }, (_, i) =>
    from.map((c, ch) => c + ((to[ch] - c) * i) / 200),
  )
  // 图形门槛：整个渐变在两种主题的卡片底上都要 ≥3:1（侧边栏底就是 --card）
  for (const [theme, { card }] of [
    ['浅色', light],
    ['深色', dark],
  ]) {
    checks += 1
    const ratios = samples.map((s) => contrast(s, card))
    const weak = ratios.filter((r) => r < GRAPHIC).length
    if (weak > 0) {
      failures.push(
        `logo 渐变在${theme}卡片底 ${toHex(card)} 上有 ${weak}/${ratios.length} 段低于 3:1` +
          `（最弱 ${Math.min(...ratios).toFixed(2)}:1，两端 ${toHex(from)} → ${toHex(to)}）`,
      )
    }
  }
  // 品牌色相：平均色相必须落在 --primary ±12° 内。
  // 这条是设计契约，不是无障碍要求 —— 2026-10 那版「青 → 紫」渐变平均色相 228°，
  // 而品牌色相是 249°，标记整体读起来仍是蓝的（59.7% 的墨迹落在 198-229°）。
  // 要故意做双色相 logo，就同时改这里和 :root 的注释，别让守卫默默失效。
  for (const [theme, { primaryHue }] of [
    ['浅色', light],
    ['深色', dark],
  ]) {
    checks += 1
    const meanHue = hue(samples.reduce((acc, s) => acc.map((c, i) => c + s[i] / 201), [0, 0, 0]))
    const drift = Math.abs(((meanHue - primaryHue + 540) % 360) - 180)
    if (drift > 12) {
      failures.push(
        `logo 平均色相 ${meanHue.toFixed(0)}° 偏离${theme} --primary ${primaryHue.toFixed(0)}° 达 ${drift.toFixed(0)}°（上限 12°）`,
      )
    }
  }
  console.log(
    `logo 渐变 ${toHex(from)} → ${toHex(to)}：平均色相 ${hue(samples.reduce((acc, s) => acc.map((c, i) => c + s[i] / 201), [0, 0, 0])).toFixed(0)}°，` +
      `浅色卡片最弱 ${Math.min(...samples.map((s) => contrast(s, light.card))).toFixed(2)}:1，` +
      `深色卡片最弱 ${Math.min(...samples.map((s) => contrast(s, dark.card))).toFixed(2)}:1`,
  )
}

// ---------- 结论 ----------

if (failures.length > 0) {
  console.error(`配色契约违规 ${failures.length} 项（共检查 ${checks} 项）：`)
  failures.forEach((f) => console.error('  ' + f))
  console.error('契约与阈值见 src/style.css 的 :root 注释；改色前请先跑 `pnpm check:contrast`。')
  process.exit(1)
}
console.log(`配色契约守门通过：${checks} 项全过（文字 ≥4.5，状态色双约束，表面分离度，logo 渐变与品牌色相）`)
