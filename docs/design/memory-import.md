# 宿主已有记忆的导入

> 状态：笔记导入已实现，覆盖 Claude Code、Codex、WorkBuddy 和宿主 Agent 导出的 JSONL；Claude Code 和 Codex 的会话导入
> （6.10、6.14、6.16）也已实现。用法见 [MCP 与宿主集成](../mcp-harness.md#import-existing-memories)。尚未实现：规则文件的
> 指令候选（`--include rules`）、`--prune`、安装器的 `--import`（6.18）和 DeepSeek Harness 的会话导入（第 9 节第 4 阶段）。
> 实现和下文不一致的地方见第 0 节。当前行为以 [API 与配置](../api.md#import-runs)、[架构](../architecture.md#import) 和代码为准。

日期：2026-10-22。范围：Codex、Claude Code、WorkBuddy 和 DeepSeek Harness 在接入本服务之前已经积累的记忆文件和
会话记录。第 4 节的格式来自一台 macOS 开发机：Claude Code 2.1.293–2.1.306 的 transcript 和 auto memory，
Codex CLI/App 0.153–0.163 的 rollout 和 memories 目录，以及 WorkBuddy、DeepSeek Harness 的数据目录。只核对了目录、
文件名、字段结构和文本长度，没有读取记忆正文。

## 0. 实现与本文的差异

笔记导入按本文实现，下面这些地方做了调整，以实现为准：

- `plan` 不连接 daemon，所以计划里不判断单元是否 `unchanged`。这一步在 propose 时做：daemon 拿单元哈希和台账比较。
- 模型不输出 `transient`。提示词让它直接略去任务进度和临时状态，抽取单元改为记录 `coverage`，即被采用的依据片段覆盖了
  多少原文。`kind` 是 `fact`、`preference`、`instruction`、`reference` 之一，`scope` 必填。
- 超过 8 KB 的抽取单元在计划里按段落和句子切成几部分，单元键加 `~part1`、`~part2` 这样的后缀。
- 台账行是单元的"活链接"：它当前持有的记录（写入的，或链接到的同文本记录），从一次导入带到下一次，直到被这个单元
  的新提议取代或不再是 current。apply 之后，这次提议过的每个单元都有一条台账行；提议全被拒绝时状态记为 `declined`，
  保留原来的链接，同样的文本不会再被提出来，`propose --reconsider` 可以重新提出。
- 和已有记录内容相同的提议默认接受，接受即链接到那条记录，不再写一条新的；这样即使另一个单元以后改了自己的文本，
  这个单元的内容也还在。
- 取代按角色进行：一条记录的单元（verbatim）的新事实取代它以前的那条事实，新指令取代以前人确认过的那条指令。另一个
  单元还持有的记录不会被取代，也不会被回滚。没被取代的旧记录在评审文件里以 `previous` 行列出。
- 指令默认只对项目生效，模型给的 `scope: global` 只作提示，人在评审文件里把 `scope` 改成 `global` 才会全局生效；所有
  指令候选都按 1024 字符的上限检查。
- 抽取缓存的键包括项目、单元文本、标题路径、`statedAt`、召回预算、提示词变体、提示词和模型；所有条目都没通过校验的
  回复不缓存，下次重新抽取。抽取的输出 token 上限是原文估算长度的两倍，至少 2000、最多 8192，不跟随形成的
  `mem0MaxTokens`。
- apply 前，CLI 重新读取来源文件，propose 之后改过的文件的提议一律按拒绝处理。
- 已知限制：重复标题用 `~2`、`~3`，超长节用 `~partN`，都按在文件里的位置编号。在前面插入同名标题、或一节长过 8 KB 时，
  单元键会变。因为重复内容会链接、别的单元持有的记录不会被取代，这不会丢内容，但可能留下一条过时的 current 记录，
  要靠 consolidation 或人撤回。
- 失败的单元不会让整个 run 失败：run 照常进入 `proposed`，失败的单元要在新的 run 里重新提出。propose 中断后不能续传，
  重新运行就是新的 run。
- 端点多了 `import_run` 和 `import_runs`。`apply` 必须带 `--review` 或 `--yes`。propose 按单元数（默认 8）和请求体大小
  （1 MiB）分批，出错时显示 daemon 给出的原因。
- 抽取没有实现"有待处理的形成任务时让出"，和形成按到达顺序共用生成并发（6.14）。
- Codex memories 目录的 commit 只记在出处里（`memoriesHead`），没有用来跳过文件，判断是否变化只看单元哈希。
- `diverged`、`source-missing` 和 `--prune` 没有实现：单元旧文本产生的记录在审核时只通过 `previousMemoryIds` 列出来。

会话导入按 6.3、6.10、6.14 和 6.16 实现，下面这些地方做了调整或补充：

- 第 10 节第 1 条已经核对：Claude Code hook 的 `prompt_id` 就是 transcript 的 `promptId`，所以导入的轮次和捕获的轮次
  turnId 相同。
- `plan --include notes,sessions`（默认只有 `notes`）。`--limit`（默认 200）取 `--since` 之后最早的若干轮，`--since` 包含边界；
  超出的部分在报告里给出下一批的 `--since`。只为选中时间窗口内被跳过的轮次写计划行；早于 `--since`、超出 `--limit` 的轮次，
  以及分叉或恢复的会话重复的前面轮次只计数。同一轮次出现在两个会话里时，归先开始的会话，和捕获时一致。
- 会话轮次不检查文件哈希：消息随计划一起走，而进行中的会话的 transcript 一直在增长。单轮提问加回复超过 256 KiB 时跳过
  （`too-large`），单个 transcript 解压后最多读 512 MiB。
- 提问和回复都去掉本服务的 `<shared-memory>` 块和 Claude Code 注入的 `<system-reminder>` 块。Claude Code 的一轮从一条带
  `turnOrigin` 的 user 记录开始，到下一条带 `turnOrigin` 的 user 记录（不论来源）之前结束，而不是到下一条 `human` 记录之前：
  否则 `sdk` 或 `task_notification` 轮次的回复会被算到前面那个人工提问上。提问是那条 user 记录的文本，回复是这一轮最后一条带
  文本的 assistant 消息（同一 `message.id` 的文本块合并）；以 API 错误结束的轮次跳过（`api-error`）。没有任何一条提问带 `turnOrigin` 的 transcript 是 `unknown-format`，子目录（子代理 transcript）不读。项目目录先看
  第一个 transcript 前 16 KiB 里的 `cwd`，属于 workspace 才完整读取。
- Codex 读 `sessions/` 和 `archived_sessions/` 下的 `rollout-*.jsonl` 和 zstd 压缩的 `rollout-*.jsonl.zst`，以第一条 `session_meta`
  为准（后面的属于分叉来源的历史）。Codex 0.131 之前的 rollout 没有 `thread_source`，按 `unknown-format` 跳过；`chatgpt_handoff`
  等非 `user` 取值都跳过（第 10 节第 6 条仍待定）。一轮里的多条 UserMessage 合并成一个提问。
- 会话轮次在 propose 时登记成 run 的行：共享 `proposals` 表里 `itemKind: session-turn` 的行，ID 由 run 和 turnId 决定，带着整轮
  消息；不进评审文件，没有台账行（daemon 按 `platform + turnId` 去重），单独计数（`sessionTurns`、`turnsQueued`、
  `turnsRolledBack`）。apply 把它们标成已排队。
- outbox 由 CLI 写入：`import apply` 把这些轮次按计划顺序写进 `--outbox`，消息体带 `importRunId`，由现有上传器提交。同一个
  outbox 里已经有这一轮的作业（实时捕获的，或更早的 run 的）时不替换。中断后对已 apply 的 run 再执行 `apply` 只补写还没写进去
  的轮次。
- `--rate` 是每分钟最多写入的轮数（默认 20，`0` 表示一次写完、不等待），另加 `--max-pending`（默认 4）：outbox 里还没形成完的
  作业（`pending` 或 `accepted`，包括实时捕获的）达到这个数时等待。待处理数取自 outbox 而不是 `/admin/stats`：后者要统计每张表，
  而 outbox 正好反映上传器和形成的进度。
- `add_memories` 多了 `importRunId`。daemon 只接受这个 run 已经 apply 的轮次，而且平台、项目、会话和消息都要和登记的一致；
  新建的请求记下 `importRunId`，run 的行记下 `requestId` 和 `duplicate`。是否属于这次导入以请求上的 `importRunId` 为准，
  所以响应丢失后的重发仍算这次导入的。MCP 桥会去掉这个字段，模型不能提交导入的轮次。
- rollback 先把 outbox 里这个 run 还没上传的作业标为 `cancelled`（上传器不会发送），daemon 再关闭这个 run：之后带它的
  `importRunId` 的提交一律拒绝。只撤回来源请求全部属于这次导入的事实（`superseded` 并标记 `importRollback`），请求也标记
  `importRollback`；还在形成中的请求留到下一次 rollback（`partially_rolled_back`）。原始输入保留，turnId 也仍被占用，所以回滚
  过的轮次再导入只会被当作重复，不会再形成一次。

## 1. 现在的情况

安装器接好一个宿主之后，捕获 hook 只记录 hook 生效之后的新轮次，召回也只能看到 daemon 里已有的记忆。宿主在此之前
积累的内容都留在宿主自己的目录里：Claude Code 的 auto memory、Codex 的 memories、WorkBuddy 的 `MEMORY.md`，以及各
宿主的历史会话。换到另一个宿主，或者让几个宿主共用一个 `--project` 时，这些内容都看不到。

现在唯一的办法是让 Agent 逐条调用 `add_memories`。这样写进去的记录没有来源文件和原始时间，写之前没法整体预览，
写错了也没法整批撤销。

## 2. 目标与非目标

目标：

- 把宿主已经提炼好的记忆导入共享记忆，保留来源宿主、来源文件、原始时间和项目归属。
- 导入后的每条记录都足够小，能被自动召回注入。
- 可选地导入历史会话，并且和实时捕获的轮次互相去重。
- 先预览、再审核、最后写入；重复运行不产生重复记录；一次导入可以整体撤销。
- 写入之前，计划里就列出会产生哪些模型调用、有多少文本会发给模型服务。

非目标：

- 不替代宿主自己的记忆和规则文件，也不往宿主目录写东西。反方向的导出见第 10 节。
- 不做持续同步。重新运行一次导入就是一次增量更新，不常驻监听文件。
- 不提供 MCP 工具。导入和 `delete_memory`、consolidation 一样只走 admin 接口和 CLI，模型不能自行发起批量导入。
- 不把规则文件自动变成 instruction。
- 不进入 `eval/` 的评测协议。导入偏离了固定的 Mem0 算法，和 consolidation 一样不在评测中启用。

## 3. 其他项目的做法

| 做法 | 项目 | 做了什么 | 对本设计的影响 |
|---|---|---|---|
| 不调 LLM，直接导入已提炼的记忆 | [claude-mem `memory ingest`](https://github.com/thedotmack/claude-mem/pull/2829)（2026-10-01 合并） | 读 `~/.claude/projects/<slug>/memory/*.md`；跳过 `MEMORY.md` 索引、空文件、软链接和超过 64 KiB 的文件；按内容哈希去重，重跑不重复写；时间回填为文件 mtime；用同目录 transcript 的 cwd 确定项目；只接受 Claude 配置目录内的路径；有 `--dry-run`。作者认为对已提炼的文本再生成一遍既有损又花钱 | 已经足够小的笔记原样导入，只做嵌入；按内容哈希保证幂等；限制读取范围 |
| 预览、执行、逐项报告 | [Hermes `hermes claw migrate`](https://hermes-agent.nousresearch.com/docs/user-guide/skills/optional/migration/migration-openclaw-migration) | 不加 `--execute` 只预览；OpenClaw 的 `MEMORY.md`、`USER.md` 转成记忆条目；默认不迁移密钥；技能冲突可选跳过、带备份覆盖或改名；输出带 migrated/conflict/skipped 状态的 JSON 报告 | 先出计划和提议，审核后再写；每个单元都有状态和原因 |
| 让 LLM 重读历史会话 | [Letta Code `/init`](https://www.letta.com/blog/context-repositories/) | 把 Claude Code、Codex 的历史切片，交给并行子代理在各自的 worktree 里总结，再合并进 git 管理的记忆 | 会话导入成本高，只能显式开启；整理用受约束的单次抽取（6.6），不用自由的 Agent |
| 回放 transcript | [claude-mem PR #4201](https://github.com/thedotmack/claude-mem/pull/4201)（未合并即关闭） | 把 `~/.claude/projects/*.jsonl` 重放进实时监听管线，保留原始时间，默认脱敏；评审指出只读 `content[0]` 会丢掉 text 块之后的工具调用 | 解析 transcript 要按记录类型筛选，并且先脱敏 |
| 回填使用原始时间 | [Mem0 `timestamp`](https://docs.mem0.ai/platform/features/timestamp)、[Zep `created_at`](https://help.getzep.com/adding-business-data)、[Graphiti `reference_time`](https://help.getzep.com/graphiti/graphiti/adding-episodes) | 都要求传事情发生的时间，而不是导入的时间。Graphiti 的批量导入不做边失效，适合空图；Zep 对同一个图的写入按顺序处理，大批量会慢；[Mem0](https://docs.mem0.ai/core-concepts/memory-operations/add) 提醒 raw 和 infer 两种方式写同一事实会产生重复 | 保留 `statedAt`；同一份知识只走一种写法 |
| 规则文件按配置迁移 | [Claude Code `/init`、`/import`](https://code.claude.com/docs/en/memory)；[Cursor 下线 Memories](https://forum.cursor.com/t/are-my-memories-gone/144057) | `/init` 把 Cursor、Copilot 等工具的规则并进 `CLAUDE.md`，`/import` 一次性复制 `AGENTS.md`、MCP server、命令和技能；Cursor 2.1 移除 Memories 时让用户导出成 `.mdc` 规则 | 规则文件在宿主之间迁移，不进记忆库 |
| 产品之间迁移 | [OpenMemory migrate](https://openmemory.cavira.app/docs/migration/mem0)、[Letta Agent File](https://docs.letta.com/guides/agents/agent-file)、[basic-memory import](https://www.mintlify.com/basicmachines-co/basic-memory/api/cli/import-commands) | 从 Mem0、Supermemory 导入并保留时间戳和元数据；`.af` 打包整个 agent；basic-memory 把 ChatGPT、Claude 的导出转成 Markdown | 接受一种通用的规范化输入 |
| 让源助手自己导出 | Claude 和 Gemini 的 Import Memory（[Claude](https://pcworld.com/article/3076376/claude-can-now-import-chat-histories-from-chatgpt-and-other-ais.html)、[Gemini](https://www.pcworld.com/article/3100804/google-gemini-can-now-import-your-chatgpt-or-claude-chat-history.html)） | Claude 给一段提示词，让对方助手逐条列出它记得的内容；贴回后 Claude 还要花约 24 小时并进记忆，之后用户可以查看和修改。Gemini 也接受完整聊天记录的 ZIP | 源助手只原样导出，整理由导入方完成（6.11）；读不到底层存储时用它兜底 |
| 反面教材 | [openai/codex#55060](https://github.com/openai/codex/issues/55060) | Codex 桌面版按目录导入 `~/.claude/projects`，把 claude-mem 内部的 SDK 会话当成用户对话导入了几百条。用户要求导入前能预览，并按来源标记筛选，而不是按目录或标题 | 按来源字段筛选；计划里列出被跳过的会话和原因 |

## 4. 各宿主已有的记忆

| 宿主 | 位置 | 内容 | 格式 | 时间 | 项目归属 | 标识 |
|---|---|---|---|---|---|---|
| Claude Code | `~/.claude/projects/<slug>/memory/*.md` | auto memory。`MEMORY.md` 是索引，其余每个文件一条笔记，frontmatter 有 `type`（user/feedback/project/reference）和 `modified` | [公开](https://code.claude.com/docs/en/memory) | `modified`，缺失时用 mtime | `<slug>` 由 git 仓库推出，同一仓库的 worktree 共用 | 文件名 |
| Claude Code | `~/.claude/projects/<slug>/*.jsonl` | 会话 transcript。user 记录带 `promptId`、`turnOrigin`、`origin.kind`、`isSidechain`、`entrypoint` | 不公开 | 每条记录的 `timestamp` | `cwd`；每个 worktree 一个 `<slug>` | `sessionId`、`promptId` |
| Claude Code | `CLAUDE.md`、`.claude/rules/`、`~/.claude/CLAUDE.md` | 规则文件 | 公开 | — | 文件位置 | 路径 |
| Codex | `~/.codex/memories/rollout_summaries/*.md` | 每个线程一篇总结：一级标题、开头的 `thread_id`、`updated_at`、`rollout_path`、`cwd`，然后是若干 `## Task N` 节 | 不公开，只有第三方分析 | `updated_at` | `cwd` | `thread_id` |
| Codex | `~/.codex/memories/MEMORY.md`、`memory_summary.md`、`raw_memories.md`、`skills/` | 合并后的手册、手册的摘要、阶段一输出的合并稿、技能 | 不公开 | — | `MEMORY.md` 按任务组引用 rollout summary | — |
| Codex | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` | 会话记录。`session_meta` 有 `cwd`、`thread_source`；`task_complete` 有 `turn_id` 和 `last_agent_message` | 不公开 | 事件时间 | `session_meta.cwd` | `session_meta.id`、`turn_id` |
| WorkBuddy | `~/.workbuddy/MEMORY.md`、`memory/<uid>_memory.md`、`USER.md` | 用户级记忆和用户画像。同目录的 `SOUL.md`、`IDENTITY.md`、`BOOTSTRAP.md` 是人设 | 不公开 | mtime | 无，属于用户级 | 路径 |
| DeepSeek Harness | `~/.dsh/sessions/<slug>/<id>/session.v4.jsonl.zstd` | zstd 压缩的会话日志，没有记忆文件 | 不公开 | 待查 | `<slug>` 由路径编码 | 目录名 |

补充观察：

- `~/.codex/memories/` 本身是一个 git 仓库。`raw_memories.md` 和 `memory_summary.md` 都是派生文件。
- Claude Code 会在 `cleanupPeriodDays` 之后删除旧 transcript，记忆文件不受影响，所以能导入的会话历史有限。
- 已经拿一条 Codex 实时捕获的轮次核对过：hook 的 `session_id` 等于 rollout 的 `session_meta.id`，hook 的 `turn_id`
  出现在同一个 rollout 的事件里，`task_complete` 带最终回复。Claude Code 也核对过（2.1.305，给临时项目装上捕获后用
  `claude -p` 跑一轮）：outbox 里的 native id 就是 transcript 的 `promptId`，会话 ID 就是 `sessionId`。这一轮的 `turnOrigin` 是
  `sdk`，导入本身会跳过它；`human` 记录同样带 `promptId`，标识的取法相同。
- Claude transcript 里，工具结果、`isMeta` 记录和 `<command-name>` 记录都是没有 `turnOrigin` 的 user 记录（工具结果带
  `promptId`）；assistant 记录不带 `promptId`，每条记录一个内容块，同一次 API 响应的记录共用 `message.id`。`claude -p` 的提问是
  `turnOrigin: sdk`。
- Codex 的 rollout 较旧时会压缩成 `.jsonl.zst`。每条记录是 `{timestamp, type, payload}`；`thread_source` 从 0.131 开始才有。
- Claude transcript 的 `turnOrigin` 观察到 `human`、`sdk`、`task_notification`、`peer` 四种取值，`origin.kind` 和它
  对应。`promptSource` 不能用来区分人工输入：桌面端发给模型的人工提问也记成 `sdk`。Codex 的 `thread_source` 观察到
  `user`、`subagent`、`guardian_review`、`chatgpt_handoff`。按来源筛选靠的就是这些字段。
- 用服务自己的 token 估算（中文一字算 1，其他字符每 4 个算 1，见 [util.rs](../../crates/memory-rules/src/util.rs)，
  每条记录另加 8）量过本机数据的长度：
  - Codex 的 40 篇 rollout summary 整篇中位数 1549，没有一篇放得进 400 的召回预算。它们由 40 个一级标题和 95 个
    `## Task N` 节组成，按标题切成的 175 块里有 71 块放不进 400；按列表项切，1284 条的中位数是 40，最大 171。
  - Claude 的两篇 auto memory 笔记分别约 340 和 430。
  - WorkBuddy 的 5 个标题节里有 1 节超过 400。

## 5. 为什么现有写入路径不够

| 路径 | 问题 |
|---|---|
| `add_memories` 的 `text` | 时间固定为写入时刻，角色固定为 user（[formation.rs](../../crates/memory-algo-mem0/src/formation.rs)）。原始时间丢失，Codex 写的总结会被当成用户说的话 |
| `messages` 加 `infer:false` | 时间保留，但 origin 记成 `explicit`。`get_context` 把 `explicit` 和 `user` 当作已确立的事实（[algorithm.rs](../../crates/memory-engine/src/runtime/algorithm.rs) 的 `context_origins`），consolidation 又排除 `explicit`（[domain consolidation](../../crates/memory-domain/src/consolidation.rs)）。导入的笔记会被当成用户明确说过的话，而且永远不会被合并 |
| `messages` 加 `infer:true` | 用面向对话的 additive 提示词把已经提炼过的文本再抽取一遍，origin 仍是 user 或 assistant，没有依据片段的校验 |
| `correct_memory` | 更正记录的 origin 是 `explicit`（[correction.rs](../../crates/memory-engine/src/app/management/correction.rs)），不能用来表示"源笔记改了" |
| `memoryType:"instruction"` | 同步、原样保存、最多 1024 个字符，只用于用户明确提出的行为要求。可以承接人确认过的指令候选，不适合批量导入 |

另外几条约束也会影响设计：

- 去重键是 `platform + turnId`（[作用域与幂等](../api.md#scope-and-idempotency)），捕获用
  `turn_key(project, session, native_id)` 生成 turnId（[outbox.rs](../../crates/memory-service/src/harness/outbox.rs)）。
- outbox 按 `seq` 顺序上传，每个租户只有一个有序的形成 worker（[写入路径](../architecture.md#write-path)）。
- Mem0 形成写入的 fact 固定是项目作用域，只有 instruction 能写成 global 或 platform 作用域。
- consolidation 合并时，结果的 origin 取输入中最弱的一个：有 assistant 就是 assistant，否则有 inferred 就是 inferred，
  否则是 user（[domain consolidation apply](../../crates/memory-domain/src/consolidation/apply.rs)）。
- 自动召回每次提问用 topK 5、`tokenBudget` 400 调 `search_memories`（[recall.rs](../../crates/memory-service/src/harness/recall.rs)）。
  daemon 先截到 topK 条，再按 `est_tokens(text) + 8` 装完整记录，装不下的整条跳过，也不从后面补
  （[retrieval.rs](../../crates/memory-algo-mem0/src/retrieval.rs)）。估算超过 392 的记录因此永远不会被自动注入，
  接近 400 的记录会挤掉同一次提问的其他结果。Agent 主动调 MCP 的 `search_memories` 时，默认预算是 2000（`defaultTokenBudget`）。

## 6. 设计

### 6.1 流程

```text
宿主文件，或宿主 Agent 导出的 JSONL
  → plan：离线、只读。筛选、映射项目、切出导入单元，写成计划文件，人可以删改
  → propose：daemon 处理每个单元，小单元原样采用，其余调模型抽取，结果存成提议，不写记忆
  → 审核：查看提议，接受或拒绝；指令候选逐条确认
  → apply：写入 imported 记录和导入台账；rollback 可以整体撤销
历史会话：plan → propose（只登记，不调模型）→ apply（写进 outbox，走正常的 Mem0 形成，turnId 与捕获相同）
```

### 6.2 三类来源

| 来源 | 例子 | 默认 | 写入方式 |
|---|---|---|---|
| 人写的规则文件 | `CLAUDE.md`、`AGENTS.md`、`.claude/rules/`、`.cursor/rules` | 只列在计划里，不导入 | 宿主自己会加载，导入后会重复注入。仓库里的文件不可信，自动变成 instruction 就等于埋下一条对所有宿主生效的持久提示注入。`--include rules` 时按标题切成最多 1024 个字符的指令候选，由人逐条确认后走现有的 instruction 路径 |
| 宿主提炼的笔记 | Claude `memory/*.md`、Codex `rollout_summaries/*.md`、WorkBuddy `MEMORY.md` 和 `memory/*_memory.md` | 导入 | 新的 `imported` 来源（6.8）。按 6.5 的规则原样采用或抽取（6.6），都要经过 propose 和审核（6.7） |
| 原始会话 | Claude `*.jsonl`、Codex rollout | `--include sessions` 时才导入 | 走正常的 Mem0 形成，使用原始时间戳和捕获时的 turnId（6.10） |

每个宿主只取一种单元，同一份知识不导入多次：

- Codex 取 `rollout_summaries`，跳过 `raw_memories.md` 和 `memory_summary.md`。`MEMORY.md` 里跨线程的总结没有对应的
  单篇来源，是否导入列为待定问题。`skills/` 是程序性知识，当前 main 不形成 skill，不导入。
- Claude 跳过 `MEMORY.md` 索引。`type: feedback` 的笔记照常导入，同时列为指令候选（超过 instruction 的 1024 字符上限时
  只取第一段），确认后另写一条 instruction。
- WorkBuddy 跳过 `SOUL.md`、`IDENTITY.md`、`BOOTSTRAP.md`。
- 同一个 Codex 线程既选了 rollout summary 又选了会话时，计划只保留会话，summary 标为 `covered-by-session`。

### 6.3 按来源筛选

| 宿主 | 导入 | 跳过 |
|---|---|---|
| Claude Code | `turnOrigin` 为 `human` 且 `isSidechain` 为 false 的提问，以及同一轮最后的助手文本 | 工具结果；`sdk`、`task_notification`、`peer` 来源的轮次；attachment 记录（hook 注入的上下文在这里）；thinking 和工具调用 |
| Codex | `thread_source` 为 `user` 的线程中的 `UserMessage` 和 `task_complete.last_agent_message` | `subagent`、`guardian_review` 线程；被中止的轮次；developer 消息和注入的环境上下文；`chatgpt_handoff` 等其他取值默认跳过 |
| 所有宿主 | — | 本服务自己的 `<shared-memory>` 召回块（否则召回内容会被再次形成记忆）；超过大小上限的文件；软链接；宿主数据目录之外的路径 |

两个宿主的会话格式都不公开。缺少筛选所依赖的来源字段（Claude 的 `turnOrigin` 或 `origin.kind`，Codex 的
`thread_source`）的记录或会话一律不导入，标为 `unknown-format`，不按猜测处理。每个被跳过的会话或轮次都在计划里写明原因，
方便复核。

### 6.4 项目映射

- 一次导入对应一个 `--project` 和一个或多个 `--workspace` 目录。
- Claude 的 auto memory 目录按 git 仓库共享，transcript 按 worktree 分目录。两者的 cwd 都先归到 git 根（worktree 归到
  主仓库），再和 `--workspace` 比较。
- auto memory 目录本身不带 cwd，用同一个 `<slug>` 下 transcript 的 `cwd` 确定，和 claude-mem 的做法一样。没有 transcript
  时才从 `<slug>` 反推路径。路径里的 `/` 和 `-` 都编码成 `-`，反推有歧义，计划里标为 `inferred-path`，需要人确认。
- Codex 的 rollout summary 和 rollout 都带 `cwd`，直接比较。
- `--workspace` 之外的单元标为 `other-project`，不导入。
- WorkBuddy 的用户级记忆不属于任何项目，默认导入到 `--project`，计划里提示它们原本是跨项目的（见 6.15）。

### 6.5 导入单元的大小

导入单元是最终写成一条记忆的文本。它的大小由自动召回的预算决定，而不是由源文件的结构决定。下面的 cost 指 daemon 打包时
的记录成本 `est_tokens(text) + 8`：

- **硬上限**：cost 不超过召回预算的一半，默认 200，即正文最多 192 个估算 token（约 190 个汉字或 760 个英文字符）。
  取一半，是为了让任何一条被注入时，同一次提问至少还能放下另一条。
- **目标**：cost 接近 `searchBudget / topK`，默认 80，正文约 72，和一条 Mem0 事实差不多。抽取时要求输出接近这个长度；
  原样采用的单元只要不超过硬上限就行。
- 召回预算和 topK 默认取安装器写入的召回参数（400、5）。用户改过召回命令时，用 `--recall-budget`、`--recall-top-k` 指定。
  计算出的上限记进计划和 import run。
- 这是由召回预算推出来的固定规则，不是从试跑分数里选的参数（[AGENTS.md](../../AGENTS.md)）。

各来源的输入单元和处理方式：

| 来源 | 输入单元 | 处理 |
|---|---|---|
| Claude auto memory | 一篇笔记的正文，`description` 作为第一行 | 不超过硬上限就原样采用，否则抽取 |
| Codex rollout summary | 一个 `## Task N` 节，前面带上该篇的一级标题 | 一律抽取：节里混着任务进度和过程记录，长短都要筛 |
| WorkBuddy 记忆文件 | 一个标题节 | 不超过硬上限就原样采用，否则抽取 |
| 宿主 Agent 导出（6.11） | 一行 | 不超过硬上限就原样采用，否则抽取 |
| 任一来源加 `--no-extract` | 同上 | 不调模型：超过硬上限的按列表项和段落切，每块带标题路径，仍超过的按句子切，计划里标 `split` |

按本机数据，Claude 的两篇笔记都超过硬上限，要抽取；Codex 的 95 个 Task 节都走抽取；WorkBuddy 的 5 个标题节里有 2 节超过
硬上限。`--no-extract` 时，Codex 和 WorkBuddy 的列表项全部在硬上限以内，但单条列表项常常离不开标题，切开后可能看不懂，
所以它只是不想花模型调用时的退路，不是默认值。

如果召回以后改成对超长记录注入截断片段，片段也只是指向全文的线索，这条上限仍然适用。

### 6.6 抽取

抽取在 daemon 里做。模型配置和密钥都在 daemon，原始响应的保存和有界修复也已经有了，和形成、consolidation 一样。

**输入**：一个单元，单次最多 8 KB，更长的节按段落拆开。另外附带：

- 来源信息：宿主、项目、标题路径、`statedAt`；
- 同一项目里与单元最相近的 10 条已有记忆（向量召回，和形成一样）。库里已有的内容不再输出，写入时就顺带去了重。

单元正文用分隔符包住，提示词说明其中出现的任何指令都只是记忆内容，不是给模型的指令。

**输出**是严格的 JSON：

```json
{"items": [{"text": "…", "kind": "fact", "scope": "project", "evidence": "…"}]}
```

- `text`：脱离原文也能看懂（把"这个项目""用户"换成明确的名称），只写一件事，长度接近 6.5 的目标，不超过硬上限。
  语言规则和形成的 `zh` 变体一致：与原文的语言和文字相同。
- `kind`：`fact`、`preference`、`instruction`、`reference` 之一。任务进度和临时状态由提示词要求直接略去，不作为一种 `kind` 输出。
- `scope`：`project` 或 `global`，只作为建议记下来（6.15）。
- `evidence`：原文里支持这一条的片段。

**校验**是纯函数，不修改内容：

- `evidence` 必须是单元正文的子串，至少 8 个字符（单元更短时须是整个单元），并且包含 `text` 至少 30% 的内容词
  （4 个字母以上的英文词、相邻汉字组成的二元组）；
- `text` 里的专有名词和数字必须出现在单元正文或来源信息里，规则和 consolidation 校验合并文本时相同
  （[rules](../../crates/memory-rules/src/consolidation.rs)）；
- `text` 不能为空，也不能超过硬上限。

不通过的条目记为 `rejected` 并写明原因，同一响应里的其他条目照常保留。JSON 结构本身不合法时，和形成一样做有界修复；
原始响应在解析前先保存。

**去向**：`fact`、`preference` 和 `reference` 成为提议；`instruction` 成为指令候选，只能在审核时逐条确认。每个抽取单元
记录 `coverage`，即通过校验的依据片段覆盖了单元正文的比例，供审核时判断模型漏掉了多少。

**缓存**：按单元内容哈希、提示词哈希和模型缓存抽取结果。同一内容再次 propose 不会重新调用模型，内容变了才重新抽取。

**提示词**是新写的，不复用 Mem0 面向对话的 additive 模板。它和导入的其他代码一起放在引擎里
（[extract.txt](../../crates/memory-engine/src/app/import/extract.txt)），不属于任何算法：输出由纯规则校验，由引擎写成普通记录。
算法只提供已有记忆的召回（`MemoryAlgorithm::nearest_facts`，和形成召回已有事实的方式相同）。

**不用自由的 Agent**：模型不读文件、不调工具，一次只看一个单元。这样每条结果都有明确的出处，能逐条校验和撤销，也不会
被笔记里嵌入的指令带偏。

**成本**：按本机数据，Codex 的 95 个 Task 节各一次，加上 Claude 的 2 次，共约 97 次生成调用。Codex `MEMORY.md` 是否
导入还没定（第 10 节），导入的话按 287 个标题节计算，最多再多 287 次。计划的汇总会先列出这些数字。

### 6.7 propose、审核与 apply

和 consolidation 一样分成提议和应用两步，提议阶段不写任何记忆：

1. `agent-memoryd import propose PLAN.jsonl` 把计划里决定导入的单元发给 daemon。daemon 对每个单元原样采用或抽取，把结果
   存成一次 import run 和一组提议，返回汇总：每个单元产生了几条提议、几条被校验拒绝、几条指令候选、丢弃了几条过程记录、
   实际用了多少次生成调用。
2. `agent-memoryd import show RUN --out REVIEW.jsonl` 导出评审文件，每条提议一行：文本、类型、来源单元、依据片段、校验
   结果，以及库里最相近的已有记忆。人把 `decision` 改成 `accept` 或 `reject`。事实提议默认 `accept`，指令候选默认
   `reject`，要逐条改成 `accept`。评审时不能改写文本，需要改的话，apply 之后用 `correct_memory`。
3. `agent-memoryd import apply RUN --review REVIEW.jsonl` 写入接受的提议。不带评审文件时，通过校验的事实提议全部写入，
   指令候选一条也不写。
4. apply 时逐条复查，和 consolidation 的 apply 一样：来源单元的哈希和 propose 时不同、同一项目里已有内容相同的 `current`
   记录，或者提议不再通过校验时，标为 `skipped` 并写明原因。
5. 交互运行时（包括安装器），propose 之后显示汇总和几条样例，再问是否 apply；非交互运行只有显式执行 apply 才会写入。

run 的状态：`proposing` → `proposed`（或 `failed`）→ `applying` → `applied` → `rolling_back` → `rolled_back` 或
`partially_rolled_back`。中断后对同一个 run 重新执行会从未完成的单元继续。提议的状态：`proposed`、`rejected`（校验未通过）、
`dropped`（评审拒绝）、`applied`、`skipped`、`rolled_back`。

### 6.8 `imported` 来源

新增 origin `imported`：

- 不加入 `context_origins`。`get_context` 把它当作历史参考，和 assistant 事实一样，不当作已确立的事实。
- 是 consolidation 的候选，可以和实时形成的事实、其他宿主导入的记录合并。合并规则要把它算作"弱"来源：合并结果只要
  有一个输入是 `imported`，就不能是 `user`。不改这条规则的话，导入的记录和用户事实合并后会变成 `user`。
- `source.platform` 是来源宿主的平台标签（`claude-code`、`codex`、`workbuddy`），和捕获时一致。
- `statedAt` 取来源时间：Claude frontmatter 的 `modified`，缺失时用 mtime；Codex 的 `updated_at`；宿主 Agent 导出的
  `date`。导出条目没有日期时留空，免得它在 consolidation 里被当成最新的说法。`effectiveAt` 留空，和 Mem0 事实一样；
  `recordedAt` 是写入时刻。
- `ingestionMode` 记为 `imported`。实体投影只接受能证明是 `inferred` 的记录（[entities.rs](../../crates/memory-algo-mem0/src/entities.rs)），
  所以导入的记录默认不进实体投影：检索靠语义和 BM25，consolidation 只能用向量把它们聚类。是否放开列为待定问题。
- `detail.import` 记录出处，例如：

```json
{
  "runId": "ir…",
  "proposalId": "ip…",
  "unit": "codex:thread:019a…#task-1",
  "host": "codex",
  "kind": "fact",
  "mode": "extracted",
  "path": "~/.codex/memories/rollout_summaries/2026-01-02T03-04-05-AbCd-example_task.md",
  "nativeId": "019a…",
  "unitHash": "…",
  "sourceUpdatedAt": "2026-01-02T03:04:05Z",
  "headingPath": ["…", "Task 1: …"],
  "extraction": {"evidence": "…", "model": "…", "promptHash": "…", "responseLogId": "…"}
}
```

`mode` 是 `verbatim`、`split` 或 `extracted`；只有 `extracted` 带 `extraction`。宿主 Agent 导出的单元还带
`via: "agent-export"` 和行号（6.11）。

### 6.9 导入台账、幂等和更新

台账为每个导入单元保存一行：`(tenant, host, unit) → {fileHash, unitHash, memoryIds, runId, status}`。单元键由宿主、
原生 ID（没有时用相对宿主目录的路径）和单元在文件里的位置组成。

- 以文件为单位比较。文件哈希不变就整文件跳过，计划里标 `unchanged`。
- 文件变了就逐个单元比较，单元哈希相同的保留，新单元照常 propose。
- 原样采用的单元和记录一一对应：内容变了就写一条新的 `imported` 记录 supersede 旧记录（旧记录变为 `superseded`，
  `validUntil` 取新记录的 `statedAt`，新记录的 `supersedes` 指向旧记录）。不调 `correct_memory`。没有 `statedAt` 的单元
  （例如不带日期的导出条目）无法判断新旧，不做 supersede：新内容写成新记录，旧记录按下一条抽取单元的方式标 `source-changed`。
- 抽取的单元会产生多条记录：内容变了就重新抽取，新提议照常审核。由旧内容抽出的记录不自动撤回，评审文件里列出它们并标
  `source-changed`，由人决定保留还是撤回；新旧说法冲突的情况交给 consolidation 按 `statedAt` 处理。
- 台账指向的记录已经不是 `current`（被合并、更正或删除）时不碰它，计划里标 `diverged`。
- 同一项目里已有内容哈希相同的 `current` 导入记录时，不再写新记录，只把台账里的这个单元指向它，和 Mem0 的精确去重一样。
- 源文件或源单元消失（例如 Codex 遗忘了一条记忆）时默认不动，计划里标 `source-missing`；加 `--prune` 时按 6.16 的方式
  逻辑撤回。
- Codex 的 memories 目录是 git 仓库，台账记下导入时的 commit，下次只检查之后改过的文件。Claude 和 WorkBuddy 没有版本库，
  靠 mtime 和文件哈希判断。

### 6.10 会话导入复用捕获的身份

- 每一轮写成一个 `messages` 批次（用户提问加最终回复），形状和捕获写进 outbox 的记录一样。
- turnId 用 `turn_key(project, session, native_id)`。Claude 的 native_id 取 `promptId`，session 取 `sessionId`；Codex 的
  native_id 取 `turn_id`，session 取 `session_meta.id`。
- 平台标签用宿主自己的（`claude-code`、`codex`），不用 `import`。daemon 按 `platform + turnId` 去重，所以已经实时捕获过的
  轮次不会再形成一次，重复运行导入也一样。用错标签就会重复形成。
- 时间戳用原始时间：提问取用户记录的时间，回复取该轮最后一条助手文本的时间（Codex 用 `task_complete`）。
- 每个会话内按时间顺序提交。Mem0 形成的历史上下文取同一 platform/project/session 里前面已经形成的请求，按顺序提交，
  指代解析才能看到正确的前文。
- Claude 的一轮从一条 `turnOrigin` 为 `human` 的 user 记录开始，到下一条这样的记录之前结束。提问取这条记录的字符串或
  text 块；回复取这一轮最后一条 assistant 记录里的 text 块，忽略 thinking 和工具调用。
- Codex 的一轮按 `turn_id` 归组：提问取 `item_completed` 里的 `UserMessage`，回复取 `task_complete.last_agent_message`；
  `turn_aborted` 或没有最终回复的轮次不导入，和捕获的规则一致。
- 会话轮次在 propose 时只登记，不调模型，也没有提议需要审核；要导入哪些会话在计划阶段决定。apply 时把它们写进 outbox，
  由已有的上传器提交。形成出的事实大小由 Mem0 的抽取决定，不受 6.5 约束。
- 每一轮是一次生成调用。计划里写明轮数、预计的生成调用次数和嵌入次数。

### 6.11 宿主 Agent 导出

有些平台读不到底层存储，例如 ChatGPT、Gemini、Cursor 的云端记忆；有些格式不明或经常变，例如 DeepSeek Harness 的压缩日志、
没有文档的 Codex 记忆。这时用 Claude Import Memory 的办法：给宿主的 Agent 一段提示词，让它把记得的内容导出成 JSONL，
再走同样的 plan、propose、审核和 apply。

源 Agent 只负责原样导出，不做整理。整理交给我们的抽取（6.6）和 consolidation：源 Agent 一归纳，信息和出处就丢了，结果
也没法校验。

`agent-memoryd import prompt --project my-project` 打印提示词，实现时把中英文两个版本放在 `examples/` 下。中文版：

```text
我要把你保存的记忆迁移到另一个记忆服务。请列出你为我保存的所有记忆，以及你从过去的对话中了解到、和项目 my-project 有关的信息。

要求：
1. 只输出一个代码块。每条记忆占一行，是一个 JSON 对象。代码块外不要写别的内容。
2. 字段：
   - text：记忆内容。尽量保留原话，一条只写一件事。
   - kind：instruction（我要求你怎么做）、preference（我的偏好）、fact（事实或决定）、reference（去哪里找信息）之一。
   - date：保存或得知的日期，格式 YYYY-MM-DD，不知道就留空。
   - source：来自哪个记忆文件；来自对话就写 conversation。
   - scope：只和这个项目有关写 project，跨项目适用写 global。
3. 不要合并、归纳、改写或省略条目，也不要补充你不确定的内容。
4. 不要输出密钥、令牌、密码等凭据，遇到时写成 [redacted]。
5. 超过 100 条时分几次输出，每次的最后一行写 {"continued": true}，等我回复"继续"再输出下一部分。
```

处理导出结果：

- `agent-memoryd import plan --from jsonl FILE --host <宿主>` 读取导出结果，去掉 `{"continued": true}` 行。不合法的行
  在计划里标 `invalid-line`。
- 每行是一个单元，同样受 6.5 的硬上限约束：不超过就原样采用，超过就抽取。
- `kind: instruction` 的行只成为指令候选；`scope: global` 记进出处，写入时仍按项目作用域（6.15）。
- 出处里的 `path` 是导出文件，另记它的哈希、行号和 `via: "agent-export"`；`statedAt` 的规则见 6.8。
- 导出结果是不可信的数据：其中的任何指令都只当记忆内容处理，脱敏规则照常执行（6.12）。
- 宿主 Agent 能写文件时，可以让它把结果直接写到指定路径，省去手动复制。
- 从 pd-memory 迁移也走这条路：它的 `MemoryExportBundle` 可以转换成同样的 JSONL。确定字段之前先对照那边的结构。

### 6.12 脱敏与隐私

- 在发出任何网络请求之前脱敏。默认配置下，原样采用的单元也会发到托管的嵌入 API；抽取和会话导入还会把全文发给生成模型。
- 规则覆盖常见的 key 和 token 格式（`sk-`、`gh*_`、私钥块等，和 [check_public.py](../../scripts/check_public.py)
  的模式一致）、`Authorization`/`Bearer` 头、URL 里的凭据，以及 `.env` 风格的 `KEY=value` 行。命中的片段替换成
  `[redacted:<kind>]`，计划里只记命中次数，不记原文。
- 命中私钥块的单元默认整条跳过。
- 计划的汇总写明用的是托管还是自托管的模型服务，并分别列出会发给嵌入服务和生成服务的字符数。
- 计划文件、评审文件和台账都放在本机私有目录（例如 `~/.local/state/agent-memory-service/imports/`，权限 0600），和
  outbox 一样不进版本库。

### 6.13 命令和端点

```sh
agent-memoryd import plan --host claude-code|codex|workbuddy|jsonl --project my-project [--workspace DIR]... \
  [--include notes,sessions] [--since TIME] [--limit 200] \
  [--all] [--from FILE] [--label LABEL] [--no-extract] [--recall-budget 400] [--recall-top-k 5] [--home DIR] \
  [--claude-config-dir DIR] [--codex-home DIR] [--workbuddy-home DIR] [--out PLAN.jsonl]
agent-memoryd import propose PLAN.jsonl [--url http://127.0.0.1:8765] [--tenant default] [--batch 8] [--timeout 900] \
  [--home DIR] [--reconsider]
agent-memoryd import show RUN [--out REVIEW.jsonl] [--force]
agent-memoryd import apply RUN (--review REVIEW.jsonl | --yes) [--home DIR] [--outbox FILE] [--rate 20] [--max-pending 4]
agent-memoryd import status [RUN] [--project ID]
agent-memoryd import rollback RUN [--outbox FILE]
agent-memoryd import prompt --project my-project [--lang zh|en]
```

以上是已实现的形式（会话导入的选项见第 0 节）。

- `plan` 只读宿主文件，不访问网络，也不需要 daemon。默认只包含 `notes`。
- 计划是 JSONL 文件。第一行是汇总：各处理方式（`verbatim`、`extract`、`split`、`session-turn`）的单元数、跳过原因的统计、
  预计的生成和嵌入调用、会发出的字符数、使用的模型服务。之后每行一个单元，人可以删行或改 `decision`：

```json
{"unit":"codex:thread:019a…#task-1","kind":"note","mode":"extract","decision":"import","reason":"new","project":"my-project","statedAt":"2026-01-02T03:04:05Z","cost":612,"redactions":{"token":1},"calls":{"generation":1}}
{"unit":"claude-code:session:6f2e…/prompt:3b9c…","kind":"session-turn","decision":"skip","reason":"turnOrigin:sdk"}
```

- `propose` 先重新计算文件哈希。源文件在 plan 之后改过的单元标为 `stale-plan` 并跳过。
- 端点和 consolidation 的三个端点对应，都不放进 MCP：

| 端点 | 作用 |
|---|---|
| `POST /admin/import_propose` | 接收计划里的单元（每次最多 64 个，暂定），原样采用或抽取，存 import run 和提议，不写记忆；会话轮次只登记 |
| `POST /admin/import_apply` | `{"runId": "ir…", "decisions": {...}}`，逐条复查后写入。每条提议一个事务：记录、证据行、台账行和 `upsert` 索引任务，提议行本身记下写入、取代了什么；嵌入交给索引器，和 consolidation 合并结果的写法一样（[app/consolidation.rs](../../crates/memory-engine/src/app/consolidation.rs)）。会话轮次写进 outbox |
| `POST /admin/import_rollback` | `{"runId": "ir…"}`，逆序撤销（6.16） |
| `POST /admin/import_run`、`import_runs` | 读一个 run 和它的提议，或按时间倒序列出 run |

```json
POST /admin/import_propose
{"project": "my-project", "host": "codex", "finish": true, "planHash": "…",
 "limits": {"recallBudget": 400, "recallTopK": 5},
 "units": [{"unit": "codex:019a…#task-1", "host": "codex", "mode": "extract", "text": "…",
            "statedAt": "2026-01-02T03:04:05Z", "fileHash": "…", "unitHash": "…", "source": {}}]}
```

- 接受的指令候选在 apply 时用 `memoryType:"instruction"` 写入，turnId 用 `import:<unit>:<unitHash>`，重跑不会重复写。
- `prompt` 打印宿主 Agent 的导出提示词（6.11）。

### 6.14 吞吐和实时捕获

outbox 按 `seq` 先进先出，每个租户只有一个有序的形成 worker。几千轮历史会话一次进队，之后的实时轮次就要排在后面。
另开一个 outbox 文件也绕不开 worker 的串行，形成环节没有优先级可用。因此会话导入要：

- 默认带 `--limit`（例如 200 轮），需要更多时分批运行；
- 用 `--rate` 限速，每次入队前查看 `/admin/stats` 里待处理的任务数，超过阈值就等待；
- 可以中断和续传：台账和 outbox 都按单元去重。

抽取不经过形成 worker，但和形成共用 daemon 的生成并发，`service install` 默认只有 1 个。目前抽取不会主动让出，和形成
按到达顺序轮流使用这个名额；大批导入时可以把计划分几次 propose。原样采用的单元只占用索引器和嵌入调用。

### 6.15 作用域限制

Mem0 形成写入的 fact 固定是项目作用域。WorkBuddy 的 `MEMORY.md` 里有专门写给跨项目复用的内容，Claude 的
`type: user` 笔记也和具体项目无关，抽取和导出结果里还会有 `scope: global` 的建议，但现在都只能表达成 global instruction。
第一版一律按项目导入，`scope` 建议只记进出处；是否允许 global 作用域的导入事实列为待定问题，放开之前要先确认检索对
global fact 的可见性，以及 consolidation 只按项目选候选的规则。

### 6.16 回滚

- `import rollback RUN` 按写入的逆序处理这次导入写入的记录，只做逻辑操作：导入的记录变为 `superseded` 并标记
  `importRollback`，被它 supersede 的旧版本恢复为 `current`，台账行标为 `rolled_back`。多个单元指向同一条记录时，
  只有这些单元全部属于这次导入才撤回它。
- 之后被人更正、删除或被 consolidation 合并过的记录不动，记下 `rollbackSkipReason`，规则和 consolidation 的回滚一样
  （[应用与回滚](../architecture.md#apply-and-rollback)）。
- 这次导入写入的 instruction 一并撤回；被人更正过的同样不动。
- 会话轮次：台账记下每一轮的 requestId，以及 daemon 是否返回 `duplicate:true`。回滚只处理这次导入新建的请求，只撤回
  来源请求全部属于这次导入的事实；还有其他来源的事实保留。还在 outbox 里没上传的轮次直接取消。
- 撤回事实要知道它由哪些请求形成，这个出处在算法的私有表里（Mem0 的 `mem0_sources`）。引擎通过通用的
  `MemoryAlgorithm::source_requests` 询问，算法不需要知道导入。导入不写算法的 history，回滚了什么记在提议行的 `rollback` 里。

### 6.17 导入之后

导入完成后，建议对这个项目跑一次 consolidation dry-run（需要 `consolidationEnabled:true`），合并 Claude 和 Codex
记了同一件事的情况。`imported` 记录是候选，人确认后写入的 instruction 不是。

### 6.18 安装器

- `scripts/install.sh` 增加 `--import [notes|sessions|all]`，默认不导入。
- 交互运行时，安装结束后执行 `agent-memoryd import plan` 并显示汇总。汇总里有生成调用时，先问是否 propose；propose 之后
  显示提议汇总，再问是否 apply。非交互运行只有显式传了 `--import` 才导入，而且只到 propose 为止，apply 要单独执行。
- 导入需要 daemon 已经运行，安装器在服务启动之后再做。`--no-service` 时只生成计划，并打印后续命令。
- `install-check` 用合成的宿主目录覆盖 plan、propose 和 apply，抽取使用本地确定性的生成桩，不访问模型。

## 7. 代码位置

按 [AGENTS.md](../../AGENTS.md) 的分层：

| 位置 | 内容 |
|---|---|
| `memory-rules` | 单元大小规则、确定性切分、脱敏规则、单元键和内容哈希、来源筛选规则、抽取结果的校验（复用 consolidation 的专有名词和数字检查），都是纯函数 |
| `memory-domain` | `ORIGIN_IMPORTED`、导入单元、import run 和提议的类型、合并 origin 规则的修改、写入和回滚的记录计划（仿照 [consolidation apply](../../crates/memory-domain/src/consolidation/apply.rs)） |
| `memory-store` | `import_runs`、`import_ledger` 和抽取缓存 `import_extractions` 表；提议复用共享的 `proposals` 表（`kind: "import"`） |
| `memory-engine` | `app/import/`：`propose.rs`、`apply.rs`、`rollback.rs` 三个 admin 用例和会话轮次的 `turns.rs`；`extract.rs` 和 `extract.txt` 是抽取的调用和提示词；`mod.rs` 放共用的台账和 run 读取 |
| `memory-algo-mem0` | 没有导入代码。导入只用两个通用钩子：`nearest_facts`（形成用的向量召回）和 `source_requests`（`mem0_sources` 里的来源请求）；`add_memories` 把 `importRunId` 和请求一起提交。实体投影只接受 `ingestionMode` 为 `inferred` 的记录，导入的记录因此不进投影 |
| `memory-service` | `/admin/import_*` 的分发；`harness/import/` 下的宿主适配器、评审文件处理和 `agent-memoryd import` 命令（捕获的适配器也在 `harness/`） |
| `examples/` | 宿主 Agent 的导出提示词（中文、英文） |
| `scripts/install/` | 安装器的 `--import` |

新代码沿用现有的依赖方向，不需要放宽 [check_architecture.py](../../scripts/check_architecture.py) 的规则。检查脚本还规定算法 crate
不能引用导入的模块（`import::`），导入的代码不会再进到算法里。

## 8. 验证

- 适配器用合成的宿主目录做单元测试：Claude 的 memory 目录和 transcript，Codex 的 memories 目录和 rollout，WorkBuddy 的
  记忆文件。覆盖 6.3 的每条筛选规则、worktree 归并、6.5 的单元划分和 `--no-extract` 切分、脱敏。fixture 只放编造的内容，
  不复制真实会话，也不提交生成的 trace。
- 抽取校验是纯函数测试：依据片段不是子串、出现原文没有的专有名词或数字、超过硬上限、不合法的 `kind`，以及同一响应里部分
  条目被拒绝。
- 引擎层用临时存储、`MEMORY_MOCK_MODELS=1` 和本地确定性的生成桩测试：propose 不写记忆，评审文件的接受和拒绝，apply 的
  复查和跳过，重跑不写入，原样单元的 supersede，抽取单元的 `source-changed`，回滚以及回滚跳过的情况，抽取缓存命中时不调模型。
- 导出 JSONL 的解析测试：`{"continued": true}` 行、不合法的行、缺少日期的条目。
- `selftest` 增加一段：导入两条原样单元后，`search_memories` 能找到它们，`get_context` 不把它们放进已确立的事实，按召回的
  参数（topK 5、预算 400）搜索时它们能被装进结果。
- 会话导入复用捕获的 outbox 测试：同一轮先捕获再导入，daemon 返回 `duplicate:true`。
- 不调用付费模型，不跑完整数据集。

## 9. 分阶段

1. 已完成：`imported` 来源（包括合并 origin 规则的修改）、单元大小规则、`plan`、`propose`、`show`、`apply`、`status`，
   原样采用和 `--no-extract` 切分，Claude 和 Codex 的笔记适配器。
2. 已完成：抽取（含校验和缓存）、指令候选的确认、回滚、宿主 Agent 导出和通用 JSONL 输入、WorkBuddy 适配器、导入后的
   consolidation 提示（写在文档里）。未完成：安装器的 `--import`。
3. 已完成：Claude 和 Codex 的会话导入，带限速和续传。
4. DeepSeek Harness 的会话导入。它的日志是压缩的私有格式（`session.v4`），但它没有自动捕获，会话导入是它唯一的自动来源。

## 10. 待定问题

1. 已解决：Claude Code hook 的 `prompt_id` 和 transcript 的 `promptId` 是同一个值（2026-10-22 用 Claude Code 2.1.305 核对：
   给临时项目装上捕获，跑一轮，outbox 里的 native id 等于 transcript 的 `promptId`）。
2. Codex 的 memories 和 rollout、Claude Code 的 transcript 都没有官方格式说明。适配器按版本字段（Codex 的
   `cli_version`、Claude 的 `version`）和文件结构识别，遇到不认识的结构就整体跳过并报告。要支持哪些版本范围还没定。
3. Codex `MEMORY.md` 里跨线程的总结（如 "Reusable knowledge"、"Failures and how to do differently"）要不要导入，导入的话
   怎样确定项目。
4. 导入的记录要不要进实体投影。进的话多一次实体嵌入，检索和 consolidation 聚类会更好。
5. 是否允许 global 作用域的导入事实（WorkBuddy 的跨项目记忆、Claude 的 `type: user` 笔记、`scope: global` 的建议）。
6. Codex 的 `chatgpt_handoff` 线程算不算用户会话。
7. 宿主删掉源文件（例如 Codex 遗忘了一条记忆）时，是否应该同步撤回，以及 `--prune` 的默认值。
8. 反方向导出：把共享记忆写成 `AGENTS.md` 或 `CLAUDE.md` 片段，给接不了 MCP 的宿主使用。Letta 已经支持把记忆导出成
   这两种文件。
9. 单元上限取召回预算的一半、目标取 `searchBudget / topK` 是否合适；用户把召回预算调大以后，已经导入的单元要不要重新切分。
10. 评审时是否允许改写文本。现在只能接受或拒绝，改写要在 apply 之后用 `correct_memory`。
11. 导出提示词在各宿主上的遵从度：条目很多时会不会被截断或归纳，`{"continued": true}` 的分页是否可靠，需要在每个宿主上
    实际试一次。
12. 抽取提示词是否需要和形成共用 `mem0CustomInstructions`。

## 11. 参考

- [claude-mem PR #2829：auto memory 导入](https://github.com/thedotmack/claude-mem/pull/2829)
- [claude-mem PR #4201：Claude Code transcript 回放](https://github.com/thedotmack/claude-mem/pull/4201)
- [openai/codex#55060：Codex 桌面版导入 claude-mem 内部会话](https://github.com/openai/codex/issues/55060)
- [Hermes：从 OpenClaw 迁移](https://hermes-agent.nousresearch.com/docs/user-guide/skills/optional/migration/migration-openclaw-migration)
- [Letta：Context Repositories](https://www.letta.com/blog/context-repositories/)、[Agent File](https://docs.letta.com/guides/agents/agent-file)
- [Mem0：timestamp](https://docs.mem0.ai/platform/features/timestamp)、[add](https://docs.mem0.ai/core-concepts/memory-operations/add)
- [Zep：Adding Business Data](https://help.getzep.com/adding-business-data)、[Graphiti：Adding Episodes](https://help.getzep.com/graphiti/graphiti/adding-episodes)
- [Claude Code：How Claude remembers your project](https://code.claude.com/docs/en/memory)
- [Cursor 论坛：Are my memories gone?](https://forum.cursor.com/t/are-my-memories-gone/144057)
- [OpenMemory：Migrate from Mem0](https://openmemory.cavira.app/docs/migration/mem0)
- [basic-memory：import commands](https://www.mintlify.com/basicmachines-co/basic-memory/api/cli/import-commands)
- [PCWorld：Claude 导入其他 AI 的记忆](https://pcworld.com/article/3076376/claude-can-now-import-chat-histories-from-chatgpt-and-other-ais.html)、[Gemini 导入 ChatGPT 和 Claude 的聊天记录](https://www.pcworld.com/article/3100804/google-gemini-can-now-import-your-chatgpt-or-claude-chat-history.html)
- [Codex CLI memory internals（第三方分析）](https://codex.danielvaughan.com/2026/04/08/codex-cli-memory-internals/)
