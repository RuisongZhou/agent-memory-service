# 按会话文件夹确定项目

> 状态：已实现。安装脚本只需要指定平台；hook 和 MCP 桥在运行时从会话所在的文件夹确定项目，没有文件夹的会话写入
> global。第 2 节的调研截至 2026-10-28，标"未核实"的条目没有公开文档或源码可查。

日期：2026-10-28。

## 1. 问题

之前项目 ID 在安装时确定：安装脚本要求 `--project`（Claude Code 还要 `--workspace`），把它写死在抓取和召回 hook 的
命令里。这带来三个问题：

- Codex 和 WorkBuddy 的 hook 只能装在用户级别，所有会话都会运行，于是所有会话的对话都记进同一个项目，不会随用户打开的
  文件夹切换。
- 用户要先想好项目 ID，换一个仓库就要重新安装。
- MCP 工具每次调用由 agent 自己填 `project`，agent 起的名字不一定和 hook 写死的一致，同一个仓库的记忆会分到两个项目里。

用户在智能体平台里自己决定在哪些项目启用 hook，所以安装时不需要知道项目。目前没有已安装的用户，可以不做兼容。

## 2. 平台能提供什么

| 平台 | hook 怎么知道会话文件夹 | MCP 进程怎么知道 | 来源 |
|---|---|---|---|
| Claude Code | 环境变量 `CLAUDE_PROJECT_DIR`：会话开始时的项目根，`cd`、进入 worktree 后都不变；桌面版 worktree 会话里指向主检出。payload 的 `cwd` 会跟着 `cd` 变 | 同样有 `CLAUDE_PROJECT_DIR`；进程工作目录是启动目录；支持 MCP roots | [hooks](https://code.claude.com/docs/en/hooks#common-input-fields)、[worktrees](https://code.claude.com/docs/en/worktrees)，本机探测 |
| Codex | 每个事件的 payload 都有 `cwd`（会话工作目录） | 配置里不写 `cwd` 时，进程在会话工作目录启动；不支持 roots | [hooks](https://learn.chatgpt.com/docs/hooks#common-input-fields)、[MCP](https://learn.chatgpt.com/docs/extend/mcp#stdio-servers)、openai/codex 源码 |
| WorkBuddy | UserPromptSubmit、Stop、SessionEnd 有 `cwd`，SessionStart 没有。不打开工作区时，任务在 `~/WorkBuddy/<时间戳>` 下新建目录 | 进程继承 worker 的工作目录，是否是会话文件夹未核实；不支持 roots | 本仓库的抓包（WorkBuddy 5.8.3），本机检查 5.9.2 安装包 |
| DeepSeek Harness | 只有 MCP，没有 hook | 继承 `dsh` 进程的工作目录，不分会话；不支持 roots | deepseek-ai/deepseek-harness 源码 |

MCP roots 在最新规范（2026-07-28）里已经废弃，规范建议改用工具参数或配置传目录，所以不用 roots。claude-mem 的做法与
这里一致：`CLAUDE_PROJECT_DIR` 优先，其次 payload `cwd`，再归到 git 根；Basic Memory、Supermemory 等只认显式配置。

## 3. 规则

**会话文件夹**：Claude Code 用 `CLAUDE_PROJECT_DIR`（非空时），其次 payload 的 `cwd`；其他平台用 payload 的 `cwd`。
MCP 桥在启动时取 `CLAUDE_PROJECT_DIR`，没有就取进程的工作目录。

**从文件夹到项目**：

1. 没有文件夹，或文件夹是用户主目录、根目录 `/`：**global**。
2. WorkBuddy 不打开工作区时自动建的目录（`~/WorkBuddy` 本身、其下的 `Claw` 和按时间戳命名的子目录及其内部）：
   **global**。用户改过 WorkBuddy 的默认工作区路径时识别不到，这种情况按普通文件夹处理。
3. 在 git 仓库里：项目是**主仓库的根目录**。worktree 归到主仓库，子目录归到仓库根。沿用导入已有的
   `repository_root`：worktree 的 `.git` 文件写着 `gitdir: <主仓库>/.git/worktrees/<名字>`，不用调用 git。仓库根就是
   主目录或在主目录之上时（比如管理 dotfiles 的仓库）不算项目，按第 4 条处理。
4. 其他情况：项目就是这个文件夹本身。

**项目 ID** 是上述文件夹的规范化绝对路径（解析符号链接，没有结尾的 `/`），例如 `/Users/me/code/agent-memory-service`。
用路径而不是文件夹名，是为了不让两个同名文件夹（比如都叫 `api`）混成一个项目；代价是移动文件夹后成为新项目。界面上
显示路径的最后一段，完整路径放在提示里。

**global 会话**：抓取的对话形成 global 范围的事实，在所有项目的召回和上下文里都可见；召回只取 global 的记忆。请求里用
`scope.kind: "global"` 明确表示，不能和 `scope.project` 同时出现；漏写 `project` 仍然是 400，避免客户端的错误把事实
悄悄写成 global。

**MCP 工具**：`project` 变成可选。不填时用桥启动时确定的项目（或 global）；填了就用填的，`*` 仍表示所有项目。桥在
服务说明里告诉 agent 当前的默认项目。导入只能进项目：默认是 global 时，导入要求显式给出 `project`。

**导入命令**：`import plan` 给了一个 `--workspace` 时，`--project` 默认取这个工作区按上面规则得到的项目 ID，和 hook
写入的一致；没有工作区（WorkBuddy、JSONL）时仍要显式给 `--project`。

## 4. 安装

- 安装脚本只接受平台和开关：`--host`、`--recall`、`--no-capture`、`--no-ui` 等；去掉 `--project`、`--workspace`，以及
  Claude Code 的 `--scope`（hook 和 MCP 一律装在用户级别）。
- hook 命令不再带 `--project`。用户想在某个项目里停用，用平台自己的开关：Claude Code 在项目设置里写
  `"disableAllHooks": true`；Codex 的用户级 hook 在不受信任的项目里也会运行，按项目停用需要在受信任项目的
  `.codex/config.toml` 里关掉 hooks 功能（未核实）。
- 安装时的自检用一个专门的检查文件夹作为会话文件夹，记忆写进这个文件夹对应的项目，不会写进 global 或用户的真实项目。
  安装脚本自己可能运行在某个 Claude Code 会话里，所以自检时要去掉继承来的 `CLAUDE_PROJECT_DIR`。

## 5. 不做的

- 不用 MCP roots（已废弃，Codex、WorkBuddy、DeepSeek Harness 都不支持）。
- 不按 git 远程地址确定项目：本机服务，路径已经唯一；远程地址会因为改名、fork 变化。
- 不提供把一个项目改名或合并到另一个项目的工具；移动文件夹的情况少，需要时另做。
