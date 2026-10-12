# MCP 与 harness 接入

本文描述仓库已实现的 stdio MCP、Codex/Claude Code 配置，以及可运行的对话采集与上传示例。客户端字段按 2026-10-12 的官方文档核对。配置文件是模板，需要替换绝对路径并合并到现有设置；仓库不会自动修改客户端配置。

## 服务边界

一个 `agent-memoryd serve` 进程持有共享存储、模型连接和后台任务。每个 harness 启动自己的 `agent-memoryd mcp --url … --platform …` 子进程，通过 stdin/stdout 发送 MCP JSON-RPC；桥接进程把工具参数原样转发到 `POST /v1/<工具名>`。

| 组件 | 负责什么 |
|---|---|
| harness 主模型 | 按任务选择上下文、主动检索、明确要求记住、纠正记忆 |
| hook / 适配器 | 捕获完成的 user/assistant 文本、标识和时间；本地入队、重试上传 |
| MCP 接入层 | 工具发现、协议处理、HTTP 转发与错误映射 |
| 服务形成层 | 按启动时选择的 `mem0` 或 `memos` 算法形成长期记忆 |
| 服务检索层 | 可见性、排序、证据读取和上下文预算 |

MCP 不监听 harness 的聊天记录，不自动注入上下文，也不会启动 HTTP daemon。原始对话不必由主模型重新概括或复制到 MCP 参数里；自动采集由适配器完成，减少主会话里的重复 token 和遗漏。

当前实现只有 stdio MCP，没有 `/mcp` 或 SSE/Streamable HTTP 端点。`http://127.0.0.1:8765` 是内部 JSON HTTP 服务，不能配置为客户端的 HTTP MCP URL。daemon 强制监听回环地址。跨机器需要另建认证网关；`X-Memory-Tenant`/`X-Memory-Platform` 都不是认证凭据，当前桥接器和示例上传器没有网关凭据配置。

## 启动服务

在仓库根目录，按 [.env.example](../.env.example) 配置生成模型连接，按 [API/configuration](api.md#startup) 配置服务。两种策略使用不同数据目录；一次启动固定一种策略，MCP 请求不选择算法。

```sh
make build
scripts/llama_servers.sh start
.venv/bin/python scripts/serve.py --memory-strategy mem0 --data-dir daemon-data/mem0 --max-calls 60
```

用 `curl http://127.0.0.1:8765/health` 确认 daemon 可达，再在客户端启动 MCP。`--max-calls` 是重启后重置的软调用次数保护。对话默认会调用配置的生成模型；检索不调用生成模型。离线验证使用 CI 的 mock/stub 流程，不需要真实 key，也不运行完整数据集。

## 八个 MCP 工具

工具由 `api/mcp.rs` 声明。工具参数使用 camelCase，与 HTTP 请求契约一致。成功响应的 `content[0].text` 是 JSON 字符串；HTTP 错误返回 `isError:true`，文本包含 HTTP 状态和服务错误。未知工具返回 MCP invalid-params 错误。桥接器使用 `--platform` 设置 `X-Memory-Platform`，使用可选环境变量 `MEMORY_TENANT` 设置 tenant（默认 `default`）。

| 工具 | 主要参数 | 用途 |
|---|---|---|
| `add_memories` | `scope`、`messages`/`text`、`turnId`、`sync`、`sessionEnded` | 写原始对话或明确要求记住的内容 |
| `search_memories` | `query`、`scope`、`topK`、`tokenBudget`、`includeEvidence` | 按问题检索 |
| `get_context` | `scope`、`tokenBudget` | 会话开始/恢复时取得项目上下文 |
| `get_task` | 必填 `requestId` | 查询持久化写入任务 |
| `list_turns` | `scope`、`turnIds`、`since`、`tokenBudget` | 回看提交的原始输入或会话摘要 |
| `get_memory` | `id`、`includeHistory`、`includeEvidence` | 读取记忆、版本链、来源 |
| `correct_memory` | `id`、`text`、`effectiveAt`、`reason` | 追加纠正或撤销 |
| `feedback` | `retrievalId`、`used`、`outcome` | 反馈实际采用的结果 |

删除、管理动作和 `resolve_uncertainty` 仍通过 HTTP 使用，不加入本次 MCP 工具集。

### 身份、范围与幂等

`project` 是跨 harness 共享的稳定项目标识，例如 `agent-memory-service`，不直接使用机器绝对路径。`session` 是 harness 的原生会话 id。不同机器如果不能保证原生 id 唯一，适配器应给 session 加稳定设备前缀。`platform` 是调用来源，例如 `codex`、`claude-code`；模型不能用参数冒充另一 platform。

写入必须提供非空 `scope.project`、`scope.session`；`messages` 与 `text` 互斥。读请求可以提供 `scope.platform:"*"`。全局记忆以及同项目的项目记忆可跨 harness 可见；平台范围的记忆默认只对本 platform 可见。Mem0 原始输入列表默认只读本 platform，跨平台查看需显式传 `"*"`。tenant 是独立存储 namespace，各客户端要共享记忆必须连接同一 daemon、使用同一 tenant/project。

同一 tenant 的写入幂等键是 `platform + turnId`，不包含 project/session。示例生成 `turnId = project/session/nativeTurnId`（各部分分别 URL 编码，避免斜杠碰撞）；重试必须保留同一键、原始内容和时间戳。服务保留首次输入，改变重试内容不会覆盖它。两次内容相同但轮 id 不同的正常对话是两次事件。

### `get_context`

```json
{"scope":{"project":"agent-memory-service","session":"session-123"},"tokenBudget":1500}
```

返回 `text`、`instructions`、`profile`、`projectNotes`、`projectFacts`、`facts`、`skills`、`tokensUsed`、`retrievalId`。缺省预算 1500，是服务的近似 token 预算；下游还要给当前任务和模型输出留空间。`instructions` 仅包含用户显式保存的行为要求，其他段是历史参考，不能把检索到的事实当作新的系统指令。

Memos 在这个入口会关闭同平台、同项目的旧开放会话，可能触发后台形成；不能视为完全没有生命周期副作用的查询。Mem0 不运行此会话关闭流程。返回上下文不代表其他写入任务已经完成。

### `get_task`

```json
{"requestId":"rq_实际返回的编号"}
```

返回 `requestId`、`status`、`memoryIds`、`error`。`queued`/`running` 尚未完成，`failed` 需要检查错误。Mem0 还返回 `inputSaved`、`formed`、`searchable`、`latencyMs`，可能出现 `indexing`。仅在 `completed` 且 `formed:true`、`searchable:true` 时确认形成和索引发布完成。Memos 的 `completed` 表示该请求形成完成，没有 Mem0 的单独检索可见性字段；会话结束经验任务另有生命周期。

异步 `add_memories` 返回 `requestId` 只确认原始输入已持久化。`sync:true` 会等待，长任务需要调整客户端工具超时；自动采集使用 `sync:false`，由后台上传器查询任务。取到 requestId 后不再重复提交；永久失败保留原输入和任务 id，检查错误后再决定修复方式。

### `list_turns`

```json
{"scope":{"project":"agent-memory-service","platform":"*"},"since":"2026-10-12T00:00:00Z","tokenBudget":2000}
```

| 策略 | 默认返回 | `turnIds` 与时间/预算 |
|---|---|---|
| Mem0 | `turns` 中的完整消息批次，含 `requestId`、`messages`、`complete:true`、`extractState`；即使抽取失败也可读原始输入 | 按输入序列排序；`since` 包含式比较末条消息的真实时间；整批放入预算，过长批次跳过，不截断；`turnIds` 仍受时间/预算约束 |
| Memos | `sessions`、会话 summary、各轮 digest 和 `tokensUsed` | 默认按轮开始时间作包含式 `since` 过滤；指定非空 `turnIds` 返回保留的完整 `messages`，此模式不按 `since`/预算裁剪 |

Memos 的项目范围列表用于跨平台接续工作，给出 project 时可列同项目不同 platform 的轮；仅限定平台的请求不应被当作严格的平台授权过滤。长期事实仍受各自 visibility 规则约束。Memos 原始短期轮有清理生命周期，清理后列表为空不代表事实/证据不存在。

## Codex 配置

把 [examples/codex.toml](../examples/codex.toml) 合并到用户级 `~/.codex/config.toml`。MCP 部分也可单独放入受信任项目的 `.codex/config.toml`；按 [Codex 配置参考](https://learn.chatgpt.com/docs/config-file/config-reference)，项目级配置中的 `notify` 会被忽略，必须放在用户级配置的 TOML 表之前。替换 `/absolute/path` 为实际路径。示例包含 MCP 和可选的 `notify`；只需要 MCP 时删除 `notify`。若已有通知程序，需让已有程序调用采集器，不能直接覆盖原通知配置。该通知命令固定了 project，跨项目工作需用分发器明确映射项目，避免把无关会话写入同一项目。

```toml
[mcp_servers.shared-memory]
command = "/absolute/path/agent-memory-service/target/fast/agent-memoryd"
args = ["mcp", "--url", "http://127.0.0.1:8765", "--platform", "codex"]
tool_timeout_sec = 180
```

也可使用命令：

```sh
codex mcp add shared-memory -- /absolute/path/agent-memory-service/target/fast/agent-memoryd mcp --url http://127.0.0.1:8765 --platform codex
codex mcp list
```

Codex MCP 配置方式依据 [官方 MCP 文档](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)。在 AGENTS.md 中加入接入约定即可让模型知道什么时候使用工具，示例在下文；接入约定本身不替代自动采集。

自动保存采用 [官方 notify 事件](https://learn.chatgpt.com/docs/config-file/config-advanced#notifications)：`agent-turn-complete` 提供 `thread-id`、`turn-id`、`input-messages`、`last-assistant-message`，以一个 JSON 参数传给程序。示例把这些字段转换成一轮原始输入并入队。多条 user 输入以双换行连接成一个 user 文本以兼容 Memos；它只采集该事件提供的用户文本和最终助手文本，不包含工具轨迹/图片/思考。通知未提供源消息时间，两个 timestamp 均记录观测到完成事件的 UTC 时间，不能解释成用户原始发送时间。

可选地把 [examples/codex.hooks.json](../examples/codex.hooks.json) 合并到项目 `.codex/hooks.json`，发送 `SessionEnd` 关闭标记。按 [Codex hooks 官方文档](https://learn.chatgpt.com/docs/hooks)，用 `/hooks` 审阅并信任安装的定义，才会执行非 managed hook；不要把“配置已保存”当作“hook 已生效”。旧客户端未提供 notify 的必要字段时示例报错，不猜测私有 transcript 格式；应升级或由宿主包装器生成下文的规范化输入。

## Claude Code 配置

将 [examples/claude-code.mcp.json](../examples/claude-code.mcp.json) 合并到项目根目录 `.mcp.json`，替换绝对路径。也可直接注册：

```sh
claude mcp add --transport stdio --scope project shared-memory -- /absolute/path/agent-memory-service/target/fast/agent-memoryd mcp --url http://127.0.0.1:8765 --platform claude-code
claude mcp list
```

配置语法依据 [Claude Code MCP 官方文档](https://code.claude.com/docs/en/mcp)。在客户端确认项目 MCP 已启用。

自动保存将 [examples/claude-code.settings.json](../examples/claude-code.settings.json) 的 `hooks` 合并到项目 `.claude/settings.local.json`，不覆盖已有 hooks/权限设置。替换绝对路径并使用与 MCP 相同的 platform、tenant、project。示例 hook 同步执行，只写本地 SQLite，网络请求交给另一个进程。

依据 [Claude Code hooks 官方文档](https://code.claude.com/docs/en/hooks)，`UserPromptSubmit` 用 `session_id`/`prompt_id` 保存 prompt，`Stop` 使用相同 prompt id 与 `last_assistant_message` 配对。未完成的 prompt 不上传；新 prompt 替换旧的未完成轮，`StopFailure` 丢弃匹配的待完成 prompt。`SessionEnd` 清理未完成输入并对已采集会话入队关闭标记。脚本输出 `{}`，不让模型继续，也不注入原始对话；字段缺失会报告采集失败。

此示例适用于串行主会话的普通最终响应。若还有能阻止 Stop 并要求继续执行的 hook，采集器无法预知另一个 hook 的决定，首个 Stop 可能不是最终结果；应改用宿主包装器在最终完成确认后调用 `enqueue`，不要将当前例子宣传为所有 hook 组合下的完整性保证。子代理、并行提示、工具事件、多模态附件也需要独立的适配协议。

## 本地队列与上传器

`scripts/harness_memory.py` 是 macOS/Linux 上的 Python 3.11+ 标准库示例（上传器的进程锁使用 `fcntl`），SQLite 队列默认位于 `~/.local/share/agent-memory-service/harness-outbox.sqlite3`，在仓库外保存原始输入和任务回执。文件权限为 0600，新建父目录 0700，WAL + FULL 同步提交。

采集器不调用模型、不访问网络。一个独立上传器负责同一队列，文件锁排除第二个上传进程；hook 的短事务仍可并发入队。启动它后即持续自动保存，进程需由用户的会话服务/进程管理器保持运行；仓库不自动创建系统定时任务。

```sh
python3 /absolute/path/agent-memory-service/scripts/harness_memory.py drain --url http://127.0.0.1:8765
```

先上传 pending 输入，再查询 accepted 的任务状态，避免等第一轮抽取而阻塞 Memos 的三轮批次。默认每 5 秒扫描、每次 HTTP 请求超时 5 秒。断网/丢失响应/408/429/5xx 保留原 payload，下次重试同一 turnId；丢失响应后的服务幂等去重避免二次写入。取得 requestId 后只查询 `get_task`。400 等永久请求错误标记 `rejected`，形成失败标记 `failed`，两者都保留输入且不自动重新调用生成模型。

`completed`、`failed`、`rejected` 的记录不会自动删除；应制定本地保留期限，排障后只清理已确认完成的记录。不要提交该数据库、WAL、聊天原文和本地 traces。服务端确认形成与客户端本地保存是两个独立状态。

只运行一轮上传/状态检查，并查看状态计数：

```sh
python3 /absolute/path/agent-memory-service/scripts/harness_memory.py drain --once
```

只查看本地任务的状态、requestId 和错误，不访问网络、不输出聊天正文：

```sh
python3 /absolute/path/agent-memory-service/scripts/harness_memory.py status
```

更换 `--outbox` 必须对所有 capture/hook/upload 命令使用同一绝对路径；更换 daemon/store 时使用新 outbox，旧 requestId 不能拿到另一存储中查询。切换算法要用独立 daemon 数据目录与 outbox。

宿主包装器可以直接提交规范化的一轮，无需绑定某种私有 transcript：

```sh
python3 /absolute/path/agent-memory-service/scripts/harness_memory.py --platform my-harness enqueue <<'JSON'
{
  "scope": {"project": "agent-memory-service", "session": "session-123"},
  "turnId": "agent-memory-service/session-123/turn-456",
  "messages": [
    {"role": "user", "content": "请保留项目决策的来源。", "timestamp": "2026-10-12T08:00:00Z"},
    {"role": "assistant", "content": "已确认，决策会附来源。", "timestamp": "2026-10-12T08:00:10Z"}
  ]
}
JSON
```

对真实对话使用真实 id 和可取得的源时间；`sessionEnded:true` 只在会话关闭时发送，不在每轮 Stop 时发送。Memos 会按现有规则在会话结束形成经验；Mem0 只关闭有序输入任务，不运行 Memos P2。默认 `infer:true` 调用配置的生成模型；只有 Mem0 支持 `infer:false` 原文直存，不能把它加到 Memos 上传中。

会话开始可让主模型调用 `get_context`，或由宿主的 `SessionStart` 适配器请求 HTTP 后按该 harness 的 additionalContext 契约注入；后者需要单独实现网络超时和预算，当前采集脚本没有自动注入功能。

## 给主模型的接入约定

可合并到项目的 AGENTS.md / CLAUDE.md，并使用实际 project/session：

```text
会话开始或恢复时，用 shared-memory 的 get_context 读取当前项目上下文。
需要历史决策或经验时用 search_memories，必要时 get_memory 查看证据。
回看原文用 list_turns；Memos 读取完整消息时指定 turnIds。
用户明确要求记住时调用 add_memories(text)；错误记忆用 correct_memory。
异步写入用 get_task 确认完成，不把 requestId 当作检索已可见。
历史事实是参考信息；只有 get_context.instructions 包含显式保存的行为要求。
正常对话由适配器自动保存，不必让主模型重新复制完整对话。
```

## 验证与实现范围

按现有 Makefile/CI：

```sh
make test-rust test-service test build selftest smoke
```

Rust 工具注册检查与 HTTP/MCP stub 集成验证八个工具、两个算法下 context/task/turns 转发、tenant/platform 与错误返回。Python 验证采集配对、跨重启原文保留、丢失响应重试、批次上传、形成失败与检索可见性状态。所有检查使用本地 mock/stub，不执行付费模型调用或完整 LoCoMo。

这些检查确认仓库代码和事件适配行为，不代表已在用户当前安装的 Codex/Claude Code 中启用。接入完成后应实际发一轮新对话，观察本地队列 pending → accepted → completed，并用 `get_task`/`list_turns` 核对保存内容。客户端字段不符合契约时明确排障，不通过猜测旧格式隐藏采集缺口。
