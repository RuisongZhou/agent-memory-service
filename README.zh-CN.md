# Agent Memory Service

[![CI](https://github.com/RuisongZhou/agent-memory-service/actions/workflows/ci.yml/badge.svg)](https://github.com/RuisongZhou/agent-memory-service/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[English](README.md) | 简体中文

**给本地 AI Agent 用的共享记忆。** 让项目决策、事实和明确保存的行为要求在 Codex、Claude Code、WorkBuddy 和
DeepSeek Harness 之间延续。

把各个客户端连到同一个本地服务，并使用同一个项目 ID。每个客户端都能检索共享记忆、查看来源、更正过时的信息。

开启捕获和召回 hooks 后，典型的使用过程是这样的：

> 在 Codex 里，你讨论并定下了存储方案。这轮对话被捕获，并形成记忆。
>
> 之后在 Claude Code 里，你问起这个决定。召回 hook 检索该项目的记忆，把相关结果加入对话。Agent 回答前可以先查看
> 原始来源。

客户端已有的记忆和过去的对话可以另外导入。

## 能做什么

- **跨客户端继续工作**：自动捕获和召回，不用在对话之间手动复制上下文。
- **检查和更正记忆**：追溯一条记录的来源，追加更正或撤回。
- **导入已有历史**：导入前先审核提议的记忆，整次导入可以回滚。

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

### 3. 接入客户端

选一个稳定的项目 ID，替换下面的 `my-project`。要共享项目记忆的客户端，必须连到**同一个 daemon**，并使用**同一个
项目 ID**。

#### Codex CLI / App

```bash
bash scripts/install.sh --host codex --apply --project my-project --recall
```

重启 Codex，然后在 `/hooks`（App 中为 **Settings → Hooks**）里信任 `shared-memory` 的 hooks（带 `--recall` 时共 5 个）。
hooks 被信任之前，捕获和召回都不会运行。

Codex 按 hook 的内容记录信任。如果重跑安装时换了 `--project` 或二进制路径，或者升级后 hooks 有变化，需要重新信任。
见 [Codex hooks](docs/mcp-harness.md#codex-hooks)。

#### Claude Code

```bash
bash scripts/install.sh --host claude-code --apply --project my-project --workspace /absolute/path/to/project --recall
```

在该工作区重启 Claude Code，批准 `shared-memory` 项目 MCP server。用 `/mcp` 查看状态。

#### WorkBuddy

```bash
bash scripts/install.sh --host workbuddy --apply --project my-project --recall
```

重启 WorkBuddy。在 **连接器 → 自定义连接器** 中信任 `shared-memory`，并启用全部 6 个工具。

#### DeepSeek Harness

```bash
bash scripts/install.sh --host deepseek-harness --apply
```

重启 DeepSeek Harness 并新建会话。这个集成只提供 MCP 工具，不接受 `--project` 或 `--recall`。让 Agent 使用记忆时，
请说明项目。

#### 安装器做了什么

首次安装时，安装器以隐藏输入的方式询问 API key。它会构建二进制、检查集成、合并客户端配置，并把 daemon 作为用户服务
启动。之后给其他客户端安装时，复用同一个 daemon 和密钥。

安装器会保留已有的、无关的 MCP server、hooks 和设置。去掉 `--apply` 可以只预览。

> **项目路由**：捕获和召回 hooks 使用安装命令里记录的项目 ID。Codex 和 WorkBuddy 的 hooks 装在用户级，所有会话都使用
> 这个 ID，不会根据当前目录选择项目。Claude Code 默认只作用于指定的工作区。MCP 调用各自指定项目范围。

**其他配置：**

- **只装 MCP**：Codex、Claude Code 或 WorkBuddy 加 `--no-capture` 并去掉 `--recall`，此时不需要 `--project`。之后想加上
  hooks，用完整命令重新运行即可。
- **只捕获、不自动召回**：去掉 `--recall`。
- **使用已有二进制**：用 `--binary /absolute/path/to/agent-memoryd` 跳过构建，例如配合
  [release 包](docs/deployment.md#release-archive)。
- **自定义部署**：要指定数据目录、端口或自托管模型，按 [部署与运维](docs/deployment.md) 操作。
- **让 Agent 帮你安装**：能执行命令的 Agent 可以按 [安装说明](docs/mcp-harness.md#one-command-install) 操作。Agent 的
  shell 通常无法隐藏输入，密钥请你自己用 `--key-file` 或在本地终端提供，不要放进聊天或命令参数。上面各客户端里的确认
  步骤仍需你本人完成。

全部选项、配置变更、备份和卸载见 [安装说明](docs/mcp-harness.md#one-command-install)。

### 4. 试一下共享记忆

把两个支持的客户端连到同一个 daemon。在第一个客户端里说：

> 用 shared-memory 给项目 `my-project` 保存这条测试事实："The project's test mascot is a copper fox." 用
> `add_memories` 并设置 `infer:false`，直接存储原文。

在第二个客户端里说：

> 用 shared-memory 在项目 `my-project` 中检索它的测试吉祥物。给出存储的事实，并用 `get_memory` 查看来源。

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

安装并重启客户端后，Agent 会看到 6 个 `shared-memory` 工具：

| 工具 | 用途 |
|---|---|
| `add_memories` | 保存对话、事实或明确的行为要求（`memoryType: "instruction"`） |
| `search_memories` | 按问题检索相关记忆 |
| `get_context` | 组装项目和会话上下文，包括已保存的行为要求 |
| `list_turns` | 列出提交过的原始对话 |
| `get_memory` | 读取单条记忆及其历史和来源 |
| `correct_memory` | 追加更正，或撤回一条记忆 |

请求字段见 [API 参考](docs/api.md#interfaces)。

可以直接让 Agent 使用记忆：

| 任务 | 示例 |
|---|---|
| 继续项目工作 | 「加载 `my-project` 的 shared-memory 上下文，然后找出我们之前的存储方案决定。」 |
| 保存行为要求 | 「作为 `my-project` 的行为要求记住：汇报项目决策时附上来源。」 |
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

可以导入 Claude Code、Codex 和 WorkBuddy 支持的记忆文件，以及其他 Agent 导出的 JSONL；过去的 Claude Code 和 Codex
会话也可以导入。导入是一个单独的、需要审核的流程：

```bash
~/.local/bin/agent-memoryd import plan --host codex --project my-project --workspace /absolute/path/to/project
~/.local/bin/agent-memoryd import propose <plan 打印的计划文件>   # 打印 RUN；不写记忆
~/.local/bin/agent-memoryd import show RUN                        # 生成评审文件，逐条标 accept / reject
~/.local/bin/agent-memoryd import apply RUN --review <评审文件>
~/.local/bin/agent-memoryd import rollback RUN                    # 需要时撤销整次导入
```

导入的事实仍是历史参考材料；行为要求需要明确接受才会写入。支持的来源和参数见
[导入已有记忆](docs/mcp-harness.md#import-existing-memories)。

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
- **升级**：`git pull` 后，对每个已安装的客户端重新运行原来的安装命令；二进制有变化时会被替换，服务随之重启。SurrealDB
  保持在 3.x。
- **更换密钥或模型**：编辑 `~/.config/agent-memory-service/.env`（见 [配置](docs/api.md#configuration)），然后重启服务。
- **卸载**：在安装命令上加 `--uninstall` 去掉某个客户端的配置；停止并删除服务见
  [安装服务](docs/deployment.md#install-the-service)。
- **排查**：见 [部署问题](docs/deployment.md#common-problems) 和 [客户端排查](docs/mcp-harness.md#troubleshooting)。

默认的本地路径：

| 内容 | 位置 |
|---|---|
| 记忆数据 | `~/.local/share/agent-memory-service/mem0` |
| 捕获对话的 outbox | `~/.local/share/agent-memory-service/harness-outbox.sqlite3` |
| 模型配置和密钥 | `~/.config/agent-memory-service/.env` |

## 文档

除本文件和 `docs/design/` 下的设计文档外，文档目前都是英文。

| 主题 | 文档 |
|---|---|
| 部署、服务管理、备份与排查 | [部署与运维](docs/deployment.md) |
| 客户端安装、hooks 与导入 | [MCP 与客户端集成](docs/mcp-harness.md) |
| 本地嵌入和重排模型 | [自托管检索模型](docs/self-hosted-models.md) |
| API 字段、作用域、完成状态与配置 | [API 参考](docs/api.md) |
| 实现与算法边界 | [架构](docs/architecture.md) |
| 记忆导入设计 | [记忆导入](docs/design/memory-import.md) |
| 评测方法与历史结果 | [评测](eval/README.md) |
| 运维与检查脚本 | [脚本清单](scripts/README.md) |

## 项目状态与贡献

安装器的离线检查在 CI 中运行。客户端 CLI 对配置的解析已在 macOS 上验证。服务管理命令有单元测试覆盖，但 CI 不会启动
真实的用户服务。

目前的验证**还不包括在真实客户端和真实模型下完整的端到端验收**，详情见
[安装器验证了什么](docs/mcp-harness.md#merges-backups-and-rollback)。安装器可选的 `--verify-model` 检查会在临时存储上
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
