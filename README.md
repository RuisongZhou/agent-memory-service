# Agent Memory Service

[![CI](https://github.com/RuisongZhou/agent-memory-service/actions/workflows/ci.yml/badge.svg)](https://github.com/RuisongZhou/agent-memory-service/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

让 Codex、Claude Code、WorkBuddy 和 DeepSeek Harness 共用一份记忆的本地服务。

## 它解决什么问题

每个 Agent 宿主都有自己的记忆：Claude Code 有 auto memory，Codex 有会话总结，WorkBuddy 有 `MEMORY.md`。
它们互相看不到，换一个宿主，之前定下的决定和提过的要求都要重新说一遍。

Agent Memory Service 在本机运行一个 daemon，几个宿主都连到它。装好之后大致是这样：

```text
周一，在 Codex 里
  你：存储就用 SurrealDB，不另外部署向量库。
      → 这一轮结束后被自动捕获，形成一条记忆

周二，在 Claude Code 里
  你：我们的存储方案是什么来着？
      → 提问前自动召回，把「存储用 SurrealDB，不另外部署向量库」注入上下文
  Claude：用的是 SurrealDB……
      → 需要时，Agent 可以用 get_memory 查到这条记忆来自周一那轮 Codex 对话
```

## 特性

- **跨宿主共享**：几个宿主用同一个 `--project`，就共用一份项目记忆。
- **自动捕获和召回**：hooks 在每轮对话结束后保存对话，在会话开始和每次提问时注入相关记忆；Agent 也可以用 8 个 MCP
  工具主动保存和检索。
- **有来源，能更正**：每条记忆都能查到形成它的原始输入，可以追加更正或撤回。
- **导入已有记忆**：宿主接入之前记下的内容和过去的会话可以导入，先审核再写入，能整次撤销。
- **一条命令安装**：安装器合并宿主配置，保留你原有的 MCP server、hooks 和设置，并把 daemon 装成用户服务。
- **数据在本机**：记忆存在本机的 SurrealDB 里，daemon 只监听回环地址。

## 项目状态

早期版本，适合个人在自己的机器上使用：

- 只服务本机的一个用户。请求头里的 platform 只用于路由，不是认证。
- 支持 macOS 和 Linux，Windows 请在 WSL 中运行。
- 记忆算法目前只有 Mem0，与 Mem0 Node SDK 3.3.1 内置 NLP 回退的行为对齐。
- DeepSeek Harness 只有 MCP 工具，没有自动捕获和自动召回。
- 安装器的离线检查在 CI 里运行，宿主 CLI 对配置的解析在 macOS 上验证过；还没有在真实客户端会话和真实模型下完成端到端
  验收，见 [验证范围](docs/mcp-harness.md#merges-backups-and-rollback)。安装时加 `--verify-model` 可以在本机用真实模型检查一次。

## 数据与隐私

- **发给模型服务的内容**：默认使用托管模型。保存的文本会发给生成模型和嵌入模型；开启自动召回后，每次提问（最多
  2000 个字符）都会作为查询发给嵌入 API。要让数据留在本机，改用 [自托管的检索模型](docs/self-hosted-models.md)，并把
  生成模型换成本地的 OpenAI 兼容服务（见 [配置](docs/api.md#configuration)）。
- **本机保存的位置**：记忆数据在 `~/.local/share/agent-memory-service/mem0`，捕获的对话先排在
  `~/.local/share/agent-memory-service/harness-outbox.sqlite3`，模型密钥在 `~/.config/agent-memory-service/.env`（权限 600）。
- **备份**：停掉服务后复制整个数据目录和 outbox，见 [存储、备份与升级](docs/deployment.md#storage-backups-and-upgrades)。

## 快速开始

前置条件：

- macOS 或 Linux；
- Git、Rust stable（含 Cargo）和 C 编译工具链，用来从源码构建；
- Python 3.11+，只有安装器需要；
- 一个模型 API key，默认是千问 MaaS 的 key，同时用于生成和托管的嵌入、重排模型。

**1. 安装 SurrealDB** 3.2 或更高的 3.x。daemon 用它保存记忆，安装器只检查它，不替你安装。macOS 用 Homebrew：

```bash
brew install surrealdb/tap/surreal
```

或者用官方安装脚本（macOS 和 Linux 都可以）：

```bash
curl -sSf https://install.surrealdb.com | sh
```

**2. 运行安装命令。** 克隆仓库，为要接入的宿主执行一条命令，以 Codex 为例：

```bash
git clone https://github.com/RuisongZhou/agent-memory-service.git
cd agent-memory-service
bash scripts/install.sh --host codex --apply --project my-project --recall
```

首次运行会以隐藏输入的方式询问 API key。之后给其他宿主安装时复用同一个 daemon 和密钥，不会再问；想让几个宿主共享记忆，
就给它们传同一个 `--project`。`--recall` 开启自动召回，不带 `--apply` 时只预览、不写文件。

**3. 在宿主里确认。** 每个宿主都要你在它自己的界面里确认一次，安装器无法代劳：

| 宿主 | 安装命令 | 安装后要手动做 |
|---|---|---|
| Codex CLI / App | `bash scripts/install.sh --host codex --apply --project my-project --recall` | 重启 Codex，在 `/hooks`（App：Settings → Hooks）里逐个信任 5 个 `shared-memory` hook；信任之前不捕获也不召回 |
| Claude Code | `bash scripts/install.sh --host claude-code --apply --project my-project --workspace /absolute/path/to/project --recall` | 在该项目里重启 Claude Code，按提示批准 `shared-memory` 项目 MCP server（用 `/mcp` 查看） |
| WorkBuddy | `bash scripts/install.sh --host workbuddy --apply --project my-project --recall` | 重启后在「连接器 → 自定义连接器」中信任 `shared-memory`，确认 8 个工具都已启用 |
| DeepSeek Harness（桌面版与 `dsh`） | `bash scripts/install.sh --host deepseek-harness --apply` | 重启后新建会话 |

Codex 按 hook 的内容记录信任：重跑安装时换了 `--project`、二进制路径等选项，或者升级后 hook 变了，要再信任一次。
安装器为什么不能代你信任，见 [Codex hooks](docs/mcp-harness.md#codex-hooks)。

**4. 检查。** 完成一轮对话后运行：

```bash
~/.local/bin/agent-memoryd outbox status
```

列表里应出现这一轮；状态为 `completed` 表示已经上传并形成记忆。宿主在接入之前记下的内容不会自动进来，需要的话见
[导入宿主已有的记忆](#导入宿主已有的记忆)。

安装器做的每一步、全部选项、拒绝写入的情形以及回退和卸载，见 [一键安装说明](docs/mcp-harness.md#one-command-install)。

### 其他安装方式

- **让 Agent 帮你安装**：把下面这段话发给已经能执行命令的 Agent（Codex、Claude Code 或 WorkBuddy）。Agent 的执行环境通常
  没有终端，无法隐藏输入密钥，这时安装器会报错并说明可用的密钥来源；第 3 步的确认仍要你本人完成。

  > 请为当前 Agent 安装 https://github.com/RuisongZhou/agent-memory-service 。先读 README.md 和
  > docs/mcp-harness.md 的 One-command install，克隆仓库或复用本地副本，执行 `scripts/install.sh` 中对应宿主的
  > 一键安装命令，项目 ID 用 my-project。保留已有的其他 MCP、hooks 和配置，不改代码。需要模型 API key 时，
  > 告诉我在本地终端用 `--key-file` 或隐藏输入提供，不要把密钥放进聊天或命令参数。

- **使用 release 二进制**：下载 [release 包](docs/deployment.md#release-archive)，安装时用 `--binary` 指定，不再构建。
- **手动部署**：要自定义数据目录、端口或自托管模型时，按 [部署与运维](docs/deployment.md) 逐步操作。
- **其他 MCP 客户端**：支持本地 stdio MCP 的客户端可以参照 [examples/mcp.json](examples/mcp.json) 和
  [其他平台](docs/mcp-harness.md#other-agent-platforms) 接入。

## 使用

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

### 自动捕获与召回

装了自动捕获后，宿主每完成一轮对话，hook 都会把它写进本地 outbox，由 daemon 上传并形成记忆，Agent 不需要自己调用工具
保存普通对话。装了 `--recall` 后，会话开始时注入项目上下文，每次提问时注入本会话还没出现过的相关记忆；daemon 没有运行时
什么也不注入，也不会阻断提问。细节见 [自动保存对话](docs/mcp-harness.md#automatically-save-conversations) 和
[自动召回](docs/mcp-harness.md#automatic-recall)。

### 导入宿主已有的记忆

宿主接入之前记下的内容（Claude Code 的 auto memory、Codex 的会话总结、WorkBuddy 的 `MEMORY.md`，以及 ChatGPT 等平台
让 Agent 导出的 JSONL），还有装上捕获之前的 Claude Code 和 Codex 会话，都可以导入共享记忆。先离线生成计划，再提议、
审核，最后写入，整次导入可以撤销：

```bash
~/.local/bin/agent-memoryd import plan --host codex --project my-project --workspace /absolute/path/to/project
~/.local/bin/agent-memoryd import propose <plan 打印的计划文件>   # 打印 RUN；不写记忆
~/.local/bin/agent-memoryd import show RUN                        # 生成评审文件，逐条改 accept / reject
~/.local/bin/agent-memoryd import apply RUN --review <评审文件>
~/.local/bin/agent-memoryd import rollback RUN                    # 需要时撤销整次导入
```

导入的事实只作为历史参考，自动召回能找到它们；需要人确认的行为要求不会自动写入。各宿主的参数、会话导入和前置条件见
[导入已有记忆](docs/mcp-harness.md#import-existing-memories)。

## 工作原理

```text
 Codex · Claude Code · WorkBuddy · DeepSeek Harness
   │ stdio MCP：agent-memoryd mcp        │ hooks：agent-memoryd hook capture / recall
   ▼                                     ▼
 agent-memoryd serve（127.0.0.1:8765，以 LaunchAgent 或 systemd 用户服务运行）
   ├── SurrealDB：daemon 启动你安装的 surreal，数据在 ~/.local/share/agent-memory-service
   └── 模型 API：生成（形成记忆）、嵌入与重排（检索）
```

- **一个二进制**：`agent-memoryd` 是 Rust 写的 daemon，提供 JSON HTTP API；同一个二进制还提供 stdio MCP 桥、宿主 hooks
  用来捕获对话和注入记忆的命令，以及把捕获的对话上传给 daemon 的上传器。
- **写入**：按顺序增量形成记忆，保存原始输入，在作用域内做精确去重。
- **检索**：综合语义、BM25 和实体打分。
- **存储**：daemon 在自己的数据目录上启动你安装的 `surreal`，只监听回环地址上的随机端口，用数据目录里随机生成的 root
  密码访问；不需要另外部署向量库。
- **模型**：嵌入和可选的重排默认调用托管模型，也可以换成自己运行的 llama-server；生成模型是单独配置的 OpenAI 兼容服务。

写入与读取路径、存储和实体投影见 [架构](docs/architecture.md)。

## 日常运维

```bash
~/.local/bin/agent-memoryd service status      # 服务文件、运行的命令，以及 daemon 是否在响应
curl -fsS http://127.0.0.1:8765/health          # 健康检查：记忆策略、模型、LLM 用量
~/.local/bin/agent-memoryd outbox status        # 捕获的对话是否已上传、形成记忆
```

- **日志**：macOS 在 `~/Library/Logs/agent-memory-service/daemon.stderr.log`；Linux 用
  `journalctl --user -u agent-memory.service`。
- **升级**：`git pull` 后，对已安装的宿主重新运行原来的安装命令；二进制有变化时会被替换，服务随之重启。SurrealDB 用安装时的
  方式升级，保持在 3.x。
- **换密钥或模型**：编辑 `~/.config/agent-memory-service/.env`（可用的设置见 [配置参考](docs/api.md#configuration)），再重启服务。
- **卸载**：安装命令加 `--uninstall` 去掉某个宿主的配置；停用并删除服务见 [安装服务](docs/deployment.md#install-the-service)。
- **排查**：见 [部署排查](docs/deployment.md#common-problems) 和 [客户端排查](docs/mcp-harness.md#troubleshooting)。

## 文档

| 要做的事 | 文档 |
|---|---|
| 安装、配置模型密钥、以服务方式运行、备份与排查 | [部署与运维](docs/deployment.md) |
| 自己运行嵌入和重排模型，代替托管 API | [自托管检索模型](docs/self-hosted-models.md) |
| 一键安装的细节、手动接入各宿主、对话捕获与自动召回 | [MCP 与宿主集成](docs/mcp-harness.md) |
| 请求字段、完成状态、作用域和配置默认值 | [API 与配置](docs/api.md) |
| 写入与读取路径、存储、实体投影、与 SDK 的差异 | [架构](docs/architecture.md) |
| 把宿主已有的记忆和过去的会话导入共享记忆 | [导入已有记忆](docs/mcp-harness.md#import-existing-memories)、[记忆导入设计](docs/design/memory-import.md) |
| 与 Mem0 SDK 的受控对比和历史评测证据 | [评测](eval/README.md) |
| 运维与校验脚本清单 | [脚本清单](scripts/README.md) |

除本 README 和 `docs/design/` 下的设计文档外，文档目前都是英文。

## 开发与贡献

```bash
make fmt test-workspace build test-scripts   # 只需要 pytest，不需要评测环境
make check-public
```

所有检查都使用临时存储、mock 检索模型和本地确定性的生成桩，不会调用付费模型。`test-workspace` 的存储测试和端到端安装
测试需要 PATH 上有 SurrealDB 3.2 或更高的 3.x。仓库结构、检查项和提交要求见 [贡献指南](CONTRIBUTING.md)。

## 许可证

[MIT](LICENSE)，第三方来源见 [THIRD_PARTY.md](THIRD_PARTY.md)。复制的源码和提示词保留各自的版权声明。
仓库不包含密钥、模型权重、生成的数据库、原始 trace 或本地缓存。本项目不是 Codex、Claude Code、WorkBuddy
或 DeepSeek Harness 厂商的官方产品。
