# MCP 工具面：第一性推导与判据

> 本文只谈一件事：**RenderLoft 作为 MCP server，对外的工具面应该长什么样。**
>
> 判据不来自口味，来自公理：每条结论后面都写清楚**它被哪条公理逼出来**、**它的物理边界在哪**。
> 两个都写不出来的，就不是结论，是偏好。
>
> 引用一律用**符号名**（函数 / 常量 / 测试名），不用行号：行号是快照，改一次代码就烂；符号名可以搜。

---

## 0. 公理（物理事实，不是设计选择）

| # | 公理 | 直接后果 |
|---|---|---|
| A1 | **上下文是物理资源。** 常驻 = 每轮都付；按需 = 用一次付一次。 | 每一比特都要指明：住哪层、谁付、付几次。 |
| A2 | **模型没有跨轮记忆。** 不可见 = 不存在。 | "有什么能力"必须常驻；"怎么调"只在调用那一轮需要。 |
| A3 | **治理在调用方，而调用方只读冷启动时的静态元数据**（`tools/list`）。审批器不看工具输出。 | 任何用于门控的事实必须**逐动作**出现在静态清单里。 |
| A4 | **花钱与不可逆是不可恢复失败。** 参数错只花一轮；花错钱、删错东西不可恢复。 | 不可恢复性属于机器通道，不属于自然语言。 |
| A5 | **协议是我们的陌生人。** MCP 没有 cost / tier / 审批字段。 | 自造这些字段 = 把指令伪装成事实，等于抢治理者的位置；协议一给就作废。 |
| A6 | **JSON Schema 是唯一的机械可校验通道。** | 能写成 schema 的约束（类型、required、min/max、enum）不许只写在散文里。 |
| A7 | **代码空间 ≠ 数据空间。** 动词集有界、可静态列举；recipe 的 inputSchema、模型 id 是运行时数据、无界。 | 有界的可常驻，无界的必须取。 |

### 0.1 设计律

> **凡是服务端已经在强制的边界，都必须出现在协议地板上；只写在散文里的边界，调用方只能靠自觉。**

它**不新增任何权力**，只是把已经存在的物理边界公开。正因如此，它同时是"守界"的判据：
任何**自造**字段（分层提示、审批要求、成本等级）都越界 —— 它表达的不是"边界存在"，而是"我希望你怎么做"。

---

## 1. 前提核对（本文档自己的更正也记在这里）

### 1.1 已成立、不再讨论的两条

1. `initialize.instructions` **已经接好了**（`mcp.rs` 的 `initialize` / `server/discover` 两个分支 返回 `verbs::MCP_IDENTITY`）—— 这是 A2 的对价，已经付了。

   **那 agent 怎么知道这个 MCP 是干嘛的？** 常驻文本的**第一句**就是身份与触发条件：

   > RenderLoft — a local workbench that runs the user's own image/video generation apps on their own cloud platform accounts (RunningHub / ComfyUI), tracks the tasks in the background, and collects the resulting files. **Use it when** the user asks to generate, edit, restyle, upscale or batch images/videos — or to see which apps (recipes) they have, what a task cost, or where its outputs went. Runs spend the user's own credits: **[COST]** marks the verbs that charge their account, **[IRREVERSIBLE]** the ones that cannot be undone.

   同一句也放在 MCP 的 `initialize.instructions` 里（协议里专门放"这个 server 是干嘛的"字段）。**为什么必须说**：元层把自带领域暗示的工具名（`renderloft_run_recipe`）换成了中立的调用口，于是"这是什么"就没有别的地方可放了——**不可见就等于不存在**。

   对你的实际意义：**没在干这活的时候，它对 agent 的判断与上下文的干扰很小** —— 而"这个动词花钱 / 会破坏东西"这类风险档，就写在常驻索引的那一行上，**首行固定 ASCII 前缀**（`[COST] operation.run — 提交任务`）——不是写进句子里，也不是靠一个容易看漏的符号。
2. "挑应用"的三步流程本来就是渐进式：`recipe.list` → `recipe.describe` → `recipe.run`。这是**泛化动词 over 数据空间**（A7）＋ schema 按需取，形状是对的；工具数是 **O(能力) 不是 O(内容)**（导入 500 个应用，动词仍是常数个）。

### 1.2 本文档自身的两处更正（写"漂移"的文档先漂了两处）

1. **守门测试名不存在。** 上一版引的 `served_tools_match_the_table_in_both_directions` 在代码里搜不到 —— `docs/AGENTS.md:83` 也写着同一个不存在的名字。真正钉住"两个工具"这个形状的是：
   - `the_meta_tools_are_annotated_conservatively`（硬断言 `tools.len() == 2`）；
   - `the_index_and_the_table_agree_in_both_directions`（同样断言恰好两个）；
   - 另有 `tools_list_is_a_local_constant_without_per_app_tools`（同一个文件）钉住"没有 per-app 工具" —— 这条**保留**。
2. **"13 个动词"已过期。** 写这份时表里 `mcp_name` 非空的是 14 条，后来是 **15**（加了 `workspace.backup`），
   现在是 **16**（加了 `operation.retry`——`attention` 的 `nextActions` 早就点名了它，而它此前只有 HTTP
   路由，CLI / MCP 都够不着）；而当时**跑着的服务端索引只有 13 条** ⇒ 磁盘上的二进制比表旧。
   **判据里不许写死数字**，一律写"= 表里 `mcp_name` 非空的条数"（有一组测试就是这么断言的：
   动词数 = 表里 `mcp_name` 非空的行数）。

### 1.3 账单（实测；别再引用"~4.5k"）

| | 字符 | ≈ token（散文按 chars/4；schema 更差，约 /3） |
|---|---|---|
| 现状常驻：identity 464 ＋ 索引 658 ＋ catalog 样板 367 ＋ `call` 描述 1234 | 2 723 | **~0.7k** |
| A 之后：14 条 `mcp_description` 合计 | 5 424 | ~1.4k |
| 其中与 `call` 长段落**逐字重合**的（`operation.run` 独占，相似度 0.78） | −776 | −0.19k |
| A 之后：再加 inputSchema（34 个参数骨架 ＋ 参数级描述 1 740） | ~3.1k | ~1.0k |
| **A ＋ B ＋ D 稳态** | ~7.8k | **~2.0–2.3k** |

⇒ **A 是净增 ~1.3–1.6k token／轮**，但它是 **O(动词数) × 每动词常数**、**与 recipe 数无关**。

上一版写的"~4.5k token 降到 ~0.5k"里，那个 4.5k 大概来自**旧的 per-app 动态工具**（那个形状随 recipe 数增长）；现在的常数形状从来没有 4.5k。

另：14 条描述之间的 8-gram 冗余 ≈ 0% —— 跨动词重复**只存在于一处**（`call` ↔ `operation.run`）。所以 §D 的收益就是这 776 字符，不是"搬家顺带省很多"；D 的真正作用是**防重复**（同一条规矩写 N 遍 = O(N) 常驻）。

（口径：从 `VERBS` 表读渲染字符串统计，schema 按每参数 40 字符骨架粗估。要复核就跑脚本，别信手抄。）

#### 1.3.1 今天的形状：量出来的常驻账单

上表是**设计期的估算**。今天的形状可以直接量——`node scripts/mcp-cost.mjs`（先
`cargo build --bin renderloft-mcp`；脚本驱动真的 `renderloft-mcp.exe`，只读、不花钱）：

| | ≈ token | 占 128k 上下文 |
|---|---|---|
| **现状常驻**：`instructions` 528 ＋ 17 个工具 2 507 | **≈3 035** | 2.37% |
| 其中：描述 4 142 字符 / schema 5 422 字符 / 名字与标注 463 字符 | — | — |
| 索引本身（按需取，**不进常驻**） | ≈3 197 | — |
| 逐条取详情（16 个动词合计，按需） | ≈3 188 | — |

> **这张表在 2026-10-18 变了性质**：A＋B 落地之前，常驻是两个元工具（1 135），"平铺"只是假设
> （4 562）；现在**现状就是平铺**，所以"省下多少"这个对比没有了（量具里也删了）。
> 三次实测记录：① 加 `operation.retry` 与 `list` 的 `attention` 句：平铺 4 049 → 4 478；
> ② 给 `cancel` 补"没有句柄的 run 也在 `remoteMayContinue` 里"：4 478 → 4 562；
> ③ **A＋B**：常驻 1 135 → **3 035**（描述被裁到"只留调用前必须知道的事实"之后的结果），
> 按需的索引 4 572 → 3 197（逐动词详情跟着变短了）。
>
> ⚠️ **改了描述或标记之后必须重跑那个脚本**——手抄的数字会漂，实测过。
> 它同时是"逐动作标注必须在常驻地板上"这条决定的**可证伪账单**：常驻涨了就是代价，涨到不可接受
> 就该回去谈形状（唯一不撒谎的杠杆是 schema，见 §5.A 判据 5）。

---

## 2. 推导：形状是唯一被逼出来的

| 事实 | 被哪条公理逼出 | 住哪层 | 现状 |
|---|---|---|---|
| 14 个动词存在 | A2 | 常驻，一行一个 | ✅ 索引 `mcp_catalog()` |
| 逐动作 `annotations` | **A3 ＋ A4** | 常驻，**每个动作一格** | ❌ 只在 `catalog(<verb>)` 的返回里（`mcp_verb_detail()` 的 `annotations` 字段） |
| 成本 marker ＋ 计费粒度 | A4 ＋ A5 | 常驻，首行固定前缀 | ✅ `mcp_markers()` 渲染 `[COST]` / `[IRREVERSIBLE]`（有测试钉住集合与表一致） |
| 外壳参数 typed schema | A6 | 常驻 | ⚠️ 部分：`runs` 的 `maxItems` 已上地板；整体仍等 A（`mcp_tools()` 里 `args` 仍是无类型对象） |
| recipe 输入字段、模型 id | A7 | **按需 `describe`** | ✅ |
| 跨动词规矩（流程、付费、重试、并发上限） | A1 | `instructions` **一份** | ⚠️ 部分：三档上限已入 `instructions()`（数字直接 import 强执行方）；流程/重试叙事仍待 D |

由此：**A / C / D / F 是被推出来的，不是选的。**

而 **B（撤 `call`）是推论而非偏好** —— 一个工具只有**一格** `annotations`，装 N 个动作时物理上只能报最保守值（`mcp_tools()` 里 `call` 的 `annotations` 块，注释自己写着"按最保守的报"）：

> 准确的说法不是"`call` 不好"，而是 **`call` 与 A3 不相容**。

而且 **A 几乎不需要新代码**：`mcp_verb_detail()`今天就已经渲染出 `description` / `inputSchema` / `annotations` 三件套 —— A 只是**把这三个字段从"详情"提升为"工具"**。唯一改动点是 `mcp_tools()`，而 `tools()` 只是纯函数转发。

---

## 3. 现状与形状的差距（精确版）

### 3.1 门控信息不在审批器读的那一层

不是"信息没了"，而是**搬到了运行时**：`mcp_annotations()`逐动词如实报，`catalog(<verb>)` 的返回里就有（`the_meta_tools_are_annotated_conservatively` 的注释自己写着"单动词的精确标注在 catalog 的返回里"）。

**但对静态审批器等于没有** —— 它读 `tools/list`，不读工具输出。后果是确定的：调用方**再也分不出** `recipe.list` 是只读、`operation.cancel` 是破坏性的；它的自动批准、审批闸门、只读白名单全部读这一格。把这一格压平，等于：

> **服务端替调用方决定了它没法分级治理，只能全拦或全放。**

（措辞必须按这句写。写成"信息丢了"会被误读成"留着 catalog 就没事"。）

### 3.2 账单来自**无意的规模**，不是拼写错误

代码事实：

- `inputs` 在**花钱之前**就被 schema 校验挡掉 —— `INPUT_SCHEMA_INVALID`（`resolve_run()` 里）；
- `runs.len() > 100` 在服务端被拒（`MAX_RUNS_PER_OPERATION`）；
- 自主调用方的**并发**上限是 4（`MAX_AUTONOMOUS_CONCURRENT_OPERATIONS`）。

⇒ **拼错是免费的（代价一轮）；花错钱来自"无意的规模"。**
所以机器通道（A6）在这台 server 上的职责是 **限制规模 ＋ 暴露不可逆**，不是防拼写 —— 这条同时修正了上一版 §1.2 的措辞和 §4 的验收判据。

### 3.3 边界在代码里，却不在清单里（设计律的直接违反）

| 已强制的边界 | 在哪 | 调用方看得见吗 |
|---|---|---|
| 单 Operation ≤ 100 个 Run | `MAX_RUNS_PER_OPERATION` | ✅ 已上地板：schema 的 `maxItems` ＋ `instructions`（`the_enforced_limits_are_on_the_protocol_floor` 钉住同值） |
| 自主调用方并发 ≤ 4 | `MAX_AUTONOMOUS_CONCURRENT_OPERATIONS` | ✅ 已上地板：`instructions()` 直接 import 那个常量（不手抄） |
| MCP 结果 ≤ 20 条 / 64KB | `MCP_RESULT_MAX_ITEMS` / `MCP_RESULT_MAX_BYTES` | —（输出侧，做得对） |

而 `mcp_property()` **早就支持 min/max**，缺的只是 `Kind::JsonArray` 那一支没把它渲染成 `maxItems`。**已修**：表里 `runs` 的 `max: Some(100)` → schema 的 `maxItems`，与 `MAX_RUNS_PER_OPERATION` 同值并有测试钉住。

> **这是全文最便宜的一处修复：不用等 A，今天就能做。** 它不新增权力，只是把已经存在的边界公开 —— 正是 §0.1 那条律。

### 3.4 描述漂移（两类，但只有一类是独立问题）

1. **工具名漂移 —— 它是元层形状的产物，A 会自然消解它。** 描述里点到 **8 个工具名**，出处如下（按动词列，不按行号 —— 行号会烂，动词名可以搜）：`describe_recipe` ← `operation.run` / `recipe.import` / `recipe.import-workflow` / `recipe.list`；`get_operation` ← `operation.list` / `operation.wait`；`run_recipe` ← `operation.list` / `operation.wait` / `recipe.describe` / `recipe.list`；`list_recipes` ← `connection.list` / `recipe.describe` / `recipe.import`；`list_connections` ← `connection.models` / `recipe.import-workflow`；`import_recipe` ← `connection.list` / `recipe.list`；`wait_operation` ← `operation.run`；`list_artifacts` ← `artifact.get` —— 这些名字**实现认**（`route_tool()`，`invoke()` 的分发路径）但**不在今天的 `tools/list` 里**（那里只有 2 个名字，`invoke()` 里写明是刻意的）。守规矩的调用方只调被提供的工具 ⇒ 要么跳过，要么困惑。

   **实测（这条推翻了本文档上一版的隐含前提）**：14 个动词的描述文本里一共点到 **8 个工具名** —— 它们**全部**是 A 要列出的那 14 个名字（`renderloft_describe_recipe` / `get_operation` / `wait_operation` / `run_recipe` / `list_recipes` / `list_connections` / `import_recipe` / `list_artifacts`）；而这 8 个名字**一个也不在今天的 `tools/list` 里**。集合外多出来的只有 `renderloft_call` / `renderloft_catalog`（元层自己）与代码里的 `renderloft_run_` 前缀。

   ⇒ **A 之后这半边自动成立，不需要逐个改文案**；F 在这半边的工作只剩"加测试钉住它"（§6-3）。
   ⇒ 反过来说：**若第 0 步决定保留元层，这半边就必须手工修**（改成中性说法"先取该动词的 schema"）—— 这正是"形状决定文案"的一例。
2. **标记拼写漂移（已修）**：索引里的风险标记此前有两种拼写 —— `⚠️`（U+26A0 **U+FE0F**）与 `⚠`（裸 U+26A0）。实测 `verbs.rs` 里 18 处带 VS16、6 处不带；当时的规则是"summary 里已含 `⚠` 就不再加，否则若 `destructive` 就补 ` ⚠破坏性`" ⇒ **花钱的行走 `⚠️`，破坏性的行走 `⚠`**。

   现在标记由 `mcp_markers()` 从表里的 `spends_credits` / `destructive` 渲染，只有一种 ASCII 写法（`[COST]` / `[IRREVERSIBLE]`），而且**被服务的 MCP 文本里不允许再出现 `⚠` 或 U+FE0F** —— 由 `cost_and_irreversibility_marks_are_mechanical_and_match_the_table` 钉住（含"标记集合 == 表"这条）。

   也就是说这两类**只靠一个零宽字符区分**：人眼看不见，正则写错一个字符就漏。

   > **零宽差异不是通道。** 这是 §C 的动机。

---

## 4. 物理边界：做不到的，写成不变量

| # | 不变量 | 为什么是物理的 |
|---|---|---|
| I1 | **静态 schema 装不下数据空间**，内层字段必须 `describe`。 | recipe 的输入字段由平台在运行时给出（A7）。任何"参数不再盲写"的承诺，边界都在外壳。 |
| I2 | **机器通道能限规模，不能判语义。** | `maxItems` 能说"最多 100 次计费"，说不出"这 100 次跑的是不是你要的那 100 次"。 |
| I3 | **常数常驻不可能为零。** | A2：能力必须可见。能省的只有"怎么调"。零常驻 = 能力不存在 —— 这是当前形状的**代价**，不是优点。 |
| I4 | **一个工具只有一格 `annotations`。** | 想把 14 种动作塞进一个工具又要逐动作标注，物理上做不到。 |

---

## 5. 要做什么（A–G，每条带判据）

### A. 16 个动词还原成原生 MCP tool（主刀）—— ✅ 已落地（2026-10-18）

**机械搬运**：三件套已在手（`mcp_verb_detail`），只改渲染形状。

| 字段 | 来源 | 状态 |
|---|---|---|
| `name` | `verb.mcp_name` | ✅ 现成（`renderloft_run_recipe` 这套，表里有） |
| `description` | 逐动词描述（`mcp_description`） | ✅ **已按判据 5 裁短**：16 条共 3 191 字符（平均 ~200），只留"调用它之前必须知道的事实"；跨动词的流程搬进了 `instructions`（§D） |
| `inputSchema` | `mcp_input_schema()` | ✅ 现成 |
| `annotations` | `mcp_annotations()` | ✅ 现成 —— 正是修掉 §3.1 的那一格 |

**判据**（全部有测试钉着，见 §6）

1. `tools/list` 的条数 **= 表里 `mcp_name` 非空的条数 + 1**（参考工具；**不许写死数字**）✅；
2. 每个动词的 `annotations` 与表里 `read_only` / `destructive` / `idempotent` **逐条相等** ✅；
3. `recipe.list` 的 `readOnlyHint` 与 `operation.cancel` 的 `destructiveHint` **能分辨** ✅；
4. 有参数的动词 schema 里参数齐全、`required` 与表一致；无参数动词渲染成 `properties: {}` 是对的 ✅；
5. **预算**：`tools/list` 序列化后 ≤ 常数，且与 recipe 数无关。
   ⚠️ **实测推翻了本条的估算**：压平后是 **12 534 字符**（描述 4 142 + schema 5 422 +
   名字与标注 463 + JSON 骨架），不是这里原先写的 ~7.8k。所以上限按**实测 + 5%** 定在 13 200，
   写在 `mcp::tests::TOOLS_LIST_BUDGET_CHARS` 里（失败信息会打印三块各占多少）。
   **再往下压只有一条不撒谎的路**：裁描述（已经到"只留必须知道的事实"了）或动 schema
   （= 动词的参数面，那是另一件事）。

> 为什么要预算判据：没有它，A 会把"每动词散文"变成 O(复杂度) 常驻 —— 老问题换个门进来。

### B. 撤掉 `renderloft_call` 作为调用路径 —— ✅ 已落地（2026-10-18）

- 它是 I4 的载体，与 A3 不相容；
- 它的长描述（1 234 字符 / ~300 token）**大部分是跨动词的规矩** —— 那属于 `instructions`；
- `renderloft_catalog` **保留**，降级为**按需查阅的参考工具**（只读、可缓存），**不许在别处被当入口点名**。

**判据**：`renderloft_call` 不再出现在 `tools/list`；没有任何描述再出现 "call `renderloft_catalog` first"。
✅ 两条都有测试钉着（`the_deprecated_call_tool_is_still_accepted_but_no_longer_listed`、
`every_tool_named_in_served_text_is_listed`）。

**兼容性**：A 是**纯增量**；破坏性只在 B —— 任何缓存过旧清单、或写死 `renderloft_call` 的调用方会断。
所以 B 最后做，并留一个**弃用窗口**。窗口的实现方式就是这条判据的形状：
**从清单里撤掉，但仍然认得**（`invoke()` 里那条兼容路径照旧；有测试钉住"撤了但还认"）。
另外压平之后**索引也不再嵌在参考工具的描述里**（那曾经是常驻账单里最大的一笔，
而且"索引常驻"本身就会把它变成入口）——它改成调用时返回。

### C. 成本与不可逆：改成**首行、固定、可见**的标记 —— ✅ 已落地

在"位置不够机械"之上再补一条更硬的：**拼写不许靠零宽字符区分**（§3.4）。

约定（取一个即可，重点是性质而不是符号）：

- 首行固定 ASCII 前缀：`[COST]`（会花用户额度，紧跟粒度）、`[IRREVERSIBLE]`（不可逆 / 破坏性）；
- 位置固定、大小写固定、**码点固定（不使用变体选择符）**；
- 粒度必须在地板上：`runs` 是数组 ⇒ **一次调用可能触发 N 次计费**（N ≤ 100），这句话要写进去，同时按 §3.3 落成 `maxItems`。

**判据**：两条正则各筛出一组，**条数与表里的"会花钱 / destructive"逐条对上**，且 `^\[COST\]` 与 `^\[IRREVERSIBLE\]` 互不冒充。
✅ 已落地并扩到**工具描述**：`every_listed_verb_reports_the_table_s_own_annotations` 断言每条工具描述
**以它自己的标记开头**、且 `[COST]` 集合与表逐条相等。

### D. 跨动词叙事归 `instructions`，逐动词细节归 `description` —— ✅ 已落地（2026-10-18）

`instructions` 已接好（`mcp.rs` 的 `initialize` / `server/discover` 两个分支）。
搬过去的是：典型流程（挑应用 → 读 schema → 提交 → 用廉价的只读 `operation.get` 跟进）、
花的是用户自己的额度（且**重投会再花一次**）、长任务（提交即返回；`wait` / `callbackUrl` 是加成）、
失败后先读 `inputs` ＋ `error` 再重试、**自主调用方并发 ≤ 4**、以及"`RUNNING` 不等于在动，看 `attention`"。

**分工一句话：`instructions` 说流程，`description` 说这一个动词。**

**判据**：`instructions` 里能读到完整流程、付费警告**与并发上限的数字**；每条 `description` 里不再出现"先调别的工具"这类跨动词指示。
✅ 有测试钉着（`instructions_carry_the_cross_verb_rules`：流程/付费/两个上限数字/`attention` 都在，
且没有一条工具描述在教"先调哪个工具"）；**16 条描述里的 `renderloft_` 出现次数为 0**。

### E. 异步路径：只用 request/response 就能表达完整

现状已对，**保持**：`operation.get` 是自足主路径（廉价只读、永不阻塞）；`wait` / `callbackUrl` 是加成而非必需；`wait` 描述里"客户端的请求超时可能比 `timeoutSeconds` 更短"保留（那是替协议说真话）；`callbackUrl` 只允许回环 http 的限制保留（安全约束）。

**判据**：只用 `operation.get` 也能跑完一次长任务。

### F. 修描述漂移（§3.4）

- **工具名 —— A 的世界里自动成立**：被点名的名字恰好都是 A 要列的名字（§3.4 实测）。**不需要改文案**；要做的是把 §6-3 的测试加上，钉住"被服务的文本里提到的名字必须都在 `tools/list` 里"。
- **标记拼写 —— 独立真问题，与 A 无关**：统一到一个码点序列（§C）。

**判据**：① §6-3 的测试绿（A 之后）；② 全文只有一个风险标记码点序列。

### G. 这一改动的性质，以及同批必须动的地方

**它不是表层调整，是一次架构决定。** `docs/AGENTS.md` 的"项目是什么"写着"自动化入口是 **MCP 元层（`catalog` ＋ `call`）与 CLI**"；A 与 B 动的就是这句已写进文档的立场。同批要改：

| 同批 | 内容 |
|---|---|
| `docs/AGENTS.md` | "自动化入口是 MCP 元层（catalog + call）"那一句；**以及第 83 行那个不存在的测试名**（§1.2） |
| `ARCHITECTURE.md` | **已核：没有工具面描述，不改。** 它的"自主调用方必须设闸"那条（"MCP 是自动批准的入口 ⇒ 按 actor 限并发"）与本次改动相容，而且正是 §7-2 的依据 |
| 守门测试 | `the_meta_tools_are_annotated_conservatively` 与 `the_index_and_the_table_agree_in_both_directions` 是钉住"两个工具"的两条，按 §6 换成性质测试；`tools_list_is_a_local_constant_without_per_app_tools` 保留 |

**实现点是收敛的**：按 `docs/AGENTS.md` 的约定（"加能力改动词表，不要改客户端"），CLI 与 MCP 的动词、参数、通道覆盖、文案都定义在 `v2/clients/verbs.rs` **一张表**里。

> **这条改动只改"表怎么渲染"，不改表本身。**

---

## 6. 守门：三条性质测试（替换两条形状测试）

形状测试钉的是数字，数字会过期（§1.2 就是例子）。改成钉**物理性质**。
**进度（2026-10-18）：三条全部落地**，两条旧的形状测试（`the_meta_tools_are_annotated_conservatively`
里的"恰好两个"、`the_index_and_the_table_agree_in_both_directions`）已按本节替换。

1. **预算**：`tools/list` 序列化后 ≤ `TOOLS_LIST_BUDGET_CHARS`（常数，实测 12 534 ⇒ 上限 13 200），
   且不随 recipe 数变化。`the_resident_tool_list_stays_within_its_budget`。
   *负对照（已验）*：给参考工具的描述加 3 000 个字符 ⇒ 红（失败信息会打印"描述 / schema / 其余"三块各占多少）。
2. **门**：清单里每个动词的 `annotations` 与表里 `read_only` / `destructive` / `idempotent` 逐条相等；`^\[COST\]` 筛出的集合 = 表里会花钱的集合。
   - 标记那一半：`cost_and_irreversibility_marks_are_mechanical_and_match_the_table`（`[COST]` / `[IRREVERSIBLE]` 集合与表逐条相同；被服务文本里不许再有 `⚠` / U+FE0F）。
   - annotations 那一半：`every_listed_verb_reports_the_table_s_own_annotations`（逐动词三条标注相等 + 描述以标记开头 + 只读与不可逆**能分辨**）。
   - *负对照（已验）*：把 `mcp_annotations()` 的 `read_only` 写死成 `false` ⇒ 红。
3. **不可见即不存在（A2 的可执行形式）**：**被服务的文本**里出现的工具名（正则 `renderloft_[a-z_]+`）必须都在 `tools/list` 里；同一份文本里出现的**动词 id** 必须在**表**里存在。"被服务的文本"含 `tools/list`、`instructions` 与 `catalog(<verb>)` 的 detail。
   - `every_tool_named_in_served_text_is_listed` 现在扫三处。压平后每条动词描述都**不再点名别的工具**，所以"工具名"那一半是零命中即绿（它拦的是"有人又把跨动词指示写回某条描述"）；真正在干活的是新增的**动词 id** 那一半 —— 流程搬进 `instructions` 之后，写错一个 id 没有任何编译期检查。
   - *负对照（已验）*：① 把 `instructions` 里的 `operation.get` 改成 `operation.gett` ⇒ 红（报错里列出表里全部 25 个 id）；② 在工具描述里写 `renderloft_typo` ⇒ 红。

---

## 7. 负空间：不许做的三件事

1. **不做分层。** 不加 `deferByDefault` 之类的自家字段去"建议"调用方怎么披露。可以预留，但描述里不许依赖（A5）。
2. **不做审批语义。** 不声明"这个需要确认"。闸门在调用方 —— `ARCHITECTURE.md` 的"自主调用方必须设闸"那条本来就是这个立场，保持。
3. **别给元层再加功能。** 这轮是把它从**承重**降成**可选**，不是给它扩面。
   ✅ 已按这条做：参考工具只保留"索引 + 单个动词的完整语义"，**索引不再嵌在常驻描述里**，
   也没有给它加任何新能力。

---

## 8. 验收（两条，都可证伪）

### ① 一问测试 —— 用一个只读 `tools/list`、不看任何额外说明的 agent

| 问题 | 考的是 |
|---|---|
| 这台能不能生成视频？ | 能力全集无隐藏成员（A2） |
| 要花谁的钱？ | 成本在地板上（A4） |
| 哪些动作会花钱？ | 成本 marker 可 grep（§C） |
| 哪些动作只读、哪些不可逆？ | **逐动作 `annotations`（§3.1 的修复）** |
| `operation.run` 的 `runs` 传 10 条会怎样？ | 计费粒度（§C）＋ 并发上限（§D） |

**答不出哪一条，就是哪一块藏了东西。**

> **① 的结果（2026-10-18，A＋B 之后，用仓库产物实测）**：五条都**能从 `tools/list` ＋
> `instructions` 直接读出来**，不需要任何额外说明：
>
> | 问题 | 答案来自 | 实测答案 |
> |---|---|---|
> | 能不能生成视频？ | 身份句 ＋ 工具名 | 能：`renderloft_run_recipe`，身份句写了 image/video |
> | 要花谁的钱？ | `instructions` 的 Money 段 | 用户自己的平台额度（`USER'S OWN platform credits`） |
> | 哪些动作会花钱？ | 描述首行的 `[COST]` | `renderloft_run_recipe`、`renderloft_retry_operation`（`[IRREVERSIBLE]`：`renderloft_cancel_operation`） |
> | 哪些只读、哪些不可逆？ | 逐动作 `annotations` | 11 个 `readOnlyHint: true`；`destructiveHint: true` 只有 `renderloft_cancel_operation` |
> | `runs` 传 10 条会怎样？ | schema `maxItems` ＋ `instructions` 的 Limits 段 | 上限 100；**每一条单独计费**；同时最多 4 个 operation 在飞 |
>
> ⚠️ 这是我（实现方）按"只读 tools/list"的口径自测的，**不是**一个独立 agent 的盲测 ——
> 那一条只有真挂一个 agent 才算数（见下）。

### ② 交叉测试 —— 挂到一个从没听说过 `renderloft_catalog` 的通用 agent

跑一次 recipe，数它浪费的轮数。

- 比自家 agent 多 ⇒ 工具面在漏；
- **静态参数**拼错导致一次真实计费 ⇒ §1.2 / A 没修。

> **注（按 I1 切）**：动态字段（recipe 的输入）永远需要 `describe` —— 判据只能按"静态参数"切，别把它当成"参数永不再盲写"的保证。
>
> **先跑基线再改。** 现在跑一次并记下轮数，否则改完没有对照，"漏没漏"只能靠感觉。

> ⚠️ **② 还没跑**（2026-10-18）：它要一个真挂上来的通用 agent ＋ **一次真实计费的任务**，
> 两者都只能由人来跑。压平之后"静态参数"这一侧的信息已经全在工具 schema 里
> （不再需要先 `catalog` 再猜），所以这一条的**预期**是轮数下降；但预期不是证据，
> 跑之前别把它写成"已验收"。

---

## 9. 顺序

| 步 | 内容 | 成本 | 收益 |
|---|---|---|---|
| 0 | ✅ **已定（2026-10-18）**：元层从"自动化入口"降为"按需参考"（§G）—— 维护者拍板 A＋B 同批 | 决策，不是工时 | 先定这个，后面才不是返工 |
| 1 | ✅ **已做**：漂移测试 `every_tool_named_in_served_text_is_listed`（扫 `tools/list` ＋ 表那一侧）＋ 标记码点统一（`mcp_markers()`） | 极低 | 把"人比对"变成"机器拦"；A 的世界里名字漂移自动消解 |
| 2 | ✅ **已做**：`runs` 的 `maxItems`（`MAX_RUNS_PER_OPERATION`）＋ `instructions()` 报三档上限（数字 import 强执行方，不手抄） | 极低 | 不新增权力，只公开边界；**不用等 A** |
| 3 | ✅ **已做（2026-10-18）**：**A** ＋ **G** 同批三处（守门测试按 §6 重写；`AGENTS.md` 的"自动化入口"那句与那个不存在的测试名都改了） | 低（三件套现成；description 裁到 16 条共 3 191 字符） | 修掉 §3.1 与 §3.2 两个真问题 |
| 4 | ✅ **已做（2026-10-18）**：**D** 叙事归 `instructions`（流程 / 付费 / 长任务 / 失败重试 / `attention`；三档上限本来就在） | 低（字段已接好） | 防重复：O(N) → O(1)（16 条描述里 `renderloft_` 出现 0 次） |
| 5 | ✅ **已做（2026-10-18）**：**B** 撤 `renderloft_call`（从清单里撤掉，**仍然认得** = 弃用窗口） | 中（要留弃用窗口） | 收口 |

> **A＋B 落地后的账单（实测，2026-10-18）**：常驻 1 135 → **3 035** token（0.89% → 2.37%），
> 按需索引 4 572 → **3 197**。这是"逐动作标注必须在常驻地板上"的代价，**不是**省钱的改动——
> 它买的是审批层能分辨"只读 / 会花钱 / 不可逆"（压平前这三类在清单里长得一模一样）。

> 第 0 步是**唯一真正的岔路**。若不打算动元层，那么 §3.1 与 §3.2 这两个代价仍然存在，但至少要**明确记在案**（写进 `status.md` 或本节）—— 它们是"调用方无法差异化治理"与"无意的规模变成账单"的成因，不该是一处无人知道的隐性取舍。

> ⚠️ 第 1、2 步改了**被服务的文本**（索引行多了 `[COST]` / `[IRREVERSIBLE]` 前缀，`instructions` 多了 Limits 段），
> 所以**本机跑着的 `renderloft-mcp` 二进制必须重建**才是新形状 —— 否则拿旧进程对照新判据，会得出"没生效"的错觉。
>
> 已提交：`793d024`（干净 HEAD 的 worktree 上复验：308 passed / 0 failed）。
> 实测本机跑的那个进程是**安装副本** `%LOCALAPPDATA%\RenderLoft\renderloft-mcp.exe`（不是仓库里的
> `src-tauri/target/release/renderloft-mcp.exe`）⇒ 要让新形状生效，得走打包/安装那条路，或改客户端指向仓库产物。
> 这是"二进制比表旧"第二次咬人（第一次是索引 13 vs 表 14）。
>
> 仓库产物已重建并**用真 stdio 烟测过**（发 `initialize` ＋ `tools/list` 两个 RPC）：`instructions` 里三档上限都在、
> 索引里 `[COST] operation.run` 与 `[IRREVERSIBLE] operation.cancel` 都在、`⚠` / U+FE0F 已绝迹，
> 而且索引是 **14 条**（安装副本仍是 13 条 —— 同一个坑的现场对照）。

**A 是主刀。** 它同时是"今天更可移植"和"将来迁移免费"—— 等协议真给了分层描述符，你要做的是**多声明一个 tier 提示**，而不是拆掉自己的架构。

> **协议该做分层；你不该手搓分层。**
