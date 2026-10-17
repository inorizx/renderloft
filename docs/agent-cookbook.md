# Agent Cookbook：从零跑通一个任务（尤其带图的）

给 **AI Agent / 自动化脚本** 的实操手册。前置认知：daemon 是唯一业务宿主，GUI / CLI / MCP
都只是 `/api/v1` 的客户端；你（Agent）通常走 **MCP**（`renderloft_catalog` + `renderloft_call`
两个元工具）或 **CLI**（`renderloft-cli`）。两条路底层完全同构，本文给两版写法。

> 产品边界（先读这条，能省掉一半的调试）：**槽位怎么定义、节点是什么意思，是上游应用/工作流
> 说了算，不是 RenderLoft 能管的。** 我们只做适配：把资产放进「食谱修订里声明的那个字段」。
> 不解析上游的「图1」「图4」这类文案，也不按任何顺序重排你的字段。

---

## 0. 发现：找到「字段键」

媒体任务的寻址单位是**字段键**：`<节点ID>::<字段名>`，例如 `475::image`。它就是你提交时用的
**键**。怎么拿到：

```text
# CLI
renderloft-cli recipe list                 # 有哪些应用（拿 recipeId）
renderloft-cli recipe describe --recipe 1  # 拿 currentRevisionId 与字段 schema
```

```text
# MCP：catalog 列出动词；call 调具体动词
renderloft_call { "verb": "recipe.list", "args": {} }
renderloft_call { "verb": "recipe.describe", "args": { "recipeId": 1 } }
```

`recipe describe` 返回的 `inputSchema.properties` 就是字段键的权威列表。媒体字段长这样：

```json
{ "properties": {
    "475::image": { "type": "string", "format": "file", "description": "…" },
    "520::image": { "type": "string", "format": "file", "description": "…" },
    "prompt":    { "type": "string" }
} }
```

- `format: "file"` ⇒ 这是媒体槽，值走**资产**（见下）；
- 其它字段照常放进 `inputs`；
- **键就是地址**：只填 `475::image` + `520::image` 是合法的（不必从第一个文件字段开始连续填）。

---

## 1. 文本任务（最简路径）

```bash
renderloft-cli operation run --recipe-revision 7 --input inputs.json
```

```text
renderloft_call { "verb": "operation.run",
  "args": { "recipeRevisionId": 7, "inputs": { "prompt": "a cat on a leash" } } }
```

长任务**别在工具调用里等**，给一个完成回调（见 §4），或短超时轮询 `operation.wait`。

---

## 2. 媒体任务端到端

三步：**拿字段键 → 上传资产 → 按字段键提交**。

### 2a. CLI（一条命令搞定上传+提交）

```bash
renderloft-cli operation run \
  --recipe-revision 7 \
  --input inputs.json \
  --asset '475::image=./person.png' \
  --asset '520::image=./cloth.png'
```

- `--asset` 可重复；形状 `<字段键>=<本机文件路径>`；
- CLI 会**先上传**每个文件（上传不计费），再把 `{assetId, checksum}` 组装成 keyed 对象提交；
- 缺 `=`、同一字段给两次、文件读不到、不是本机路径，都会在提交前报清楚。

### 2b. MCP（先 asset.upload，再按字段键放进 runs[].assets）

```text
# ① 上传（每次一个文件；返回 assetId 与 checksum）
renderloft_call { "verb": "asset.upload", "args": { "path": "/abs/person.png" } }
→ { "assetId": 12, "checksum": "sha1-…", "filename": "person.png" }

# ② 提交：资产放进 **assets**，键 = 字段键；不要放进 inputs、不要按位置排
renderloft_call { "verb": "operation.run", "args": {
  "recipeRevisionId": 7,
  "inputs": { "prompt": "换装" },
  "assets": {
    "475::image": { "assetId": 12, "checksum": "sha1-…" },
    "520::image": { "assetId": 13, "checksum": "sha1-…" }
  }
} }
```

> 只传你想传的槽位：`{475::image, 520::image}` 只会落这两个；其它槽位保持工作流默认。

---

## 3. 批量（一个 Operation 多个 Run）

批量用一个调用提交（每行**单独计费**；上限 100 行）。每行可以有**自己的** assets：

```text
renderloft_call { "verb": "operation.run", "args": {
  "recipeRevisionId": 7,
  "runs": [
    { "clientRunId": "look-1", "inputs": { "prompt": "blue" },
      "assets": { "475::image": { "assetId": 12, "checksum": "sha1-…" } } },
    { "clientRunId": "look-2", "inputs": { "prompt": "red" },
      "assets": { "520::image": { "assetId": 13, "checksum": "sha1-…" } } }
  ]
} }
```

CLI 一次一个任务；批量走 MCP 的 `runs`（或 GUI 的批量页）。`clientRunId` 是你的对齐锚，
不依赖返回顺序。

---

## 4. 长任务：提交完就走（callback）

```bash
renderloft-cli operation run --recipe-revision 7 --input inputs.json \
  --callback-url http://127.0.0.1:8787/hook
```

```text
renderloft_call { "verb": "operation.run", "args": {
  "recipeRevisionId": 7, "inputs": { … },
  "callbackUrl": "https://your-receiver.example/hook" } }
```

任务进终态时 daemon 会 POST 一份快照（`{operationId, status, runs:[…]}`），失败按退避重试。
地址必须 https，或回环地址的 http。**不要**在 `operation.wait` 里等几分钟——那会占住 MCP
客户端唯一的请求通道。

---

## 5. 提交前预检（dry-run，不花钱）

形状对不上时你先知道，而不是先计费一次再知道：

```bash
renderloft-cli operation run --recipe-revision 7 --input inputs.json \
  --asset '475::image=./person.png' --dry-run
# → {"dryRun": true, "valid": true, "runs": [{ "clientRunId": "…",
#      "assets": [{ "fieldKey": "475::image", "assetId": 12 }] }]}
```

```text
renderloft_call { "verb": "operation.run", "args": {
  "recipeRevisionId": 7, "inputs": { … },
  "assets": { "475::image": { "assetId": 12, "checksum": "sha1-…" } },
  "dryRun": true } }
```

预检与真实提交**共用同一套判据**（revision、输入 schema、字段键、资产存在与 checksum），
但**不创建任务、不调度、不调用上游、不花钱**。CLI 的 `--dry-run` 会先上传资产（上传本身不计费）。

---

## 6. 常见静默失败（都真的发生过）

| 症状 | 原因 | 正确做法 |
|---|---|---|
| 提交"成功"但图是空的/默认图 | assetId 塞进了 `inputs`（schema 媒体字段 format 是 `"file"`，不是 `"asset"`） | 放进 `assets`，键 = 字段键 |
| 图错位（第二格变成了上游的第 4 个槽） | 按**数组位置**猜字段（`assets:[a,b]` 里的第 2 个并不对应界面的第 2 格） | 用字段键；位置数组只在旧 revision 的兼容路径里被换算，新 revision 直接拒绝（`POSITIONAL_ASSETS_UNSUPPORTED`） |
| `ASSET_FIELD_UNKNOWN` | 字段键写错（不在该 revision 的媒体字段集合里） | `recipe describe` 后逐个核对键 |
| `ASSET_NOT_FOUND` / `ASSET_CHECKSUM_MISMATCH` | assetId 不存在、或 checksum 不是该资产的 | 用 `asset.upload` 原样返回的 `assetId`+`checksum` |
| 上游报错但看不出哪个字段 | 只看错误、不看提交的 `inputs` | `operation.get` 回执里带提交时的 inputs；一起读 |

---

## 7. 边界与事实

- **字段键 =（recipe revision, `<节点>::<字段>`）**，这是资产的真实身份；数组位置只是旧传输
  编码，内部一律先换算成字段键再用。
- RenderLoft **不解释**上游槽位的含义（「图1」是上游文案）；不改上游语义、不重排字段。
- 上传不计费；**提交计费**（RunningHub 每个 run 单独计费）。预检不花钱。
- 自主调用方（MCP）有并发上限（按 Operation 计），铺开大量小调用不如一个 `runs` 批量。
- 更细的协议细节（错误信封、签名、分页、截断）见 `docs/mcp-surface.md` 与 README。
