# Agent Memory Service

给多个 AI Agent 共用的本地记忆服务。Codex、Claude Code、WorkBuddy、DeepSeek Harness 连到同一个
`agent-memoryd` daemon 后，可以保存事实和明确的行为要求，召回项目里以前的决定，查看记忆的来源，
并更正已有记忆。各个客户端的任务状态和原生配置仍由它们自己管理。

## 项目简介

- **一个二进制**：`agent-memoryd` 是 Rust 写的 daemon，提供 JSON HTTP API；同一个二进制还提供
  stdio MCP 桥（8 个工具）、宿主 hooks 用来捕获对话和注入记忆的命令，以及把捕获的对话上传给 daemon 的上传器。
- **记忆算法**：目前只支持 **Mem0**。按顺序增量形成记忆，保存原始输入，在作用域内做精确去重，
  检索时综合语义、BM25 和实体打分，与 Mem0 Node SDK 3.3.1 内置 NLP 回退的行为对齐。
- **存储**：SurrealDB / SurrealKV 嵌入在进程内运行，不需要另外部署数据库或向量库。
- **模型**：嵌入和可选的重排默认用 API key 调用托管模型，也可以换成自己运行的 llama-server；
  从对话中提取记忆的生成模型是单独配置的 OpenAI 兼容服务。
- **边界**：daemon 只监听本机回环地址，服务一个本地用户。请求头里的 tenant、platform 只用于路由，不是认证。

```text
 Codex · Claude Code · WorkBuddy · DeepSeek Harness
   │ stdio MCP：agent-memoryd mcp        │ hooks：agent-memoryd hook capture / recall
   ▼                                     ▼
 agent-memoryd serve（127.0.0.1:8765，以 LaunchAgent 或 systemd 用户服务运行）
   ├── 本地存储：SurrealKV 数据库与全文索引（~/.local/share/agent-memory-service）
   └── 模型 API：生成（形成记忆）、嵌入与重排（检索）
```

## 一键安装

前置条件：

- macOS 或 Linux（Windows 请在 WSL 中运行）；
- Git、Rust stable（含 Cargo）和 C 编译工具链，用来从源码构建；
- Python 3.11+，只有安装器本身需要，daemon、MCP 桥和 hooks 都不依赖 Python；
- 一个模型 API key，默认是千问 MaaS 的 key（写入 `QWEN_API_KEY`），同时用于生成和托管的嵌入、重排模型。

克隆仓库后，在仓库根目录为要接入的宿主执行一条命令：

```bash
git clone https://github.com/RuisongZhou/agent-memory-service.git
cd agent-memory-service
bash scripts/install.sh --host codex --apply --project my-project --recall
```

四个宿主都能一键安装，但接入深度不同：DeepSeek Harness 目前只有 MCP 工具，没有自动捕获和自动召回。

| 宿主 | 接入方式 | 命令 | 安装后还需手动做 |
|---|---|---|---|
| Codex CLI / App | MCP + 自动捕获（3 个 hooks）+ 自动召回 | `bash scripts/install.sh --host codex --apply --project my-project --recall` | 重启 Codex；在 `/hooks`（App：Settings → Hooks）中信任 3 个捕获 hook 和 2 个召回 hook，信任之前不会捕获 |
| Claude Code | MCP + 自动捕获（4 个 hooks）+ 自动召回 | `bash scripts/install.sh --host claude-code --apply --project my-project --workspace /absolute/path/to/project --recall` | 在该项目中重启 Claude Code，按提示批准项目 MCP server（用 `/mcp` 查看） |
| WorkBuddy | MCP + 自动捕获（3 个 hooks）+ 自动召回 | `bash scripts/install.sh --host workbuddy --apply --project my-project --recall` | 重启 WorkBuddy，在「连接器 → 自定义连接器」中信任 `shared-memory`，确认 8 个工具都已启用 |
| DeepSeek Harness（桌面版与 `dsh`） | 仅 MCP 工具，由模型主动调用 | `bash scripts/install.sh --host deepseek-harness --apply` | 重启 DeepSeek Harness，新建会话 |

首次运行会以隐藏输入的方式询问模型 API key，写进 `~/.config/agent-memory-service/.env`（权限 600）。
之后给其他宿主安装时复用同一个二进制、密钥和 daemon，不会再问。想让几个宿主共享同一份项目记忆，
就给它们传同一个 `--project`。`--recall` 让 hook 在会话开始和每次提问时自动注入相关记忆；不需要时可以去掉。

Codex 和 Claude Code 一样用 hooks 捕获对话（`UserPromptSubmit`、`Stop`、`SessionEnd`），不占用 `notify`，
因此不会和 Codex Computer Use 等使用 `notify` 的程序冲突。之前的安装用 `notify` 捕获，重新运行安装命令时会去掉它，
并把原来的程序（例如 Computer Use 的客户端）还回去。Codex 按命令内容记录信任，改了 `--project` 等选项后需要重新信任。

### 这条命令做了什么

1. 有需要时先询问 API key（输入不回显），之后的步骤无需值守。密钥依次从 `--key-file`、环境变量
   `QWEN_API_KEY`（可用 `--api-key-env` 改名）和隐藏输入中读取，不会出现在命令行参数或输出里。
2. 构建 `agent-memoryd`（等同 `make build`），复制到 `~/.local/bin/agent-memoryd`，此后 `cargo clean`
   或移动仓库都不影响已安装的配置。已有可执行文件（例如 [release 包](docs/deployment.md#release-archive)）
   可用 `--binary` 直接使用，不再构建。
3. 用临时的 mock daemon 离线验证将要写入的命令：MCP 握手和 8 个工具、捕获 → outbox → 上传 → 形成记忆 → 检索、
   召回注入。本机装有 `codex`、`claude` 或 `dsh` CLI 时，还会让宿主自己在临时目录里解析一遍配置。
4. 备份并合并宿主配置，保留已有的其他 MCP server、hooks、设置、注释和顺序。在这一步之前失败时，
   配置文件、二进制和 env 文件都会恢复原样。
5. 用 `agent-memoryd service install --apply --start` 把 daemon 安装成 macOS LaunchAgent 或 Linux systemd
   用户服务并启动，等待 `/health` 响应；宿主需要捕获对话时，服务同时负责上传 outbox。已经安装过的服务
   保留原来的数据目录、env 文件和端口等选项；不是由它写入的服务文件、以及你手动启动的 daemon 都不会被替换。

加上 `--verify-model` 时，安装器还会用真实模型在临时库里形成并检索一条合成的测试事实，以确认生成和检索
都能用；这会产生少量模型调用，测试数据不会进入正式记忆。

### 常用选项

| 选项 | 作用 |
|---|---|
| 不带 `--apply`（或 `--dry-run`） | 只预览，不构建、不写文件、不访问网络 |
| `--project ID` | 捕获和召回使用的共享项目 ID；交互运行时不传会询问。DeepSeek Harness 不接受 |
| `--recall` / `--no-capture` | 加上自动召回 hooks / 不安装对话捕获 |
| `--no-service` | 只注册宿主，不写密钥、不安装 daemon 服务，适合自己运行 daemon 的情况 |
| `--binary PATH` | 直接使用已有的 `agent-memoryd`，不构建也不复制 |
| `--env-file`、`--key-file`、`--api-key-env NAME` | 指定 daemon 的 env 文件、从文件读取（并替换）密钥、从指定环境变量读取密钥 |
| `--verify-model` | 用真实模型做一次端到端检查（少量模型调用） |
| `--non-interactive` | 从不提示，缺少项目 ID 或密钥时直接失败 |
| `--url`、`--tenant`、`--outbox` | daemon 地址（默认 `http://127.0.0.1:8765`，端口也用于服务）、存储租户、outbox 路径 |
| `--rollback [BACKUP]` / `--uninstall` | 恢复该宿主最近一次（或指定）安装前的配置文件 / 只删除本安装器写入的条目 |

各宿主自己的选项（`--workspace`、`--scope user`、`--codex-home`、`--mcp-target`、`--dsh-home` 等）、
安装器的检查和拒绝写入的情形见 [一键安装说明](docs/mcp-harness.md#one-command-install)。

### 回退与卸载

`--rollback` 和 `--uninstall` 只处理当前宿主的配置。二进制、daemon 服务、env 文件、记忆数据和 outbox
可能还有其他宿主在用，都会保留。要彻底停掉并移除服务：

```bash
# macOS
launchctl bootout "gui/$(id -u)/local.agent-memory-daemon"
# Linux
systemctl --user disable --now agent-memory.service
# 然后删除服务文件（会先备份）
~/.local/bin/agent-memoryd service uninstall --apply
```

### 让 Agent 帮你安装

也可以把下面这段话发给已经能执行命令的 Agent（Codex、Claude Code 或 WorkBuddy）：

> 请为当前 Agent 安装 https://github.com/RuisongZhou/agent-memory-service 。先读 README.md 和
> docs/mcp-harness.md 的 One-command install，克隆仓库或复用本地副本，执行 `scripts/install.sh` 中对应宿主的
> 一键安装命令，项目 ID 用 my-project。保留已有的其他 MCP、hooks 和配置，不改代码。需要模型 API key 时，
> 告诉我在本地终端用 `--key-file` 或隐藏输入提供，不要把密钥放进聊天或命令参数。

Agent 的执行环境通常没有终端，无法使用隐藏输入，这时安装器会报错并说明可用的密钥来源。安装后仍需完成上表
最后一列的手动步骤。

### 验证范围

CI 对四个宿主运行完整的离线安装检查（`make install-check`，使用 `--no-service`）。宿主 CLI 解析检查在 macOS
上用 Codex CLI 0.163.0、Claude Code 2.1.305 和 DeepSeek Harness 0.2.1-rc.2 的 `dsh` 运行过；WorkBuddy 没有可用的
CLI，只由安装器检查 JSON 结构。服务启动这一步（`launchctl` / `systemctl`）的命令序列有单元测试，CI 不会真正
启动服务。目前还没有在真实客户端会话和真实模型下完成端到端验收，可以用 `--verify-model` 和
[共享检查](docs/mcp-harness.md#shared-memory-workflow) 在本机确认。

## 使用方式

### 在 Agent 里使用记忆

安装完成并重启宿主后，Agent 会看到 `shared-memory` 提供的 8 个 MCP 工具：

| 工具 | 用途 |
|---|---|
| `add_memories` | 保存对话、事实或明确的行为要求（`memoryType: "instruction"`） |
| `search_memories` | 按问题检索相关记忆 |
| `get_context` | 组装项目和会话上下文，以及已保存的行为要求 |
| `get_task` | 查询异步写入是否完成（拿到 requestId 不等于完成） |
| `list_turns` | 查看提交过的原始对话 |
| `get_memory` | 读取单条记忆及其历史和来源 |
| `correct_memory` | 追加更正，或撤回一条记忆 |
| `feedback` | 记录检索结果是否被采用 |

建议把 [共享记忆工作流](docs/mcp-harness.md#shared-memory-workflow) 中的说明合并到目标项目的 `AGENTS.md` 或
`CLAUDE.md`，并把其中的项目 ID 换成你的 `--project`。之后可以直接这样对 Agent 说：

- 「用 shared-memory 加载项目 my-project 的上下文，然后找出我们之前关于存储方案的决定，并给出来源。」
- 「作为 my-project 的行为要求记住：汇报项目决策时附上来源。」

装了自动捕获后，宿主每完成一轮对话，hook 都会把它写进本地 outbox，由 daemon 上传并形成记忆，Agent 不需要
自己调用工具保存普通对话。装了 `--recall` 后，会话开始时注入项目上下文，每次提问时注入本会话还没出现过的
相关记忆；daemon 没有运行时什么也不注入，也不会阻断提问。

**隐私**：使用默认的托管模型时，保存的文本和检索查询都会发给模型服务；开启 `--recall` 后，每次提问
（最多 2000 个字符）都会作为查询发给嵌入 API。需要让数据留在本机时，改用
[自托管的检索模型](docs/self-hosted-models.md)。

### 日常运维

```bash
~/.local/bin/agent-memoryd service status      # 服务文件、运行的命令，以及 daemon 是否在响应
curl -fsS http://127.0.0.1:8765/health          # 健康检查：记忆策略、模型、LLM 用量
~/.local/bin/agent-memoryd outbox status        # 捕获的对话是否已上传、形成记忆
```

- **日志**：macOS 在 `~/Library/Logs/agent-memory-service/daemon.stderr.log`；Linux 用
  `journalctl --user -u agent-memory.service`。
- **升级**：`git pull` 后，对已安装的宿主重新运行原来的安装命令。二进制有变化时会被替换，服务随之重启。
- **换密钥或模型**：编辑 `~/.config/agent-memory-service/.env`（可用的设置见 [配置参考](docs/api.md#configuration)），
  再重启服务；也可以带上 `--key-file` 重新运行安装命令。
- **备份**：停掉服务后，复制整个数据目录（默认 `~/.local/share/agent-memory-service/mem0`）和 outbox，
  详见 [存储、备份与升级](docs/deployment.md#storage-backups-and-upgrades)。
- **排查**：见 [部署排查](docs/deployment.md#common-problems) 和 [客户端排查](docs/mcp-harness.md#troubleshooting)。

### 手动部署与其他平台

不想用一键安装，或要自定义数据目录、端口、自托管模型时，按 [部署与运维](docs/deployment.md) 逐步操作。
其他支持本地 stdio MCP 的客户端可以参照 [examples/mcp.json](examples/mcp.json) 和
[其他平台](docs/mcp-harness.md#other-agent-platforms) 接入。

## 文档索引

| 要做的事 | 文档 |
|---|---|
| 安装、配置模型密钥、以服务方式运行、备份与排查 | [部署与运维](docs/deployment.md) |
| 自己运行嵌入和重排模型，代替托管 API | [自托管检索模型](docs/self-hosted-models.md) |
| 一键安装的细节、手动接入 Codex / Claude Code / WorkBuddy / DeepSeek Harness、对话捕获与自动召回 | [MCP 与宿主集成](docs/mcp-harness.md) |
| 请求字段、完成状态、作用域和配置默认值 | [API 与配置](docs/api.md) |
| 写入与读取路径、存储、实体投影、与 SDK 的差异 | [架构](docs/architecture.md) |
| 构建、校验和提交改动 | [贡献指南](CONTRIBUTING.md) |
| 与 Mem0 SDK 的受控对比和历史评测证据 | [评测](eval/README.md) |
| 运维与校验脚本清单 | [脚本清单](scripts/README.md) |

除本 README 外，文档目前都是英文。

## 开发与验证

```bash
make build                                        # 构建 target/fast/agent-memoryd
make setup                                        # 创建 .venv、安装测试依赖并下载公开数据集（不调用付费模型）
make fmt test-workspace test build selftest smoke install-check
make check-arch check-public
```

所有检查都使用临时存储、mock 检索模型和本地确定性的生成桩，不会调用付费模型。详见 [贡献指南](CONTRIBUTING.md#checks)。

## 仓库结构

| 位置 | 内容 |
|---|---|
| `crates/` | 服务实现，各层职责见架构文档 |
| `docs/` | 当前服务的文档，每个主题一篇 |
| `examples/` | 可编辑的服务配置、MCP 与捕获模板 |
| `scripts/` | 服务运维与校验脚本，包括一键安装器 `scripts/install.sh` |
| `eval/` | SDK 基线、数据集、评测协议、实验和历史报告 |
| `results/` | 被忽略的服务日志与检查结果 |
| `eval/results/` | 被忽略的评测证据、运行记录和存储 |

本仓库的文档描述当前的 Mem0 服务。之前的 Mem0 + Memos 实现保留在
[`codex/mem0-memos`](https://github.com/RuisongZhou/agent-memory-service/tree/codex/mem0-memos) 分支；
更早的设计版本留在 Git 历史中，`eval/` 中的评测报告保留其记录时的版本。复用旧数据目录前请先看
[兼容性说明](docs/api.md#compatibility)。

## 许可证

[MIT](LICENSE)，第三方来源见 [THIRD_PARTY.md](THIRD_PARTY.md)。复制的源码和提示词保留各自的版权声明。
仓库不包含密钥、模型权重、生成的数据库、原始 trace 或本地缓存。本项目不是 Codex、Claude Code、WorkBuddy
或 DeepSeek Harness 厂商的官方产品。
