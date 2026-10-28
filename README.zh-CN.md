# Agent Memory Service

[![CI](https://github.com/RuisongZhou/agent-memory-service/actions/workflows/ci.yml/badge.svg)](https://github.com/RuisongZhou/agent-memory-service/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[English](README.md) | 简体中文

**给本地 AI Agent 用的共享记忆。** 让项目决策、事实和明确保存的行为要求在 Codex、Claude Code、WorkBuddy 和
DeepSeek Harness 之间延续。

把各个客户端连到同一个本地服务，并在各个客户端里打开同一个文件夹。每个客户端都能检索共享记忆、查看来源、更正过时的信息。

开启捕获和召回 hooks 后，典型的使用过程是这样的：

> 在 Codex 里，你讨论并定下了存储方案。这轮对话被捕获，并形成记忆。
>
> 之后在 Claude Code 里，你问起这个决定。召回 hook 检索该项目的记忆，把相关结果加入对话。Agent 回答前可以先查看
> 原始来源。

客户端已有的记忆、过去的对话，以及 ChatGPT 等其他助手记得的内容，都可以另外导入。

## 能做什么

- **跨客户端继续工作**：自动捕获和召回，不用在对话之间手动复制上下文。
- **检查和更正记忆**：追溯一条记录的来源，追加更正或撤回；在 Agent 里或本机的[网页界面](#网页界面)里都可以。
- **导入已有历史**：用 CLI 导入客户端的记忆文件，或者通过 MCP 直接在对话里导入另一个助手的记忆；写入前先审核提议
  的记忆，整次导入可以回滚。

### 支持的客户端

| 客户端 | MCP 工具 | 自动捕获 | 自动召回 |
|---|:---:|:---:|:---:|
| Codex CLI / App | ✓ | ✓ | ✓ |
| Claude Code | ✓ | ✓ | ✓ |
| WorkBuddy | ✓ | ✓ | ✓ |
| DeepSeek Harness / `dsh` | ✓ | — | — |

MCP 提供 Agent 可以调用的工具；hooks 在特定的对话事件上触发捕获和召回。没有 hooks 时，是否调用工具取决于 Agent 自己的
判断、你的要求和项目说明。

其他支持本地 stdio MCP 的客户端，可以参照 [examples/mcp.json](examples/mcp.json) 和
[其他平台](docs/mcp-harness.md#other-agent-platforms) 接入。

## 安装之前

这是一个早期项目，面向**一台机器上的一个用户**，运行在 macOS 或 Linux 上。Windows 请使用 WSL。

记忆保存在本机，但**默认的模型配置会把相关文本发给托管模型服务**：

- 形成记忆时，提交的文本会发给生成模型。
- 检索使用嵌入模型，以及可选的重排模型。
- 自动召回把每次提问作为检索查询，最多 2000 个字符。

要让服务的模型处理都在本地完成，需要同时配置本地的 OpenAI 兼容生成服务（见 [配置](docs/api.md#configuration)）和
[自托管检索模型](docs/self-hosted-models.md)。你的 Agent 客户端自身使用的模型另行配置，与此无关。

daemon 只监听回环地址（`127.0.0.1:8765`）。项目和平台标签用于组织请求，不是认证或访问控制边界。

## 快速开始

### 1. 安装前置依赖

从源码安装需要：

- Git
- Rust stable（含 Cargo）和 C 编译工具链
- Python 3.11+，供安装器使用
- SurrealDB 3.2 或更高的 3.x 版本
- 可选：Node.js 20.19+ 或 22.12+ 及 npm，用来构建[网页界面](#网页界面)；没有 npm 时安装器下载已发布版本的网页，而没有已发布
  版本的代码（见[更新日志](CHANGELOG.md)）没有可下载的网页，服务会在没有网页的情况下运行
- 一个模型 API key；默认配置使用千问 MaaS，用于生成、嵌入和重排

macOS 上用 Homebrew 安装 SurrealDB：

```bash
brew install surrealdb/tap/surreal
```

或者用官方脚本（macOS 和 Linux 均可）：

```bash
curl -sSf https://install.surrealdb.com | sh
```

确认 `surreal` 在 `PATH` 上，且版本受支持：

```bash
surreal db version   # 安装脚本装的 `surreal`；Homebrew 装的用 surreal version
```

daemon 会自己启动和管理这个可执行文件作为存储，不需要另外启动数据库服务。其他安装方式见
[安装 SurrealDB](docs/deployment.md#install-surrealdb)。

### 2. 获取仓库

```bash
git clone https://github.com/RuisongZhou/agent-memory-service.git
cd agent-memory-service
```

### 3. 安装服务，再接入客户端

分两步，按顺序执行。**服务**是本机唯一的 daemon；**客户端**是连到它的一个 Agent 平台。服务只装一次，然后给你用到的每个平台
各接入一次。两条命令不加 `--apply` 都只预览。

```bash
bash scripts/install.sh service --apply
```

它会检查 SurrealDB，以隐藏输入的方式询问模型 API key，构建二进制，安装网页，并把 daemon 作为用户服务启动（macOS 上是
LaunchAgent，Linux 上是 systemd 用户单元）。下面的 `connect` 不构建、不询问密钥：它从已安装的服务读取二进制、URL 和 outbox，
再合并平台的配置，并保留已有的、无关的 MCP server、hooks 和设置。

接入时不用指定项目。hooks 和 MCP 桥在运行时从会话所在的文件夹确定项目：在 git 仓库里是仓库的根目录，否则是这个文件夹本身。
没有文件夹的会话，或在用户主目录里打开的会话，使用 **global** 记忆，所有项目也都能看到它。要共享项目记忆的客户端，必须连到
**同一个 daemon**，并打开**同一个文件夹**。详见[项目路由](docs/mcp-harness.md#project-routing)；某个项目不想用 hooks 时，见
[按项目关闭 hooks](docs/mcp-harness.md#turning-the-hooks-off-per-project)。

也可以在网页里接入：**接入平台**页（`http://127.0.0.1:8765/ui/#/platforms`；[网页界面](#网页界面)只有中文）列出本机检测到的平台，
预览并写入配置，一步步带你在平台里信任 hook、重启，最后确认收到了第一次调用。它运行的是同一个安装器。

#### Codex CLI / App

```bash
bash scripts/install.sh connect --host codex --apply --recall
```

重启 Codex，然后在 `/hooks`（App 中为 **Settings → Hooks**）里信任 `shared-memory` 的 hooks（带 `--recall` 时共 5 个）。
hooks 被信任之前，捕获和召回都不会运行。

Codex 按 hook 的内容记录信任。如果重跑 `connect` 时换了二进制路径或选项，或者升级后 hooks 有变化，需要重新信任。
见 [Codex hooks](docs/mcp-harness.md#codex-hooks)。

#### Claude Code

```bash
bash scripts/install.sh connect --host claude-code --apply --recall
```

重启 Claude Code，用 `/mcp` 查看 `shared-memory` 的状态。它装在用户级，它和它的 hooks 对所有项目生效。

#### WorkBuddy

```bash
bash scripts/install.sh connect --host workbuddy --apply --recall
```

重启 WorkBuddy。在 **连接器 → 自定义连接器** 中信任 `shared-memory`，并启用全部 8 个工具。

#### DeepSeek Harness

```bash
bash scripts/install.sh connect --host deepseek-harness --apply
```

重启 DeepSeek Harness 并新建会话。这个集成只提供 MCP 工具，不接受 `--recall`。MCP 桥运行在 `dsh` 的工作目录，不是
会话所在的文件夹，所以让 Agent 使用记忆时，请指明项目（某个文件夹的绝对路径）。

其他安装方式（只装 MCP、只捕获不召回、不装网页、使用已有的二进制或自己运行的 daemon、让 Agent 帮你安装）、全部选项、配置变更、
备份和卸载，见[安装器参考](docs/mcp-harness.md#installer)。

### 4. 试一下共享记忆

把两个支持的客户端连到同一个 daemon，并在两个客户端里打开同一个文件夹（DeepSeek Harness：在请求里指明这个文件夹作为
项目）。在第一个客户端里说：

> 用 shared-memory 保存这条测试事实："The project's test mascot is a copper fox." 用 `add_memories` 并设置
> `infer:false`，直接存储原文。

在第二个客户端里说：

> 用 shared-memory 检索项目的测试吉祥物。给出存储的事实，并用 `get_memory` 查看来源。

第二个客户端应该能检索到第一个客户端保存的事实。这一步检查显式存储、检索和跨客户端可见性，可能会为嵌入或重排调用
模型 API。

要单独检查**自动捕获**，在已信任捕获 hooks 的客户端里完成一轮对话，然后运行：

```bash
~/.local/bin/agent-memoryd outbox status
```

`completed` 表示上传和记忆处理已经完成，但不保证产生了新记忆：抽取可能得不到事实，重复内容也可能复用已有记录。

要检查**自动召回**，在开启召回的新会话里询问一个已存储的项目事实，不要求 Agent 调用 MCP。客户端支持时，查看 hook
输出或注入的上下文。

## 日常使用

### MCP 工具

安装并重启客户端后，Agent 会看到 8 个 `shared-memory` 工具：

| 工具 | 用途 |
|---|---|
| `add_memories` | 保存对话、事实或明确的行为要求（`memoryType: "instruction"`） |
| `search_memories` | 按问题检索相关记忆 |
| `get_context` | 组装项目和会话上下文，包括已保存的行为要求 |
| `list_turns` | 列出提交过的原始对话 |
| `get_memory` | 读取单条记忆及其历史和来源 |
| `correct_memory` | 追加更正，或撤回一条记忆 |
| `propose_import` | 把另一个助手的记忆导出提交成导入提议，不写记忆 |
| `apply_import` | 你在对话里同意后，写入提议的导入 |

请求字段见 [API 参考](docs/api.md#interfaces)。`propose_import` 和 `apply_import` 用来导入另一个助手记得的内容，见
[导入已有记忆](#导入已有记忆)。

可以直接让 Agent 使用记忆：

| 任务 | 示例 |
|---|---|
| 继续项目工作 | 「加载这个项目的 shared-memory 上下文，然后找出我们之前的存储方案决定。」 |
| 保存行为要求 | 「作为这个项目的行为要求记住：汇报项目决策时附上来源。」 |
| 查看来源 | 「依赖这条记忆之前，先查看它的来源。」 |
| 更正记录 | 「用 shared-memory 更正这条记录：服务端口现在是 9000。」 |
| 撤回记录 | 「撤回这条记忆，它已经不成立了。」 |

历史记忆是参考材料，可能已经过时。明确保存的行为要求会单独返回，并且仍以当前的指令为准。

想让 Agent 稳定地使用工具，把 [共享记忆工作流](docs/mcp-harness.md#shared-memory-workflow) 加到项目的 `AGENTS.md` 或
`CLAUDE.md`。其中说明了何时加载上下文、查看来源、保存行为要求，以及等待异步写入完成。

### 自动捕获与召回

开启捕获 hooks 后，普通的已完成对话会被自动保存。开启召回后，服务在会话开始时加载上下文，并在每次提问前检索相关
记忆。检索受时间和上下文预算限制；召回失败或 daemon 未运行都不会阻断提问。见
[自动保存对话](docs/mcp-harness.md#automatically-save-conversations) 和 [自动召回](docs/mcp-harness.md#automatic-recall)。

### 导入已有记忆

有两种导入方式。两种都先生成提议，只写入你接受的内容，整次导入可以回滚。导入的记忆都是历史参考材料，不会变成指令；
需要每个宿主都遵守的规则请自己保存。

**用 CLI 导入记忆文件。** 支持 Claude Code、Codex 和 WorkBuddy 的记忆文件、其他 Agent 导出的 JSONL，以及过去的
Claude Code 和 Codex 会话：

```bash
~/.local/bin/agent-memoryd import plan --host codex --workspace /absolute/path/to/project   # 项目就是这个工作区的
~/.local/bin/agent-memoryd import propose <plan 打印的计划文件>   # 打印 RUN；不写记忆
~/.local/bin/agent-memoryd import show RUN                        # 生成评审文件，逐条标 accept / reject
~/.local/bin/agent-memoryd import apply RUN --review <评审文件>
~/.local/bin/agent-memoryd import rollback RUN                    # 需要时撤销整次导入
```

支持的来源和参数见 [导入已有记忆](docs/mcp-harness.md#import-existing-memories)。

**通过 MCP 在对话里导入另一个助手的记忆。** 适合 ChatGPT、Gemini、Claude.ai 保存的记忆，不用存文件：

1. 在已连接的宿主里运行 `/shared-memory:import_memories /absolute/path/to/project chatgpt`（Claude Code；Codex 这类不支持
   MCP 提示词的宿主，把 `agent-memoryd import prompt --for host --project /absolute/path/to/project --source chatgpt`
   的输出贴进对话）。
2. 把 Agent 给你的导出提示词拿到原来的助手里运行，再把它的回答贴回来。
3. Agent 删掉不该导入的行，用 `propose_import` 提交其余的行。这时还没有写入，Agent 会把提议给你看。
4. 回复"写入"表示同意，Agent 调用 `apply_import` 写入这次导入；要撤销，运行 `agent-memoryd import rollback RUN`。

详细步骤、限制和常见问题见
[在对话里导入另一个助手的记忆](docs/mcp-harness.md#import-from-another-assistant-in-a-conversation)。

### 网页界面

服务还在 `http://127.0.0.1:8765/ui/` 提供一个本机网页，可以浏览、检索、新增、更正、撤回和删除记忆，审核导入、consolidation 和
冲突，以及接入 Agent 平台。网页没有自己的登录，和 daemon 的其余部分一样依赖只监听回环地址这一边界（见
[安装之前](#安装之前)）。如果 `/ui/` 返回 404，说明服务没有网页：安装 Node.js 和 npm（见前置依赖），再运行一次
`bash scripts/install.sh service --apply`。

**界面文字目前只有中文，没有语言切换。** 导航按顺序是：

| 页面 | 用来做什么 |
|---|---|
| 概览 | 看各类记录数、LLM 用量、待处理任务和模型状态，近 30 天有哪些平台调用过，以及哪些事项需要处理 |
| 记忆列表 | 按项目、平台、类型、状态和来源筛选浏览。单条记录的页面显示来源、更正链、谱系和原始对话，并可更正、撤回或删除 |
| 实体 | 浏览检索关联到记忆的实体，以及每个实体对应的记忆 |
| 新增 | 原样保存一条事实，可选让生成模型抽取事实（会调用模型），或保存一条行为要求 |
| 检索调试 | 输入查询，查看排序、分数和耗时；查询会发给嵌入模型，重排需要你主动勾选，也会调用模型 |
| 上下文预览 | 按部分查看 `get_context` 会为某个项目和会话注入的文本，不调用模型 |
| 接入平台 | 找出本机上的平台，预览并写入配置，信任 hooks，确认收到第一次调用，也可卸载或回滚 |
| 导入审核 | 审核导入运行：接受或拒绝提议，应用或回滚一次导入 |
| consolidation | 提出对重复或重叠事实的合并（每个簇一次生成调用），审核后整批应用或回滚；daemon 开启 `consolidationEnabled` 后才可用 |
| 冲突 | 并排比较有冲突的记录，选择保留哪条并填理由 |
| 召回记录 | 查看哪个平台、哪个会话、什么时候取回了哪些记忆，以及哪些被用上 |
| 任务 | 跟踪记忆形成的请求、状态和失败原因，以及模型的原始响应 |

## 工作原理

`agent-memoryd` 是一个 Rust 二进制，提供 HTTP 服务、stdio MCP 桥、捕获和召回 hooks，以及 outbox 上传器。

- **捕获**：hooks 把已完成的对话写入本地 outbox，上传器再提交给 daemon。
- **形成**：服务保存原始输入，并用配置的生成模型抽取事实。完全相同的内容可以复用已有记忆。
- **检索**：综合语义相似度、BM25 和实体打分，可选重排。
- **存储**：daemon 管理一个使用 SurrealKV 的本地 SurrealDB 进程，不需要另外的向量数据库。

记忆算法遵循 Node/TypeScript 开源版 `mem0ai@3.3.1` 内置 NLP 回退路径。服务有自己的存储、作用域规则、任务处理和来源
追踪；这并不表示完全兼容 Mem0 SDK 或 Mem0 Platform。

实现细节和差异见 [架构](docs/architecture.md)。

## 运维

```bash
~/.local/bin/agent-memoryd service status    # 服务文件、运行的命令，以及 daemon 是否在响应
curl -fsS http://127.0.0.1:8765/health        # 记忆策略、模型和 LLM 用量
~/.local/bin/agent-memoryd outbox status      # 捕获的对话是否已上传并处理
```

- **日志**：macOS 在 `~/Library/Logs/agent-memory-service/daemon.stderr.log`；Linux 用
  `journalctl --user -u agent-memory.service`。
- **升级**：`git pull` 后运行一次 `bash scripts/install.sh service --apply`，不论接了几个客户端：它会重新构建、替换二进制和
  网页，并重启服务。只有[更新日志](CHANGELOG.md)提到某个平台的 hook 或 MCP 桥条目有变化时，才需要对它重跑 `connect`；
  `connect` 是幂等的，随时重跑都安全。SurrealDB 保持在 3.x。
- **更换密钥或模型**：编辑 `~/.config/agent-memory-service/.env`（见 [配置](docs/api.md#configuration)），然后重启服务。
- **卸载**：对每个已接入的平台运行 `bash scripts/install.sh connect --host HOST --uninstall`，然后运行
  `bash scripts/install.sh service --uninstall`：它会停止服务并删除服务单元，保留记忆数据、env 文件、二进制和网页
  （见 [安装服务](docs/deployment.md#install-the-service)）。
- **排查**：daemon 起不来或没有响应，或模型密钥、模型服务、SurrealDB、存储的问题，见
  [部署问题](docs/deployment.md#common-problems)；客户端没接上、hooks、outbox、召回和导入的问题，见
  [客户端排查](docs/mcp-harness.md#troubleshooting)。

默认的本地路径：

| 内容 | 位置 |
|---|---|
| 记忆数据 | `~/.local/share/agent-memory-service/mem0` |
| 捕获对话的 outbox | `~/.local/share/agent-memory-service/harness-outbox.sqlite3` |
| 模型配置和密钥 | `~/.config/agent-memory-service/.env` |

## 文档

除本文件和 `docs/design/` 下的设计文档外，文档目前都是英文。[文档索引](docs/README.md)按任务列出每份文档，并解释文中用到的
术语（daemon、客户端、桥、hook、outbox、项目）。

| 主题 | 文档 |
|---|---|
| 安装器、接入客户端、hooks、项目路由与导入 | [MCP 与客户端集成](docs/mcp-harness.md) |
| 部署、服务管理、备份与排查 | [部署与运维](docs/deployment.md) |
| 本地嵌入和重排模型 | [自托管检索模型](docs/self-hosted-models.md) |
| API 字段、作用域、完成状态与配置 | [API 参考](docs/api.md) |
| 实现与算法边界 | [架构](docs/architecture.md) |
| 版本之间的变化与升级说明 | [更新日志](CHANGELOG.md) |
| 评测方法与历史结果 | [评测](eval/README.md) |
| 运维与检查脚本 | [脚本清单](scripts/README.md) |

## 项目状态与贡献

安装器的离线检查在 CI 中运行。客户端 CLI 对配置的解析已在 macOS 上验证。服务管理命令有单元测试覆盖，但 CI 不会启动
真实的用户服务。

目前的验证**还不包括在真实客户端和真实模型下完整的端到端验收**，详情见
[安装器验证了什么](docs/mcp-harness.md#merges-backups-and-rollback)。可选的 `service --verify-model` 检查会在临时存储上
使用真实模型，可能产生 API 费用。

开发时运行：

```bash
make fmt test-workspace build test-scripts
make check-public
```

这些检查使用临时存储和 mock 模型，不调用付费模型。存储测试和端到端安装测试需要 `PATH` 上有受支持的 SurrealDB。

环境准备和贡献要求见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 许可证

[MIT](LICENSE)。第三方代码和提示词保留各自的声明，见 [THIRD_PARTY.md](THIRD_PARTY.md)。

本项目是独立项目，不是所支持客户端厂商的官方产品。
