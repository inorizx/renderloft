# 完成回调与验签

任务跑完之后，daemon 怎么把结果主动告诉你（webhook），以及**接收端怎么确认"这确实来自我的 RenderLoft"**。

> 只想"跑完把图收下来"、不想写接收端？用 CLI 定时收图即可，见
> [`agent-cookbook.md`](agent-cookbook.md) 的「定时收产物」。

---

## 1. 为什么需要它

MCP 的工具调用是**请求/响应**：`operation.wait` 等太久会把客户端那条通道占住
（实测过一次，后面的调用全部排队超时）。要"提交完就走、跑完通知我"，就在提交时给一个回调地址。

## 2. 怎么开

提交任务时给地址——CLI 用 `--callback-url`，MCP 用 `callbackUrl`：

```powershell
renderloft-cli operation run --recipe-revision 1 --input cat.json --callback-url http://127.0.0.1:8787/hook
```

```text
renderloft_call { "verb": "operation.run", "args": {
  "recipeRevisionId": 1, "inputs": { … },
  "callbackUrl": "https://your-receiver.example/hook" } }
```

**地址规则**：必须是 **https**，或**回环地址的 http**（`127.0.0.1` / `localhost` / `[::1]`）——
本地接收端不用造证书；内网与公网地址只收 https。地址不合规在提交时就被拒（`CALLBACK_URL_INVALID`）。

## 3. 什么时候发、发什么

任务进终态时，daemon 会 POST 一份快照过去：

```json
{ "operationId": 73, "status": "SUCCEEDED",
  "runs": [ { "runId": 77, "status": "SUCCEEDED", "clientRunId": "run-b6f50adf…" } ] }
```

触发方式是**后台按状态补扫**，不是在各终态出口各挂一处——所以 reconcile / scheduler / cancel /
hygiene 哪条路把任务推到终态都覆盖：

- **daemon 当时没在跑、重启后也会补上**（24 小时窗口内——一个几周前的旧任务突然发通知会很吓人）；
- **幂等**：判据是"还没有投递记录"，所以重启、重复补扫都不会收到第二遍。

## 4. 失败重试

投递失败按**退避重试**，**投递记录留在库里**（daemon 的 SQLite 工作区）——"发没发出去、试了几次"
是可查的事实，不靠翻日志猜。

## 5. 验签（接收端怎么确认"这确实来自我的 RenderLoft"）

自己生成一串随机密钥，两边填同一个值——**它只进不出**：RenderLoft 只写不读（读回来只有"配没配"）。

```powershell
# 你自己生成（例：32 字节随机），填进接收端，再交给 RenderLoft
renderloft-cli callback set-secret --from-file .\callback-key.txt   # 值只从文件读，不进命令行
renderloft-cli callback status                                      # {"configured": true}
renderloft-cli callback clear-secret                                # 关掉签名（想换密钥时先清再设）
```

生成一个 32 字节随机密钥（PowerShell 5.1 与 7 都能跑），写出来的文件不带 BOM：

```powershell
$b = New-Object byte[] 32
[System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($b)
[System.IO.File]::WriteAllText("$PWD\callback-key.txt", (-join ($b | ForEach-Object { $_.ToString('x2') })))
```

几条关于密钥本身：

- 存在**系统凭据库**里，只进不出；
- **只有 CLI 能写**（`callback` 组在动词表里 `mcp_name: None`）：MCP 的参数是模型生成的，
  密钥进去就等于进了模型上下文与对话记录；
- 值**只从文件读**，不进命令行（也就不进 shell 历史）；文件内容的首尾空白会被去掉；
- 默认**拒绝覆盖**已有密钥，要覆盖加 `--force`——旧值不可恢复，而覆盖会让原本能验签的接收端**验不过**；
- `callback status` **只回 `true`/`false`，永远不回值**。

配上之后，每次投递都带这个头：

```text
X-RenderLoft-Signature: t=1791609031058,v1=<hex>
```

接收端按这个契约验（**顺序别弄反**）：

1. 取 `t`（毫秒时间戳）与 `v1`（十六进制签名）；
2. 用同一个密钥，对 **`"{t}.{请求体的原始字节}"`** 算 HMAC-SHA256；
3. 与 `v1` 比较（常数时间比较），并拒绝太旧的 `t`（防重放，窗口多大你自己定）。

> 为什么签的是"原始字节"：签名那头用的是同一份序列化结果，所以两边逐字节一致。
> 这也意味着**别在中间重新序列化 body**（换键序、加空格都会验不过）。

没配密钥就**不带**这个头——宁可不签，也不签一个假的。所以"要不要验签"由你的接收端决定：
**看到头就必须验，验不过就拒收**。

> 没有签名这一层时，接收端无法验证"这确实来自我的 RenderLoft"——所以**别把接收端暴露到公网**，
> 除非先配好密钥并在接收端验签。

## 6. 收不到回调怎么办

- 先看**投递记录**（在 daemon 的库里），确认是"没投"还是"投了没成"；
- 地址写错、接收端没起、内网/公网用了 http，都会被挡在提交或投递那一步；
- 不想写接收端、也不想挂着 daemon 等通知 → 用 CLI 定时收产物：
  [`agent-cookbook.md`](agent-cookbook.md) 的「定时收产物」。

## 相关

- [`agent-cookbook.md`](agent-cookbook.md) —— Agent / 自动化脚本的实操手册（批量、dry-run、产物落盘）
- [`status.md`](status.md) —— 回调能力的实现状态
