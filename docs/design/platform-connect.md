# 在网页里接入平台

> 状态：已实现。接口和字段以 [API 与配置](../api.md) 和代码为准。

日期：2026-10-28。范围：网页检测本机装了哪些 agent 平台，一键写入它们的 MCP 和 hook 配置，再一步步带用户在平台里
信任 hook、重启，最后确认真的收到了调用。

## 1. 现在的情况

- 接入平台只能在终端里跑 `bash scripts/install.sh connect --host H --apply`。安装器会合并已有配置、写前备份、能回滚，
  还会用临时 daemon 离线跑一遍 hook 命令，并让平台自己的 CLI 解析一遍配置（Codex、Claude Code、DeepSeek Harness）。
- 写完配置还不算接上：Codex 的 hook 要在 `/hooks` 里逐个信任，命令一变就要重新信任；WorkBuddy 要在连接器里信任
  `shared-memory`；所有平台都要重启。用户最常卡在这一步，而终端输出只在最后几行提了一句。
- 概览的"接入平台"卡片（`platform_activity`）能看到各平台最近有没有调用，但看不到哪些平台装了、哪些已经接入、
  配置是否过期，也不能从网页接入。

## 2. 目标与非目标

目标：

- 网页列出安装器支持的四个平台：本机有没有这个平台、是否已接入、接入了哪些功能、配置是否需要更新、最近一次调用。
- 从网页接入、更新、卸载和回滚，效果和终端里跑安装器完全一样。
- 写完配置后，按平台给出信任和重启的步骤，并实时确认 hook 和 MCP 的第一次调用，让用户知道哪一步没做完。

非目标：

- 替用户信任 hook 或重启平台。平台故意不让外部程序这样做，安装器也做不到。
- 在 daemon 里重写安装器。读写平台配置只由安装器做，网页和 daemon 都不解析平台的配置文件。
- 远程接入：daemon 只听本机回环地址，这个功能也只在本机用。

## 3. 架构

```
网页 ──POST /admin/platforms──▶ daemon ──运行──▶ installer/run status --json
     ──POST /admin/connect ───▶ daemon ──运行──▶ installer/run connect --host H --json [...]
     ──POST /admin/platform_activity（验证时每 3 秒一次）
```

### 3.1 安装器的副本

`install.sh service --apply` 把 `scripts/install` 复制到 `~/.local/share/agent-memory-service/installer/`，和网页文件
放在一起，并写一个启动脚本 `run`：用安装时的 Python（安装器要 3.11+，macOS 自带的 `/usr/bin/python3` 不够）和安装时的
`PATH` 运行这份副本。daemon 由 launchd 或 systemd 启动，`PATH` 只有系统目录，找不到 `codex`、`claude`、`dsh`，平台自己的
配置检查就会跳过；记下安装时的 `PATH` 才能和在终端里跑时一样。

用副本而不是仓库里的脚本：daemon 不知道仓库在哪；`git pull` 之后、`service --apply` 之前，仓库里的安装器可能比装好的
daemon 新。副本和 daemon 每次一起更新。

`service install` 把启动脚本记进服务单元：`serve --installer PATH`，和 `--ui-dir` 一样。daemon 启动时检查它是可执行文件。
没有这个参数（手动启动的 daemon），平台页给出终端命令。

### 3.2 安装器新增

- `install.sh status [--json]`：每个平台一项
  - `detected`：本机有这个平台的证据（配置目录或 CLI 的路径），没有就是 null；
  - `connected`：配置里有安装器管理的条目；
  - `options`：已接入时，当前装了对话抓取（`capture`）和自动召回（`recall`）没有；
  - `upToDate`：按当前选项重新接入会不会改动文件（比如 binary 路径变了，或者是 #49 之前带 `--project` 的 hook）；
  - `canRollback`：有一次可以回滚的写入；
  - `files`：会改的配置文件；`error`：读不了配置时的原因。
  另有 `service`：装好的 daemon 的地址，读不到时给出原因（这时 `upToDate` 为 null）。
- `connect --json`：结果作为一个 JSON 对象打印在 stdout 上，安装器原来的输出放进 `log`；子进程的输出改到 stderr，
  保证 stdout 只有这个对象。`{ok, host, mode, changes: [{path, action}], backup, log, error}`，`mode` 是 `preview`、
  `apply`、`uninstall` 或 `rollback`，`action` 是 `create`、`update` 或 `delete`。失败时 `ok` 为 false、退出码 1。

### 3.3 daemon 新增

- `POST /admin/platforms` → 运行 `status --json`，原样返回。
- `POST /admin/connect` `{host, action, capture?, recall?}` → 运行 `connect --host H --json` 加对应参数，原样返回。
  `host` 只接受小写字母和连字符（不能以 `-` 开头，参数不经过 shell）；平台名由安装器检查。`capture` 默认 true，
  `recall` 默认 false，只用于 `preview` 和 `apply`。最长运行 10 分钟（写入前的离线检查要启动临时 daemon）。
- 没有 `--installer` 时两个接口都返回 404 `NOT_FOUND`，网页据此提示重新跑 `service --apply`。
- 安装器能运行但失败（比如别的安装器正在运行、配置文件格式不对）时仍返回 200，由 `ok` 和 `error` 说明，网页显示
  原因和完整日志。

## 4. 流程

平台的状态：

| 状态 | 条件 | 主按钮 |
|---|---|---|
| 未检测到 | `detected` 为 null 且未接入 | 仍然接入（次要按钮） |
| 可以接入 | 检测到，未接入 | 接入 |
| 需要更新 | 已接入，`upToDate` 为 false | 更新配置 |
| 等待信任 | 已接入，近 30 天没有任何调用 | 完成信任步骤 |
| 已接入 | 已接入，配置最新，有调用 | 查看步骤 |

接入向导（弹窗，顶部是五步的进度条）：

1. **选择功能**：MCP 工具总会装；对话抓取默认开；自动召回默认关，打开时说明每次提问都会作为检索词发给 daemon，
   用托管嵌入模型时还会发到嵌入 API。只支持 MCP 的平台（DeepSeek Harness）不显示这两个开关。已接入的平台按当前选项预填。
2. **确认改动**：调用 `preview`，列出会新建、修改、删除的文件；说明写入前会离线校验、会先备份、可以一键回滚；
   可展开看等价的终端命令。
3. **写入**：调用 `apply`，显示进度（约 10–30 秒）。失败时停在这一步，显示原因和日志，配置不变。
4. **在平台里信任**：按平台列出要做的事，每一步可以勾选：
   - Codex：完全退出并重新打开（CLI 和桌面版共用 `~/.codex`）；输入 `/hooks`（桌面版：设置 → Hooks），信任
     "shared-memory capture"（和打开召回时的 "shared-memory recall"）；输入 `/mcp` 确认 `shared-memory` 已连接。
   - Claude Code：重启；用 `/hooks` 确认 shared-memory 的 hook 已加载；`/mcp` 确认已连接。
   - WorkBuddy：重启；打开 连接器 → 自定义连接器，信任 `shared-memory`，确认 7 个工具都已启用。
   - DeepSeek Harness：重启并新开会话；确认出现 `mcp__shared-memory__*` 工具。
5. **验证**：提示"在 Codex 里开一个新会话，说一句话"，每 3 秒读一次 `platform_activity`，逐项显示写入之后有没有
   收到：对话抓取、自动召回（打开时）、MCP 工具（可选，agent 用到记忆工具时才有）。收到就打勾；两分钟还没收到，
   提示最常见的原因（没信任、没重启、Codex 的 hook 改过后要重新信任）。这是网页唯一会自动刷新的地方，只在这一步。

卸载和回滚：先确认，再显示会改的文件，执行后提示重启平台。

## 5. 界面

- 新页面"接入平台"（导航第二组第一个），概览的接入平台卡片链接到这里。
- 每个平台一张卡片：平台的字母标识和名字、状态徽标；三项功能（MCP 工具、对话抓取、自动召回）用打勾和灰色表示；
  最近一次 hook 和 MCP 调用；"本机检测到：~/.codex"之类的证据；主按钮和"更多"（卸载、回滚上次修改）。
- 已接入但近 30 天没有调用的卡片显示提示，链接到信任步骤：这正是"装了但没生效"的情况。
- 加载、刷新、失败都在卡片和弹窗里就地显示；写入和卸载的按钮在执行时禁用，避免重复提交。

## 6. 安全

- 新能力：daemon 能改其他应用的配置，也就是决定它们会运行哪些 hook 命令。写入的命令只有安装器生成的那几条
  （`agent-memoryd hook ...`、`agent-memoryd mcp ...`），接口不接受路径、命令或任意参数。
- 访问控制沿用现有的本机检查：只接受回环地址的 `Host`，同源的 `Origin`，拒绝 `Sec-Fetch-Site: cross-site`。别的网站的
  页面调不到这两个接口。本机以你的身份运行的程序本来就能直接改这些文件，所以没有新增本机风险。
- 每次写入都先备份，可以回滚；离线校验失败时不写。
