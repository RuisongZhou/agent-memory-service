# 宿主已有记忆的导入

> 状态：笔记导入已实现，覆盖 Claude Code、Codex、WorkBuddy 和宿主 Agent 导出的 JSONL；Claude Code 和 Codex 的会话导入
> （6.10、6.14、6.16）也已实现。尚未实现：规则文件的指令候选（`--include rules`，6.2）、安装器的 `--import`（6.18）和
> DeepSeek Harness 的会话导入（它的日志是压缩的私有格式 `session.v4`，但它没有自动捕获，会话导入是它唯一的自动来源）。
> 用法见 [MCP 与宿主集成](../mcp-harness.md#import-existing-memories)，端点见 [API 与配置](../api.md#import-runs)，daemon 一侧的
> 处理见 [架构](../architecture.md#import)。本文只记设计取舍，细节以这些文档和代码为准。

日期：2026-10-22。范围：Codex、Claude Code、WorkBuddy 和 DeepSeek Harness 在接入本服务之前已经积累的记忆文件和
会话记录。第 4 节的格式来自一台 macOS 开发机：Claude Code 2.1.293–2.1.306 的 transcript 和 auto memory，
Codex CLI/App 0.153–0.163 的 rollout 和 memories 目录，以及 WorkBuddy、DeepSeek Harness 的数据目录。只核对了目录、
文件名、字段结构和文本长度，没有读取记忆正文。

## 0. 已知限制

- 单元键里重复标题的 `~2`、`~3` 和超长节的 `~part1`、`~part2` 都按在文件里的位置编号。在前面插入同名标题、或一节长过
  8 KB 时，单元键会变。因为相同文本会链接、别的单元持有的记录不会被取代（6.9），这不会丢内容，但可能留下一条过时的
  current 记录，要靠 consolidation 或人撤回。
- 宿主删掉源文件或源单元（例如 Codex 遗忘了一条记忆）时不做任何处理，旧单元写入的记录保持 current（第 9 节第 6 条）。
- 抽取不会在有待处理的形成任务时让出，和形成按到达顺序共用生成并发（6.14）。
- Codex memories 目录的 git commit 只记在出处里（`memoriesHead`），不用来跳过文件，是否变化只看单元哈希。
- Codex 的 `chatgpt_handoff` 等非 `user` 线程一律跳过（第 9 节第 5 条）。

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

- 不替代宿主自己的记忆和规则文件，也不往宿主目录写东西。反方向的导出见第 9 节。
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
| Codex | `~/.codex/memories/rollout_summaries/*.md` | 每个线程一篇总结：一级标题、开头的 `thread_id`、`updated_at`、`rollout_path`、`cwd`，然后是若干 `## Task N` 节 | 不公开，只有[第三方分析](https://codex.danielvaughan.com/2026/04/08/codex-cli-memory-internals/) | `updated_at` | `cwd` | `thread_id` |
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
- outbox 按 `seq` 顺序上传，daemon 只有一个有序的形成 worker（[写入路径](../architecture.md#write-path)）。
- Mem0 形成写入的 fact 固定是项目作用域，只有 instruction 能写成 global 或 platform 作用域。
- consolidation 合并时，结果的 origin 取输入中最弱的一个：有 assistant 就是 assistant，否则有 imported 就是 imported，
  否则有 inferred 就是 inferred，否则是 user（[domain consolidation apply](../../crates/memory-domain/src/consolidation/apply.rs)）。
  `imported` 是为导入加的（6.8）。
- 自动召回每次提问向 daemon 要 `max(15, 3 × topK)` 条结果，预算是条数乘 `--search-budget`（默认 15 条、6000），再在本地按
  topK 5、预算 400 装，每条按 `est_tokens(text) + 8` 计（[recall.rs](../../crates/memory-service/src/harness/recall.rs)）。
  第一条装不下的记录截成片段注入并结束这次注入，剩下的预算放不下 32 个 token 的片段时跳过它。所以接近 400 的记录会挤掉
  同一次提问的其他结果，更长的只剩开头一段。daemon 自己按同样的成本装完整记录，装不下的整条跳过
  （[retrieval.rs](../../crates/memory-algo-mem0/src/retrieval.rs)）；Agent 主动调 MCP 的 `search_memories` 时，默认预算是 2000
  （`defaultTokenBudget`）。

## 6. 设计

### 6.1 流程

笔记和宿主 Agent 的导出走 plan（离线、只读，筛选、映射项目、切出导入单元，写成人可以删改的计划文件）→ propose（daemon
对小单元原样采用，其余调模型抽取，存成提议，不写记忆）→ 审核（指令候选逐条确认）→ apply（写入 `imported` 记录和导入台账），
rollback 可以整体撤销。历史会话在 propose 时只登记、不调模型，apply 把它们写进 outbox，走正常的 Mem0 形成，turnId 和捕获的
相同。每一步的命令和输出见 [导入已有记忆](../mcp-harness.md#import-existing-memories)。

### 6.2 三类来源

| 来源 | 例子 | 默认 | 写入方式 |
|---|---|---|---|
| 人写的规则文件 | `CLAUDE.md`、`AGENTS.md`、`.claude/rules/`、`.cursor/rules` | 不导入 | 宿主自己会加载，导入后会重复注入。仓库里的文件不可信，自动变成 instruction 就等于埋下一条对所有宿主生效的持久提示注入。设想中的 `--include rules`（未实现）按标题切成最多 1024 个字符的指令候选，由人逐条确认后写成 instruction |
| 宿主提炼的笔记 | Claude `memory/*.md`、Codex `rollout_summaries/*.md`、WorkBuddy `MEMORY.md`、`USER.md` 和 `memory/*_memory.md` | 导入 | 新的 `imported` 来源（6.8）。按 6.5 的规则原样采用或抽取（6.6），都要经过 propose 和审核（6.7） |
| 原始会话 | Claude `*.jsonl`、Codex rollout | `--include sessions` 时才导入 | 走正常的 Mem0 形成，使用原始时间戳和捕获时的 turnId（6.10） |

每个宿主只取一种单元，同一份知识不导入多次：

- Codex 取 `rollout_summaries`，跳过 `raw_memories.md` 和 `memory_summary.md`。`MEMORY.md` 里跨线程的总结没有对应的
  单篇来源，是否导入列为待定问题。`skills/` 是程序性知识，当前 main 不形成 skill，不导入。
- Claude 跳过 `MEMORY.md` 索引。`type: feedback` 的笔记照常导入，同时把它的第一段（有 `description` 时就是它）列为
  指令候选，确认后另写一条 instruction。所有指令候选都按 instruction 的 1024 字符上限检查。
- WorkBuddy 跳过 `SOUL.md`、`IDENTITY.md`、`BOOTSTRAP.md`。

### 6.3 按来源筛选

只导入人提出的轮次，判断靠宿主自己的来源字段：Claude Code 的 `turnOrigin`（或 `origin.kind`）为 `human`，Codex 的
`thread_source` 为 `user`，而不是目录或标题（openai/codex#55060 就是按目录导入，把 claude-mem 的内部会话当成了用户对话）。
两个宿主各自导入和跳过什么见 [导入历史会话](../mcp-harness.md#import-past-sessions)。

两个宿主的会话格式都不公开。缺少筛选所依赖的来源字段的会话一律不导入，标为 `unknown-format`，不按猜测处理。所有宿主都
去掉本服务自己的 `<shared-memory>` 召回块（否则召回内容会被再次形成记忆）和 Claude Code 注入的 `<system-reminder>` 块，
不跟随软链接，不读宿主数据目录之外的路径。选中时间窗口内每个被跳过的轮次都在计划里写明原因，方便复核。

### 6.4 项目映射

- 一次导入对应一个 `--project` 和一个或多个 `--workspace` 目录。所有 cwd 都先归到 git 根（worktree 归到主仓库），再和
  `--workspace` 比较。
- Claude 的 auto memory 目录按 git 仓库共享，transcript 按 worktree 和子目录分目录。默认按 Claude Code 的规则把 workspace
  的仓库根编码成目录名（字母和数字以外的字符都换成 `-`）。不从目录名反推路径：`/` 和 `-` 都编码成 `-`，反推有歧义。
  `--all` 时逐个查看带笔记的目录，用同目录第一个 transcript 前 16 KiB 里的 `cwd` 确定项目，和 claude-mem 的做法一样（没有
  transcript 时只认 workspace 自己的目录）；
  会话导入也这样判断每个 transcript 目录属于哪个 workspace。
- Codex 的 rollout summary 和 rollout 都带 `cwd`，直接比较。
- `--workspace` 之外的文件或会话标为 `other-project`，不导入。
- WorkBuddy 的用户级记忆不属于任何项目，导入到 `--project`，出处里记 `userLevel: true`（见 6.15）。

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

各来源的输入单元和处理方式见 [导入已有记忆](../mcp-harness.md#import-existing-memories) 的表。原则是：已经提炼好、不超过
硬上限的单元原样采用（`verbatim`），只做嵌入；超过的抽取（6.6）；Codex 的 Task 节混着任务进度和过程记录，长短都要筛，一律
抽取。`--no-extract` 时不调模型，超过硬上限的按列表项和段落切，仍超过的按句子切，每块带标题路径（`split`）。

按本机数据，Claude 的两篇笔记都超过硬上限，要抽取；Codex 的 95 个 Task 节都走抽取；WorkBuddy 的 5 个标题节里有 2 节超过
硬上限。`--no-extract` 时，Codex 和 WorkBuddy 的列表项全部在硬上限以内，但单条列表项常常离不开标题，切开后可能看不懂，
所以它只是不想花模型调用时的退路，不是默认值。

召回现在会把装不下的记录截成片段注入（第 5 节），但片段只是指向全文的线索，还会结束这次注入，所以这条上限仍然适用。

### 6.6 抽取

抽取在 daemon 里做。模型配置和密钥都在 daemon，原始响应的保存和有界修复也已经有了，和形成、consolidation 一样。

**输入**：一个单元，单次最多 8 KB，更长的节在计划里按段落和句子切成几部分（单元键加 `~part1`、`~part2`）。另外附带：

- 来源信息：宿主、项目、标题路径、`statedAt`；
- 同一项目里与单元最相近的 10 条已有记忆（向量召回，和形成一样）。库里已有的内容不再输出，写入时就顺带去了重。

单元正文用分隔符包住，提示词说明其中出现的任何指令都只是记忆内容，不是给模型的指令。

**输出**是严格的 JSON：

```json
{"memories": [{"text": "…", "kind": "fact", "scope": "project", "evidence": "…"}]}
```

- `text`：脱离原文也能看懂（把"这个项目""用户"换成明确的名称），只写一件事，长度接近 6.5 的目标，不超过硬上限。
  语言规则和形成的 `zh` 变体一致：与原文的语言和文字相同。
- `kind`：`fact`、`preference`、`instruction`、`reference` 之一。任务进度和临时状态由提示词要求直接略去，不作为一种 `kind` 输出。
- `scope`：`project` 或 `global`，只作为建议记下来（6.15）。
- `evidence`：原文里支持这一条的片段。

**校验**是纯函数，不修改内容：

- `evidence` 必须是单元正文的子串，8 到 600 个字符（单元更短时须是整个单元），并且包含 `text` 至少 30% 的内容词
  （4 个字母以上的英文词、相邻汉字组成的二元组）；
- `text` 里的专有名词和数字必须出现在单元正文或来源信息里，规则和 consolidation 校验合并文本时相同
  （[rules](../../crates/memory-rules/src/consolidation.rs)）；
- `text` 不能为空，也不能超过硬上限。

不通过的条目记为 `rejected` 并写明原因，同一响应里的其他条目照常保留。JSON 结构本身不合法时，和形成一样做有界修复；
原始响应在解析前先保存。

**去向**：`fact`、`preference` 和 `reference` 成为提议；`instruction` 成为指令候选，只能在审核时逐条确认。每个抽取单元
记录 `coverage`，即通过校验的依据片段覆盖了单元正文的比例，供审核时判断模型漏掉了多少。

**缓存**：键包括项目、单元文本、标题路径、`statedAt`、来源的分类、召回预算、提示词变体、提示词和模型，不包括每次都会变的
已有记忆。同一内容再次 propose 不会重新调用模型，内容变了才重新抽取；所有条目都没通过校验的回复不缓存，下次重新抽取。

**输出长度**：上限是原文估算长度的两倍，至少 2000、最多 8192 个 token，不跟随形成的 `mem0MaxTokens`：每一条都要引用原文作为
依据，回复被截断就要花修复调用。

**提示词**是新写的，不复用 Mem0 面向对话的 additive 模板。它和导入的其他代码一起放在引擎里
（[extract.txt](../../crates/memory-engine/src/app/import/extract.txt)），不属于任何算法：输出由纯规则校验，由引擎写成普通记录。
算法只提供已有记忆的召回（`MemoryAlgorithm::nearest_facts`，和形成召回已有事实的方式相同）。

**不用自由的 Agent**：模型不读文件、不调工具，一次只看一个单元。这样每条结果都有明确的出处，能逐条校验和撤销，也不会
被笔记里嵌入的指令带偏。

**成本**：按本机数据，Codex 的 95 个 Task 节各一次，加上 Claude 的 2 次，共约 97 次生成调用。Codex `MEMORY.md` 是否
导入还没定（第 9 节），导入的话按 287 个标题节计算，最多再多 287 次。计划的汇总会先列出这些数字。

### 6.7 propose、审核与 apply

和 consolidation 一样分成提议和应用两步，提议阶段不写任何记忆。命令、评审文件的字段和默认决定见
[导入已有记忆](../mcp-harness.md#import-existing-memories)，这里只记取舍：

- propose 按单元数（默认 8，每次请求最多 64）和请求体大小（1 MiB）分批，组成一个 run。中断后不续传，重新运行就是新的 run。
  单元失败（例如模型调用出错）不会让整个 run 失败：run 照常进入 `proposed`，失败的单元没有提议，下次在新的 run 里重新提出。
- 评审文件（`import show` 写出）每条提议一行。事实提议默认 `accept`；指令候选默认 `reject`，要逐条改成 `accept`，因为 instruction 会被每个宿主
  当作行为要求加载。评审时不能改写文本，需要改的话，apply 之后用 `correct_memory`。
- apply 必须带 `--review` 或 `--yes`，不会因为漏了评审文件就按默认决定写入。apply 前 CLI 重新读取来源文件，propose 之后
  改过的文件的提议一律按拒绝处理。daemon 再逐条复查：propose 之后另一个 run 导入或回滚过同一个单元时标为 `skipped` 并写明
  原因；同一项目里已有内容相同的 current 记录时，链接到它而不是再写一条（6.9）。
- 命令本身不做交互确认，propose 之后只有显式执行 apply 才会写入；交互式的确认留给安装器（6.18）。

run 的状态见 [架构](../architecture.md#import)，apply 或 rollback 中断后对同一个 run 重新执行会接着做。提议的状态：
`proposed`、`rejected`（校验未通过）、`dropped`（评审拒绝）、`applied`、`skipped`、`rolled_back`。

### 6.8 `imported` 来源

新增 origin `imported`：

- 不加入 `context_origins`。`get_context` 把它当作历史参考，和 assistant 事实一样，不当作已确立的事实。
- 是 consolidation 的候选，可以和实时形成的事实、其他宿主导入的记录合并。合并规则把它算作"弱"来源（第 5 节）：合并结果
  只要有一个输入是 `imported`，就不会是 `user`。不这样的话，导入的记录和用户事实合并后会变成 `user`。
- `source.platform` 是来源宿主的平台标签（`claude-code`、`codex`、`workbuddy`），和捕获时一致。
- `statedAt` 取来源时间：Claude frontmatter 的 `modified`，缺失时用 mtime；Codex 的 `updated_at`；宿主 Agent 导出的
  `date`。导出条目没有日期时留空，免得它在 consolidation 里被当成最新的说法。`effectiveAt` 留空，和 Mem0 事实一样；
  `recordedAt` 是写入时刻。人确认的指令写成 explicit instruction，和其他 instruction 一样从写入时刻生效，来源时间留在
  `detail.import.statedAt`。
- `ingestionMode` 记为 `imported`。实体投影只接受能证明是 `inferred` 的记录（[entities.rs](../../crates/memory-algo-mem0/src/entities.rs)），
  所以导入的记录不进实体投影：检索靠语义和 BM25，consolidation 只能用向量把它们聚类。是否放开列为待定问题。
- `detail.import` 记录出处：`runId`、`proposalId`、`unit`、`host`、`mode`（`verbatim`、`split` 或 `extract`）、`itemKind`、
  `scopeHint`、`evidence`、`headingPath`、`unitHash`、`fileHash`、抽取的原始响应 `responseLogId`，以及适配器给的 `source`
  （路径、原生 ID、`via`，导出文件还有行号，见 6.11）。另写一条 `role: import` 的证据行，记下依据片段。字段以
  [`provenance`](../../crates/memory-domain/src/import/apply.rs) 为准。

### 6.9 导入台账、幂等和更新

台账在 daemon 的存储里，每个项目的每个单元一行：`(project, unit) → {unitHash, fileHash, runId, memoryIds, status, previous}`。
单元键由宿主、原生 ID（没有时用相对宿主目录的路径）和单元在文件里的位置组成。

- 是否变化只看单元哈希，在 propose 时由 daemon 和台账比较；`plan` 不连接 daemon，所以计划里不判断。哈希相同的单元是
  `unchanged`，不再提议。
- `memoryIds` 是单元的"活链接"：它当前持有的记录（写入的，或链接到的同文本记录），从一次导入带到下一次，直到被这个单元
  的新提议取代或不再是 current。apply 之后这次提议过的每个单元都有一条台账行；提议全被拒绝时状态记为 `declined`，保留
  原来的链接，同样的文本不会再被提出来，`propose --reconsider` 可以重新提出。
- 和同一项目里已有的 current 导入记录内容相同（文本空白归一后比较）的提议默认接受，接受即把单元链接到那条记录，不再写
  新的，和 Mem0 的精确去重一样。这样即使另一个单元以后改了自己的文本，这个单元的内容也还在。
- 单元变了，取代按角色进行：原样采用的单元只对应一条记录，它的新事实取代它以前的那条事实；新指令取代它以前人确认过的那条
  指令。旧记录变为 `superseded`，`validUntil` 取新记录的 `statedAt`（没有时取写入时刻），新记录的 `supersedes` 指向旧记录。
  不调 `correct_memory`。
- 抽取和切分的单元会产生多条记录，旧文本产生的事实不自动撤回：评审文件用 `previous` 行列出它们（`source-changed`），由人
  决定保留还是用 `correct_memory` 撤回；新旧说法冲突的情况交给 consolidation 按 `statedAt` 处理。
- 另一个单元还持有的记录不会被取代，也不会被回滚；已经不是 current（被合并、更正或删除）的记录也不碰。

### 6.10 会话导入复用捕获的身份

- 每一轮写成一个 `messages` 批次（用户提问加最终回复），形状和捕获写进 outbox 的记录一样。
- turnId 用 `turn_key(project, session, native_id)`。Claude 的 native_id 取 `promptId`，session 取 `sessionId`；Codex 的
  native_id 取 `turn_id`，session 取 `session_meta.id`。两者都核对过，和 hook 拿到的值相同（第 4 节）。
- 平台标签用宿主自己的（`claude-code`、`codex`），不用 `import`。daemon 按 `platform + turnId` 去重，所以已经实时捕获过的
  轮次不会再形成一次，重复运行导入也一样。用错标签就会重复形成。分叉或恢复的会话会重复前面的轮次，同一轮出现在两个会话里
  时归先开始的会话，和捕获时一致。
- 时间戳用原始时间：提问取用户记录的时间，回复取该轮最后一条助手文本的时间（Codex 用 `task_complete`）。每个会话内按时间
  顺序提交：Mem0 形成的历史上下文取同一 platform/project/session 里前面已经形成的请求，顺序对了，指代解析才能看到正确的前文。
- Claude 的一轮从一条带 `turnOrigin` 的 user 记录开始，到下一条带 `turnOrigin` 的 user 记录（不论来源）之前结束，而不是到
  下一条 `human` 记录之前：否则 `sdk` 或 `task_notification` 轮次的回复会被算到前面那个人工提问上。回复取这一轮最后一条带
  文本的 assistant 消息（同一 `message.id` 的文本块合并），忽略 thinking 和工具调用。Codex 的一轮按 `turn_id` 归组，和捕获
  一样不导入被中止或没有最终回复的轮次。
- 会话轮次不检查文件哈希：消息随计划一起走，而进行中的会话的 transcript 一直在增长。
- propose 时只把轮次登记成 run 的行（共享 `proposals` 表里 `itemKind: session-turn` 的行），不调模型，也没有提议需要审核；
  要导入哪些会话在计划阶段决定。轮次没有台账行，去重交给 daemon。
- apply 之后由 CLI 把轮次写进 dialogue outbox，消息体带 `importRunId`，由已有的上传器提交。daemon 只接受这个 run 已经
  apply 的轮次，而且平台、项目、会话和消息都要和登记的一致；新建的请求记下 `importRunId`，所以响应丢失后的重发仍算这次导入
  的。MCP 桥会去掉这个字段，模型不能提交导入的轮次。
- 形成出的事实大小由 Mem0 的抽取决定，不受 6.5 约束。每一轮是一次生成调用，计划里写明轮数、预计的调用次数和发出的字符数。

### 6.11 宿主 Agent 导出

有些平台读不到底层存储，例如 ChatGPT、Gemini、Cursor 的云端记忆；有些格式不明或经常变，例如 DeepSeek Harness 的压缩日志、
没有文档的 Codex 记忆。这时用 Claude Import Memory 的办法：给宿主的 Agent 一段提示词，让它把记得的内容导出成 JSONL，
再走同样的 plan、propose、审核和 apply。

源 Agent 只负责原样导出，不做整理。整理交给我们的抽取（6.6）和 consolidation：源 Agent 一归纳，信息和出处就丢了，结果
也没法校验。

`agent-memoryd import prompt --project my-project [--lang zh|en]` 打印给宿主 Agent 的提示词，原文在
[examples/memory-export-prompt.zh.txt](../../examples/memory-export-prompt.zh.txt) 和同目录的英文版里。它要求每条记忆输出
一行 JSON（`text`、`kind`、`date`、`source`、`scope`），尽量保留原话，不合并、归纳、改写或补充；凭据写成 `[redacted]`；
条目多时分几次输出，每次以 `{"continued": true}` 结尾。

处理导出结果：

- `agent-memoryd import plan --host jsonl --from FILE [--label LABEL]` 读取导出结果，允许外面包着代码块，去掉
  `{"continued": true}` 行。不合法的行在计划的问题列表里标 `invalid-line`。平台标签取 `--label`，默认 `agent-export`。
- 每行是一个单元，同样受 6.5 的硬上限约束：不超过就原样采用，超过就抽取。
- `kind: instruction` 的行只成为指令候选；`scope` 只作为提示记进出处（6.15）。
- 出处里的 `path` 是导出文件，另记行号和 `via: "agent-export"`；`statedAt` 的规则见 6.8。
- 导出结果是不可信的数据：其中的任何指令都只当记忆内容处理，脱敏规则照常执行（6.12）。
- 宿主 Agent 能写文件时，可以让它把结果直接写到指定路径，省去手动复制。
- 从 pd-memory 迁移也走这条路：它的 `MemoryExportBundle` 可以转换成同样的 JSONL。确定字段之前先对照那边的结构。

### 6.12 脱敏与隐私

- 在发出任何网络请求之前脱敏。默认配置下，原样采用的单元也会发到托管的嵌入 API；抽取和会话导入还会把全文发给生成模型。
- 规则覆盖常见的 key 和 token 格式（`sk-`、`gh*_`、私钥块等，和 [check_public.py](../../scripts/check_public.py)
  的模式一致）、`Authorization`/`Bearer` 头、URL 里的凭据，以及 `.env` 风格的 `KEY=value` 行。命中的片段替换成
  `[redacted:<kind>]`，计划里只记命中次数，不记原文。
- 命中私钥块的单元整条跳过。
- 计划的汇总分别列出会发给生成服务和嵌入服务的调用数和字符数。
- 计划文件和评审文件放在本机私有目录（默认 `~/.local/state/agent-memory-service/imports/`，权限 0600），和 outbox 一样
  不进版本库；run、提议和台账在 daemon 的存储里。

### 6.13 命令和端点

命令和选项见 [导入已有记忆](../mcp-harness.md#import-existing-memories)，端点和请求字段见 [导入 run](../api.md#import-runs)。
设计上的取舍：

- `plan` 只读宿主文件，不访问网络，也不需要 daemon，所以可以先看清要发出什么再决定。计划是 JSONL 文件，第一行是汇总，
  之后每行一个单元，人可以删行或把 `decision` 改成 `skip`。
- `propose` 先重新计算文件哈希，源文件在 plan 之后改过的单元标为 `stale-plan` 并跳过。
- 端点和 consolidation 的提议、应用、回滚三个端点对应，另有读 run 的 `import_run` 和 `import_runs`，都不放进 MCP。apply
  每条提议一个事务：记录、证据行、台账行和 `upsert` 索引任务，提议行本身记下写入、取代了什么；嵌入交给索引器，和
  consolidation 合并结果的写法一样（[app/consolidation.rs](../../crates/memory-engine/src/app/consolidation.rs)）。会话轮次只
  标为已排队，由 CLI 写进 outbox（6.10）。
- 接受的指令候选由 apply 直接写成 explicit instruction，不经过 `add_memories`。同一项目里已有相同文本、相同作用域的导入
  指令时只链接到它，所以重跑不会重复写。

### 6.14 吞吐和实时捕获

outbox 按 `seq` 先进先出，daemon 只有一个有序的形成 worker。几千轮历史会话一次进队，之后的实时轮次就要排在后面。
另开一个 outbox 文件也绕不开 worker 的串行，形成环节没有优先级可用。因此会话导入要：

- 每次计划最多取 `--limit` 轮（默认 200），取 `--since`（默认不限）之后最早的那些，报告里给出下一批的 `--since`，需要更多时
  分批运行；
- apply 用 `--rate` 限速（每分钟最多写入的轮数，默认 20），outbox 里还没形成完的作业（包括实时捕获的）达到 `--max-pending`
  （默认 4）时等待。待处理数取自 outbox 而不是 `/admin/stats`：后者要统计每张表，而 outbox 正好反映上传器和形成的进度；
- 可以中断：同一个 outbox 里已经有这一轮的作业（实时捕获的，或更早的 run 的）时不替换，对已 apply 的 run 再执行 `apply`
  只补写还没写进去的轮次。

抽取不经过形成 worker，但和形成共用 daemon 的生成并发，`service install` 默认只有 1 个，而抽取不会让出（第 0 节）。大批导入时
可以把计划分几次 propose。原样采用的单元只占用索引器和嵌入调用。

### 6.15 作用域限制

Mem0 形成写入的 fact 固定是项目作用域。WorkBuddy 的 `MEMORY.md` 里有专门写给跨项目复用的内容，Claude 的
`type: user` 笔记也和具体项目无关，抽取和导出结果里还会有 `scope: global` 的建议，但现在都只能表达成 global instruction。
因此事实一律按项目导入，`scope` 建议只记进出处（`scopeHint`）。指令默认也只对项目生效，模型给的 `scope: global` 只作提示，
人在评审文件里把它的 `scope` 改成 `global` 才会全局生效。是否允许 global 作用域的导入事实列为待定问题，放开之前要先确认
检索对 global fact 的可见性，以及 consolidation 只按项目选候选的规则。

### 6.16 回滚

- `import rollback RUN` 按写入的逆序撤销这次导入的提议，只做逻辑操作：写入的记录变为 `superseded` 并标记
  `importRollback`，被它取代的旧版本恢复为 `current`；链接到同文本记录的只解除链接，那条记录不动，链接取代的旧版本同样恢复。一个单元的提议全部撤销后，它的台账行恢复成
  这次导入之前的那一行（原来没有就删掉）。另一个单元还持有的记录不撤回。
- 之后被人更正、删除、被 consolidation 合并过的记录不动，记下 `rollbackSkipReason`，规则和 consolidation 的回滚一样
  （[应用与回滚](../architecture.md#apply-and-rollback)）；之后又被别的 run 导入过的单元要先回滚那个 run。这次导入写入的
  instruction 按同样的规则撤回。
- 会话轮次：CLI 先把 outbox 里这个 run 还没上传的作业标为 `cancelled`，daemon 再关闭这个 run，之后带它的 `importRunId` 的
  提交一律拒绝。run 的行记下每一轮的 `requestId` 和 daemon 是否返回 `duplicate:true`；回滚只撤回来源请求全部属于这次导入的
  事实，还有捕获或其他 run 的来源的事实保留。还在形成中的轮次留到下一次 rollback（`partially_rolled_back`）。原始输入保留，
  turnId 也仍被占用，所以回滚过的轮次再导入只会被当作重复，不会再形成一次。
- 撤回事实要知道它由哪些请求形成，这个出处在算法的私有表里（Mem0 的 `mem0_sources`）。引擎通过通用的
  `MemoryAlgorithm::source_requests` 询问，算法不需要知道导入。导入不写算法的 history，回滚了什么记在提议行的 `rollback` 里。

### 6.17 导入之后

导入完成后，建议对这个项目跑一次 consolidation dry-run（需要 `consolidationEnabled:true`），合并 Claude 和 Codex
记了同一件事的情况。`imported` 记录是候选，人确认后写入的 instruction 不是。

### 6.18 安装器

未实现，下面是计划：

- `scripts/install.sh` 增加 `--import [notes|sessions|all]`，默认不导入。
- 交互运行时，安装结束后执行 `agent-memoryd import plan` 并显示汇总。汇总里有生成调用时，先问是否 propose；propose 之后
  显示提议汇总，再问是否 apply。非交互运行只有显式传了 `--import` 才导入，而且只到 propose 为止，apply 要单独执行。
- 导入需要 daemon 已经运行，安装器在服务启动之后再做。`--no-service` 时只生成计划，并打印后续命令。
- 安装器的端到端测试用合成的宿主目录覆盖 plan、propose 和 apply，抽取使用本地确定性的生成桩，不访问模型。

## 7. 代码位置

各层的入口见 [架构的代码地图](../architecture.md#code-map-and-evidence)。分层按 [AGENTS.md](../../AGENTS.md)：纯规则（单元大小、
切分、脱敏、单元键、抽取校验）在 `memory-rules`，类型和写入、回滚的记录计划在 `memory-domain`，`import_runs`、`import_ledger`
和抽取缓存 `import_extractions` 表在 `memory-store`（提议复用共享的 `proposals` 表），用例、抽取提示词和台账在
`memory-engine` 的 `app/import/`，宿主适配器和命令在 `memory-service` 的 `harness/import/`。算法 crate 没有导入代码，只提供
`nearest_facts` 和 `source_requests` 两个通用钩子，导入的代码不会进到算法里。

## 8. 验证

- 纯规则（单元划分、`--no-extract` 切分、脱敏、抽取校验）和各宿主适配器用合成的宿主目录做单元测试，覆盖 6.3 的筛选规则、
  worktree 归并和导出 JSONL 的解析。fixture 只放编造的内容，不复制真实会话，也不提交生成的 trace。
- 存储测试用临时存储、mock 检索模型和本地确定性的生成桩跑完整流程（[import.rs](../../crates/memory-service/src/diagnostics/import.rs)、
  [import_sessions.rs](../../crates/memory-service/src/diagnostics/import_sessions.rs)）：propose 不写记忆，评审决定，apply 的
  复查和跳过，重跑不写入，原样单元的取代，回滚和回滚跳过，抽取缓存；导入的记录能被 `search_memories` 找到，`get_context`
  不把它们当作已确立的事实；同一轮先捕获再导入时 daemon 返回 `duplicate:true`。
- 不调用付费模型，不跑完整数据集。

## 9. 待定问题

1. Codex 的 memories 和 rollout、Claude Code 的 transcript 都没有官方格式说明。适配器按版本字段（Codex 的
   `cli_version`、Claude 的 `version`）和文件结构识别，遇到不认识的结构就整体跳过并报告。要支持哪些版本范围还没定。
2. Codex `MEMORY.md` 里跨线程的总结（如 "Reusable knowledge"、"Failures and how to do differently"）要不要导入，导入的话
   怎样确定项目。
3. 导入的记录要不要进实体投影。进的话多一次实体嵌入，检索和 consolidation 聚类会更好。
4. 是否允许 global 作用域的导入事实（WorkBuddy 的跨项目记忆、Claude 的 `type: user` 笔记、`scope: global` 的建议）。
5. Codex 的 `chatgpt_handoff` 线程算不算用户会话。
6. 宿主删掉源文件（例如 Codex 遗忘了一条记忆）时，是否应该同步撤回（例如加一个 `--prune`），默认怎么做。
7. 反方向导出：把共享记忆写成 `AGENTS.md` 或 `CLAUDE.md` 片段，给接不了 MCP 的宿主使用。Letta 已经支持把记忆导出成
   这两种文件。
8. 单元上限取召回预算的一半、目标取 `searchBudget / topK` 是否合适；用户把召回预算调大以后，已经导入的单元要不要重新切分。
9. 评审时是否允许改写文本。现在只能接受或拒绝，改写要在 apply 之后用 `correct_memory`。
10. 导出提示词在各宿主上的遵从度：条目很多时会不会被截断或归纳，`{"continued": true}` 的分页是否可靠，需要在每个宿主上
    实际试一次。
11. 抽取提示词是否需要和形成共用 `mem0CustomInstructions`。
