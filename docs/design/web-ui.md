# 记忆管理 Web 界面

> 状态：第一期（4.1）和第二期（4.2）已实现：第 5 节的接口、`--ui-dir` 和 `web/` 前端；发布流程和安装器安装前端（7.3）
> 也已实现。第三期尚未实现。第 3 节的调研截至 2026-10-27。标"未核实"的条目只见于搜索摘要或第三方文章；闭源的
> 托管控制台只能按文档描述，看不到技术栈。第 4、5 节的接口和字段以 [API 与配置](../api.md) 和代码为准。

日期：2026-10-27。范围：一个由 daemon 自己提供的本机网页，用来浏览、检索、新增、更正、撤回和删除记忆，并审核
consolidation、导入和冲突。

## 1. 现在的情况

查看和修改记忆只有三条路：Agent 调用 MCP 工具，`agent-memoryd` 的 CLI，或者手写 `curl` 调 `POST /v1/*`、`/admin/*`。
数据模型里已经有很多值得展示的东西，但没有地方看：

- 状态和谱系：`current`、`superseded`、`conflict`、`candidate`、`stale`；`supersedes`/`supersededBy` 链、`validUntil`、
  `related`、`derivation`。更正不覆盖原文，而是写一条新的 `explicit` 记录取代它；撤回就是只取代、没有后继
  （[correction.rs](../../crates/memory-engine/src/app/management/correction.rs)）。
- 出处：`source.platform`、`source.turnIds`、`source.origin`（user/assistant/inferred/imported/explicit），三种时间
  （`effectiveAt` 生效、`statedAt` 说出、`recordedAt` 入库），以及 `get_memory` 的 `includeEvidence` 能取到的原始对话。
- 审核流程：未解决的冲突（uncertainties）、consolidation 运行和导入运行。两种运行都是先提议、再 apply，之后可以整批回滚。
- 召回记录：每次 `search_memories` 和 `get_context` 都写一条 `retrieval_log`，`feedback` 记录记忆有没有被用上。

`MemoryRec.proposal` 的注释写着"kept for the management UI"（[types.rs](../../crates/memory-domain/src/types.rs)），
可见最初就打算做管理界面。

## 2. 目标与非目标

目标：

- 浏览一个项目、一个平台或全部的记忆，能筛选、检索、翻页，看到每条记录的出处和更正历史。
- 增删改查都通过现有的 HTTP 接口完成，语义和 MCP、CLI 一致：改是"更正或撤回，附理由"，不是原地编辑。
- 把审核类的工作搬到网页上：冲突裁决、consolidation 和导入的逐条查看、apply、回滚。
- 让人看得到"哪个 Agent 在什么时候取回了哪些记忆"，以及自动召回时实际注入了什么。

非目标：

- 不做多用户、登录或远程访问。daemon 只监听回环地址（[README](../../README.md#before-you-install)），网页也一样。
- 网页不直接读写存储，只调用 JSON 接口。服务端各层的分工（AGENTS.md）不因为加了网页而改变。
- 不做原地覆盖编辑，也不绕开 `correct_memory` 改文本。
- 不做按平台暂停写入或召回：后端没有这个功能，需要另外设计（第 4.3 节第 17 条）。

## 3. 其他项目的做法

### 3.1 记忆库和记忆服务

| 项目 | 界面和技术栈 | 值得借鉴 | 对本设计的影响 |
|---|---|---|---|
| [mem0 OpenMemory](https://github.com/mem0ai/openmemory)（2026-07-29 移出 mem0 主仓库，已归档） | 独立的 Next.js 15 应用，Redux Toolkit、Radix、Tailwind 3、recharts | 表格按 app、分类、日期筛选并分页；搜索是 SQL `ILIKE` 关键词匹配；详情里有**访问记录**（哪个 app 读过）和同类记忆；归档、暂停、批量删除；可以按 app 暂停 | 访问记录对应本服务的 `retrieval_log`；默认隐藏已归档，对应默认只看活跃状态 |
| [mem0 server/dashboard](https://github.com/mem0ai/mem0/tree/main/server) | 独立的 Next.js 15 应用，TanStack Table、shadcn | Requests 页是 API 审计日志；Entities 页列出 user、agent、run 及计数，删除时级联 | 请求和任务列表值得做；分析、导出等页面锁在付费版之后 |
| mem0 Platform（闭源，按[文档](https://docs.mem0.ai/platform/features/dream)） | — | Dream：运行 → 生成的记忆 → 来源，列出被取代和被合并的记忆；文档里没有审批步骤，也没有撤销 | 本服务的 consolidation 能整批回滚，这一点比它强，界面要把回滚做出来 |
| [Letta](https://docs.letta.com/v1-sdk/ade)：ADE 已弃用；[letta-oss-ui](https://github.com/letta-ai/letta-oss-ui) | Electron，Vite 8、React 19、Tailwind 4、Radix | 记忆是 git 里的 Markdown 文件，每次修改是一个版本，History 列出提交并能恢复 | 版本历史值得做；ADE 的核心块、上下文窗口视图已经弃用，不要照抄 |
| Zep（闭源，按[文档](https://help.getzep.com/v4/graph-visualizer-assistant)）；Graphiti 没有界面 | 只公开了一个 2025-04 后没更新的 Next.js + D3 图可视化仓库 | 图浏览；"Ask graph"列出每一步检索的操作、参数、结果和耗时，并能在图上高亮证据；episode 有 [Debug Logs 和 Ingestion Traces](https://help.getzep.com/v4/debug-mode)，解释某条事实是怎么抽出来的 | 检索调试页、形成过程追溯 |
| [Cognee](https://github.com/topoteretes/cognee/tree/main/cognee-frontend) | Next.js 16、Mantine 8、TanStack Query、d3、react-force-graph-2d | 数据集、知识图谱、搜索、活动、分析 | `start_ui()` 下载对应版本的前端源码再 `npm run dev`，运行时要 Node：反例 |
| Supermemory：控制台闭源；[memory-graph](https://supermemory.ai/docs/recall/memory-review.md) 组件开源 | Canvas 2D + d3-force，Radix、Tailwind 4 | 图里的边表示相似度和**版本链**；推断出的记忆进审核队列，可以通过、驳回、撤销（API，控制台未核实） | 更正链适合画成版本链 |
| [MemOS 本地插件 viewer](https://github.com/MemTensor/MemOS/tree/main/apps/memos-local-plugin/viewer) | Preact + signals，Vite 5，不用 CSS 框架，hash 路由；**由插件自己的 HTTP 服务挂在 `/ui/` 下**；实时事件走 SSE | 记忆检索、筛选、分页、批量操作；candidate → active → archived 生命周期；导入导出包；重建嵌入 | 和本服务最像的托管方式 |
| [Hindsight](https://github.com/vectorize-io/hindsight) | 控制面是独立的 Next.js 16 应用；另有内嵌的 control center，Preact + Tailwind，**构建产物提交在仓库里**，由 Python 包在本机提供 | **作废时填理由，之后可以恢复**；历史按词做 diff，并列出每个版本依据的事实；表单纠正 LLM 抽错的文本、类型、日期；**检索调试页**分列语义、关键词、时间分数、RRF 融合、rerank 分数和 token 预算；LLM 请求日志 | 数据模型最接近：更正附理由、历史 diff、检索调试都直接可用 |
| [claude-mem](https://github.com/thedotmack/claude-mem) | 自己的 worker 提供 `/viewer`；React 19 用 esbuild 打成一个文件 | 会话和观察的实时流、分类筛选、删除；设置里能**预览会话开始时要注入的上下文** | 上下文预览对应 `get_context`，后端不用改 |
| MIRIX | Vite 5、React 18、Radix、Tailwind 3 | 检索可选 BM25 或嵌入，并指定字段；只读 | — |
| [Basic Memory Cloud](https://docs.basicmemory.com/cloud/file-history)（闭源） | — | 文件历史并排 diff，可以挑选改动恢复，"恢复会生成新版本，不是回滚"；建议式修改，接受或拒绝 | 恢复旧值也走更正，生成新记录 |
| Mastra Studio | Vite 8、React 19、TanStack Query/Table、Radix、CodeMirror、@xyflow | 工作记忆可以直接编辑，语义召回检索 | — |

Graphiti、memU、本地版 Basic Memory 没有界面；Memobase 的 playground 没有查看。

技术栈统计（只算核对过 `package.json` 的）：

- 托管方式：独立 Next.js 应用 6 个；Vite 或 esbuild 构建的单页应用 6 个。后者中有 3 个由记忆服务自己提供：MemOS、
  Hindsight control center、claude-mem。
- 框架：React 11 个，Preact 2 个（都是内嵌的本地 viewer）。没有一个用服务端模板加 HTMX。
- 组件和样式：Radix/shadcn 8 个，Tailwind 10 个（v4 有 7 个）。
- 图：d3 或 d3-force 4 个，@xyflow 1 个，自绘 canvas 1 个。

没有一个项目能把一次合并或导入的提议整批 apply、再整批回滚：mem0 Dream 只列出运行，Supermemory 只能逐条撤销。

### 3.2 Agent 产品和编程助手

| 产品 | 怎么管理记忆 | 对本设计的影响 |
|---|---|---|
| [ChatGPT](https://help.openai.com/en/articles/8590148-memory-faq) | 设置里分开控制"引用已保存记忆"和"引用聊天记录"；Manage memories 逐条删除；临时聊天不用记忆。2026-06 起改为自动更新并显示成一段记忆摘要，旧的逐条列表仍在 Saved memories 里。回答下方的 Sources 列出用到的记忆，可以就地纠正或选"不要再提"（上线日期未核实）。删除聊天不会删除从中得出的记忆 | 摘要背后要保留逐条记录；"用在了哪个回答里"值得做；删除来源时要连带处理派生记忆 |
| [Claude 应用](https://support.claude.com/en/articles/11817273) | 记忆按主题逐条保存，可以编辑、删除；每个 project 有独立记忆，可选是否共享账户记忆；暂停（保留但不读写）和重置分开；单个对话可以关闭记忆；导入其他助手的导出，逐条审核 | 暂停和删除分开；按项目隔离 |
| [Claude Code](https://code.claude.com/docs/en/memory) | CLAUDE.md 和 `.claude/rules` 是人写的指令；auto memory 是 Claude 写的笔记，带 `user`/`feedback`/`project`/`reference` 类型；`/memory` 列出文件、开关 auto memory；索引只加载前 200 行或 25KB，超出部分静默丢弃；`/doctor prompt-audit` 找过时或冲突的指令，只提议不修改 | 指令和事实分开展示；注入的内容和预算要看得见 |
| [Codex](https://learn.chatgpt.com/docs/customization/memories) | 默认关闭；后台从空闲对话生成记忆并脱敏；`/memories` 按对话控制能否读、能否写；`disable_on_external_context` 不从用了工具或网页的对话生成记忆；没有列表、编辑或审批界面 | 按来源类型筛选和控制 |
| [GitHub Copilot Memory](https://docs.github.com/en/copilot/concepts/agents/copilot-memory) | 每条事实引用支撑它的代码，使用前对照当前分支验证；28 天没用就删除，用一次重新计时；仓库设置里按时间倒序列出，只能删除；云端 agent、代码评审、CLI 共用 | 出处要能点开；显示"最近被召回"和"从未被召回" |
| [Cursor](https://forum.cursor.com/t/cant-clear-memories/148254)（部分未核实） | 2.1.x 移除了 Memories 管理界面，但记忆仍在使用；有用户的 32 条记忆删不掉，只能回退版本 | 反例：有写入就必须有删除 |
| [Windsurf](https://docs.devin.ai/desktop/cascade/memories) | 自动记忆按 workspace 存放，在 Customizations 面板查看、编辑；文档建议长期规则写进 Rules 或 AGENTS.md | — |
| [Gemini](https://support.google.com/gemini/answer/16413516) | Saved info 逐条编辑、删除、开关；回答的来源里标出"Your saved info"，Google 说明这个标注可能不准；可以导入其他 AI 的记忆导出和聊天记录 | 本服务掌握完整的召回路径，召回记录可以做到完整 |
| [Hermes Agent](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory) | `write_approval` 让每次写入（包括后台写入）都先进待审队列，`/memory pending`、`approve`、`reject`；写入前扫描提示注入、凭据外泄和不可见 Unicode；桌面版有记忆图，节点可编辑、删除 | 待审队列；对来自工具输出的内容提高警惕 |
| [Devin Knowledge](https://docs.devin.ai/product-guides/knowledge)（已被 Skills 取代） | 从对话反馈里建议知识，用户编辑后保存或忽略；按用户停用而不删除 | — |

用户的主要不满：记忆在用户不知情时写入（一项研究称 2,050 条 ChatGPT 记忆中 96% 不是用户要求保存的，第三方转述）；
旧的或错的记忆删不掉或会回来（个案，未核实）；通过被读取的内容投毒，例如
[MemGhost](https://labs.cloudsecurityalliance.org/research/csa-research-note-memghost-agent-memory-poisoning-20260723-c/)
演示了一封邮件就能写入一条假记忆。

### 3.3 归纳

- 人人都有的：逐条列表，搜索和筛选，翻页；详情；手动新增；删除；概览统计。
- 本服务已有数据、别人做得好的：出处和原始对话（Copilot、Hindsight）；附理由作废并保留历史 diff（Hindsight、Basic
  Memory）；检索调试和分项分数（Hindsight、Zep）；访问记录和"用在了哪里"（OpenMemory、ChatGPT Sources）；注入上下文预览
  （claude-mem）。
- 别人没有、本服务已经能做的：consolidation 和导入的整批 apply 与回滚，冲突的显式裁决。
- 要避免的：只给摘要不给逐条记录；能写不能删；删除来源后派生记忆还在；注入内容被截断却看不见。

## 4. 功能

### 4.1 第一期：浏览、增删改查、检索调试

| # | 功能 | 借鉴 | 用到的接口 | 缺口 |
|---|---|---|---|---|
| 1 | 记忆列表：按项目、平台、类型、状态、origin 筛选；默认只看活跃状态（current/conflict/candidate），可切换显示 superseded 和 stale；按 recordedAt 或 effectiveAt 排序；分页 | OpenMemory、MemOS | — | 需要 `list_memories`（5.1） |
| 2 | 项目和平台下拉框，带计数 | mem0 Entities | `/admin/stats` 只有类型 × 状态 | 需要 facets（5.2） |
| 3 | 详情：全文、scope、出处、三种时间、`validUntil`、更正链、`related`、`derivation`、原始对话 | Hindsight、Copilot | `get_memory`（`includeHistory`、`includeEvidence`）、`list_turns` | — |
| 4 | 新增：事实可以原样保存（`infer:false`）或交给模型抽取（`infer:true`）；指令（`memoryType:"instruction"`）选择 global/project/platform | 各家的手动添加 | `add_memories`，异步时用 `get_task` 轮询 | — |
| 5 | 更正和撤回：填新文本和理由，显示新旧 diff；不填文本就是撤回。只有 current 记录能更正，其他状态返回 409 并给出 `currentId`，界面跳到当前值 | Hindsight | `correct_memory` | 撤回之后没有"恢复"，只能重新新增 |
| 6 | 删除：确认框说明会连带删除的合并记录，删除后列出恢复为 current 的输入（`restoredInputs`） | — | `delete_memory` | 删除前没法预览影响范围；第一期只在结果里展示 |
| 7 | 检索调试：输入查询和 scope，显示排序、`score`、`rerankScore`、`tokensUsed`、`gap`、分阶段耗时 | Hindsight、Zep | `search_memories`（`touch:false`） | 不返回语义和关键词的分项分数，后续可以加 |
| 8 | 上下文预览：选项目和会话，看 `get_context` 要注入的文本、各部分和 token 用量 | claude-mem | `get_context` | 会写一条 `retrieval_log`，平台记为 `web`，召回记录里要能过滤掉 |
| 9 | 概览：各类记录数、LLM 用量、待处理任务、模型和索引状态 | 各家 | `/health`、`/admin/stats` | — |

### 4.2 第二期：审核和追溯

| # | 功能 | 借鉴 | 用到的接口 | 缺口 |
|---|---|---|---|---|
| 10 | 冲突队列：列出未解决的冲突，并排比较，选择保留哪条并填理由 | Hermes、Supermemory | `resolve_uncertainty` | 需要列出 uncertainties（5.4） |
| 11 | consolidation：发起提议（标明会调用模型）；逐条查看 merge/supersede/conflict 和被拒原因；apply；回滚，并显示 `skipReason`、`rollbackSkipReason` | mem0 Dream（它没有回滚） | `/admin/consolidate`、`consolidation_apply`、`consolidation_rollback` | 没有列出和查看运行的接口（5.5），界面不知道功能开没开（5.6）；apply 只能整批，不能逐条拒绝 |
| 12 | 导入：列出运行，逐条接受或拒绝，apply，回滚 | Basic Memory、Claude 导入 | `import_runs`、`import_run`、`import_apply`（`decisions`）、`import_rollback` | — |
| 13 | 召回记录：哪个平台、哪个会话、什么时候取回了哪些记忆，有没有被用上；详情里显示"最近被召回" | ChatGPT Sources、OpenMemory 访问记录、Memori | `feedback` | 需要分页接口（5.7）；`search_memories` 的日志补上平台、项目和会话（5.3） |
| 14 | 任务和失败：请求状态、失败原因、模型原始响应 | Zep Ingestion Traces | `get_task`、`/admin/export` 的 `requests`、`llm_responses` | 需要分页接口；模型调用全部失败的请求连不到原始响应（5.8） |

### 4.3 第三期：谱系、实体、过期提示

| # | 功能 | 说明 | 缺口 |
|---|---|---|---|
| 15 | 谱系：更正链、合并、派生、`related` | 数据都在；更正链大多是一条线，先在详情页用时间线展示，不引入图库 | 反向的边（谁由这条合并或派生而来）要查询（5.9） |
| 16 | 实体视图：按项目和类型浏览实体，看每个实体关联的记忆；详情页列出这条记忆的实体 | Mem0 的实体投影是算法私有表 | 由算法提供读取（5.10） |
| 17 | 按平台暂停写入或召回 | OpenMemory 和 Codex 有类似开关；后端没有，需要单独设计，不在第三期 | — |
| 18 | 过期提示：从未被召回、N 天没被召回；列表可按此筛选，概览给出数量 | `lastAccessedAt` 只有 `touch:true` 的检索才会更新，而默认不更新，所以不能用 | 按召回日志汇总（5.11） |

### 4.4 交互约定

- 请求头带 `X-Memory-Platform: web`；读原始输入时用 `scope.platform:"*"` 跨平台读取。
- 浏览和调试时 `search_memories` 传 `touch:false`，不改变记忆的访问时间。
- 会调用模型或产生费用的操作要标出来，并在提交前说明：`infer:true` 的新增（生成）、检索（查询要做嵌入）、
  `rerank:true`、consolidation 提议（每个簇一次生成）、reindex（重新嵌入全部记忆）、导入的 `extract` 单元。
- 指令和事实分开展示：指令是行为要求，事实只是参考（[API](../api.md#reads-and-provenance)）。

## 5. 后端要补的接口

都加在现有的 `/v1/{name}` 或 `/admin/{name}` 分发里，每个接口在入口校验一次参数（AGENTS.md），单元测试放在实现旁边。

第一期（已实现，见 [API](../api.md)）：

1. `list_memories`：按项目、平台、类型、状态、origin 和文本筛选，按 `recordedTs` 倒序翻页。`/admin/export` 只能一次
   返回全部。
2. `memory_facets`：各项目、各平台、各 origin、各类型和状态的记录数，供下拉框和概览使用。

第二期（已实现）。导入已经有 `import_runs`、`import_run`、`import_apply`、`import_rollback`，不用补；其余几张表之前只能
通过 export 整表读取：

3. 检索日志补字段：`search_memories` 写 `retrieval_log` 时，和 `get_context` 一样记 `kind`、平台、项目、会话和
   `createdTs`，并在 `returned` 里记 `gap`（[retrieval.rs](../../crates/memory-algo-mem0/src/retrieval.rs)）。之前 search
   的日志没有 `kind` 和 `gap`，所以 `/admin/stats` 的 `retrieval.gapSearches`（概览的"无结果的检索"）一直是 0。启动修复
   给旧的 search 日志补上 `kind`、`createdTs`（由 `createdAt` 换算）和 `gap`；当时没记平台、项目和会话，召回记录里显示为
   未知。
4. `list_uncertainties`：`{status?, project?, limit?}`，`status` 默认 `open`，按 `createdAt` 倒序，每个问题带上成员记录
   （字段和 `get_memory` 相同），界面不用逐条再取。问题只来自 consolidation 的 conflict 操作和启动修复，数量少，和
   `import_runs` 一样只按条数截断，不翻页。
5. consolidation 运行：`consolidation_runs` `{project?, limit?}` 倒序列出运行，不带 `clusters` 等大字段；
   `consolidation_run` `{runId}` 返回 `{run, proposals, memories}`，和 `import_run` 对应，`memories` 是提议涉及的记录摘要。
   之前只有 consolidate、apply、rollback 这几个写操作会返回运行。
6. `/admin/stats` 的 `features` 报告 `consolidation`（`consolidationEnabled`，默认关闭）和 `generation`（有没有生成模型）。
   界面据此禁用"发起提议"和"apply"，不必等接口返回 400 或 `MODEL_UNAVAILABLE`。
7. `list_retrievals`：`{kind?, platform?, excludePlatforms?, project?, session?, memoryId?, cursor?, limit?}`，按
   `createdTs`、`mid` 倒序翻页，每行带返回记录的 id、类型、状态、文本，以及 `feedback`。界面默认排除 `web`，也就是网页
   自己的检索和上下文预览。`retrieval_log` 没有清理，`memoryId`（详情里的"最近被召回"）要扫描数组字段，只看最近 30
   天（`createdTs` 有索引）；需要更长的历史时再加保留期或索引。
8. 任务：`list_requests` `{statuses?, kind?, platform?, project?, cursor?, limit?}` 倒序列出写入请求，不带
   `inputMessages`；`request` `{requestId}` 返回整条请求、它的 `extract_log` 和这些日志指向的 `llm_responses`。模型调用
   全部失败时没有 `extract_log`，`llm_responses` 也不记 `requestId`，请求上只有最后一次的 `error`；
   `list_model_responses` `{stage?, failedOnly?, cursor?, limit?}` 按时间列出原始响应，用来查这类失败；`failedOnly`
   只留没有得到合格输出的调用（无响应、非 2xx、响应不是 JSON 或输出格式不对）。

第三期：

9. `memory_lineage`：`{id, depth?}`，从一条记录出发，沿四种边向两个方向走，最多 `depth` 步（默认 3，最多 6），最多 200
   个节点：取代（`supersedes`/`supersededBy`）、合并和派生（`derivation.inputMemoryIds`，比如 consolidation 的合并）、
   视图引用（画像和技能的 `usedMemoryIds`）、`related`（`conflicts`、`related`、`same`）。正向的边记在记录上，反向的
   （谁以这条为输入）要查询。返回节点（记录摘要加 `recordedAt`、`origin`、`platform`）和带类型的边，界面按时间排成
   时间线。删除的记录不出现，边只连已有的节点。
10. 实体：`list_entities` `{project?, entityType?, text?, cursor?, limit?}` 按关联记忆数倒序翻页；`entity` `{id}`
    返回实体和它关联的记录；`memory_entities` `{id}` 返回一条记录关联的实体。实体投影是 Mem0 的私有表
    （`mem0_entities`），所以读取作为算法接口的方法实现，不让管理层直接读私有表；不返回实体的向量。读取时不重算投影：
    重算要给新实体做嵌入，会调用模型；投影由检索和写入保持更新，响应带上投影的更新时间。
11. 召回汇总：新的共享表 `memory_recalls`，每条记录一行：`lastRecalledAt`、`lastRecalledTs`、`recallCount`。写
    `retrieval_log` 的同一个事务里更新返回的每条记录；平台为 `web` 的调用（网页自己的检索和上下文预览）不计。启动时
    如果汇总表是空的而日志不是，就从全部日志回填一次。`list_memories` 每条记录带上 `lastRecalledAt` 和
    `recallCount`，并支持 `notRecalledDays`：入库超过 N 天、且最近 N 天没被召回的记录。`/admin/stats` 的 `retrieval`
    报告活跃记录里从未被召回的和 30 天没被召回的数量。

## 6. 安全前提

加网页之前：daemon 只监听 `127.0.0.1`，没有鉴权，也不检查 `Host` 和 `Origin`。

- `/admin/{name}` 的请求体是 `Option<Json<Value>>`。axum 0.8 对没有 `Content-Type` 的请求返回 `None`，按 `{}` 处理。
  因此任何网页用 `fetch(url, {method: "POST", mode: "no-cors"})` 不带请求体，就能触发 `/admin/reindex`（重新嵌入全部记忆，
  用托管模型时要花钱）和 `wait_idle`。`/v1/*` 要求 `application/json`，跨站请求会先预检，而 daemon 不回 CORS 头，所以被拦下。
- DNS rebinding 可以绕过同源限制，读写全部接口，包括 `/admin/export`。
- Chrome 从 142 起会在公网页面访问本机地址前弹出
  [Local Network Access](https://developer.chrome.com/blog/local-network-access) 授权；其他浏览器没有核实。

网页上线后浏览器就成了正式客户端，所以在第一期之前加一层中间件：`Host` 只接受 `127.0.0.1`、`localhost`、`[::1]`
加监听端口；带 `Origin` 的请求必须同源；`Sec-Fetch-Site: cross-site` 一律拒绝。这一层不依赖网页，已经单独实现
（[http.rs](../../crates/memory-service/src/api/http.rs) 的 `loopback_only`），同样作用于 `/ui/*`。

## 7. 托管方式和技术栈

### 7.1 托管方式

"分开"有三层：代码、构建产物、运行进程。前端代码放在 `web/`，构建产物单独发布，这两层和 daemon 分开；运行时由 daemon
在同一个进程里提供，不另起服务。

| 方式 | 先例 | 取舍 |
|---|---|---|
| **daemon 同进程提供，产物从磁盘加载（`--ui-dir`）** | MemOS、Hindsight control center、claude-mem 都由记忆服务自己提供静态文件（它们是 Node 或 Python 服务，文件随包装在磁盘上） | 一个进程、一个端口，同源，不用 CORS；前端单独构建、单独升级，`cargo build` 不需要 Node，`web/dist` 也不用提交；两个产物要对齐版本 |
| daemon 同进程提供，产物编译进二进制（rust-embed） | — | 版本总是一致；但要么把 `web/dist` 提交进仓库，要么 `cargo build` 时需要 Node |
| 独立的网页进程（Next.js 等） | mem0、Cognee、Hindsight 控制面 | 多一个端口和常驻进程，安装器要再装一个用户服务；浏览器跨源调 daemon，要么 daemon 开 CORS（和第 6 节的加固相反），要么由它反向代理，它就成了 daemon 前面的网关，要自己再做一遍 Host 和 Origin 检查；网页和接口分开升级，可能对不上 |
| Electron 桌面应用 | letta-oss-ui | 太重 |

采用第一种：

- 启动参数 `--ui-dir PATH`，环境变量 `MEMORY_UI_DIR`。不设置时不提供 `/ui/`，访问返回 404 和一句如何安装前端的说明。
  设置了就在启动时校验一次：目录存在且有 `index.html`，否则启动失败。`service install` 装了前端时，把绝对路径记进服务
  配置，和 `surreal` 可执行文件的路径一样。
- `GET /ui/` 返回 `index.html`，`/ui/*` 返回目录里的静态文件。前端用 hash 路由，不需要回退到 `index.html`。用 tower-http
  的 `ServeDir`，它会拒绝 `..` 之类越出目录的路径。响应带 `Cache-Control: no-cache`，前端升级后浏览器不会继续用旧文件。
  daemon 只读这个目录，不写。
- 网页和 `/v1`、`/admin` 同源，第 6 节的 Host 和 Origin 检查同样作用于 `/ui/*`。
- 前端和 daemon 用同一个版本号（workspace 的 `version`）。`/health` 报告这个版本，构建时把前端的版本写进
  `version.json`；前端启动时比对，不一致就在页面顶部提示。
- 开发时可以 `vite dev`，把 `/v1`、`/admin`、`/health` 代理到 daemon；也可以 `vite build --watch` 输出到一个目录，让
  daemon 的 `--ui-dir` 指向它，刷新页面即可，不用重新编译 Rust。

以后要从手机或别的机器访问、要多用户登录、或者一个网页连多个 daemon 时，再加一个独立的、带认证的网关进程。daemon
只允许绑定回环地址，绑别的地址时启动报错，说远程访问需要一个带认证的网关
（[agent_memoryd.rs](../../crates/memory-service/src/bin/agent_memoryd.rs)）。前端只调 JSON 接口，到时可以原样放到网关后面。

### 7.2 前端技术栈

| 方案 | 构建要求 | 适合 | 不足 | 先例 |
|---|---|---|---|---|
| **React 19 + Vite + TypeScript** | 开发和构建前端要 Node；用户安装不需要（7.3） | 表格、筛选、表单、diff、审核队列，以后加图 | — | 调研中的大多数 |
| Preact + Vite | 同上 | 同上，体积更小 | shadcn、@xyflow 等要走兼容层；在本机体积不是问题 | MemOS、Hindsight control center |
| 服务端模板（askama 或 minijinja）+ HTMX | 不需要 Node | 列表、表单、逐条审核都能做 | 网页处理器在 daemon 里直接调引擎，相当于第二套接口；图和 diff 仍要另写 JS；调研里没有先例 | 无 |
| Leptos、Dioxus（Rust 编译成 WASM） | trunk 或 wasm 工具链 | 和后端共用 Rust 类型 | 当前版本分别是 0.9.0-rc0、0.8.0-alpha；组件生态小，编译慢 | 无 |

推荐 React 19 + Vite + TypeScript：

- 第 4 节的功能主要是表格、筛选、表单、diff 和审核队列，以后加谱系图。这些在 React 生态里都有现成的库。
- 前端只调 JSON 接口，补出来的接口（第 5 节）CLI 和 MCP 也能用。
- 调研里绝大多数界面用 React，也最容易找到参考实现。

依赖控制在这些：TanStack Query 5（服务端状态和 `get_task` 轮询）、TanStack Table（列表）、Radix 原语或 shadcn 组件加
Tailwind 4、hash 路由、`diff`（更正前后的词级 diff）。谱系图到第三期再加 @xyflow/react 12。请求和响应的 TypeScript 类型
先手写，和 `memory-domain` 的类型对齐。前端逻辑的单元测试用 Vitest，放在组件旁边，由 `make test-web` 运行。

服务端模板加 HTMX 的主要好处是不需要 Node。前端产物单独发布以后，只有开发前端的人需要 Node，用户安装时不需要，
所以不再把它作为备选。

### 7.3 前端产物怎么装到用户机器上

`web/dist` 不提交，加进 `.gitignore`。`cargo build` 和 `make test-workspace` 都不涉及 Node。

| 安装方式 | 前端从哪里来 |
|---|---|
| 发布的二进制包 | 发布流程每个版本构建一次前端（和平台无关），放进各平台压缩包的 `ui/` 目录，同时单独上传 `agent-memory-ui-<版本>.tar.gz` 和它的 SHA-256 |
| 源码安装（`install.sh`） | 有 Node 时用 `make ui` 在本地构建；没有时下载与当前版本对应的前端包并校验 SHA-256；两样都不行就跳过，daemon 照常运行，只是没有网页。`--no-ui` 直接跳过，已安装的服务原来提供的网页保留。前端装到 `~/.local/share/agent-memory-service/ui`（和记忆数据同级），`service install --ui-dir` 把这个目录记进服务配置；安装器不管理服务时（`--no-service`、手写的服务文件、手动启动的 daemon）不装前端 |
| 开发 | `make ui` 输出到 `web/dist`，`--ui-dir web/dist` |

下载的前端包只有在 checkout 正好是某个发布 tag 时才和代码一致。在 main 上安装时版本可能对不上，页面会提示
（7.1），这时应该用 Node 在本地构建。离线又没有 Node 的源码安装没有网页。

## 8. 实施顺序

1. Host 和 Origin 中间件（第 6 节），单独提交（已完成）。
2. 后端接口（第 5 节）和单元测试。
3. daemon 的 `--ui-dir` 和 `/ui/` 路由，`/health` 报告版本；前端骨架和第一期的页面；`make ui`、`make test-web`。
4. 发布流程打前端包，安装器安装前端（7.3）（已完成）。
5. 第二期：冲突、consolidation、导入、召回记录、任务（已完成）。
6. 第三期：谱系、实体、过期提示（4.3、5.9–5.11）；按平台暂停另行设计。
