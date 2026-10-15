# Mem0 SDK 3.3.1 流程对齐设计与实现

2026-10-14。本轮目标是 **npm `mem0ai@3.3.1` 的 OSS `Memory`**，源码固定到 `ts-v3.3.1`。3.3.1是Node/TypeScript SDK版本；Python同日核验为2.2.1。此前针对Python V3管线的报告保留为历史记录，不再作为当前实现的对齐结论。

## 1. 现有方案是什么

原服务借用了Python OSS 2.2.1的additive提示词和形成思路：输入持久化、读取近期消息与相关旧事实、一次LLM提取、去重和批量向量入库，公开搜索仅使用语义相似度。本次将消息、提示词、实体索引和混合检索的依据统一为Node SDK 3.3.1。

官方来源：

- [3.3.1发布页](https://github.com/mem0ai/mem0/releases/tag/ts-v3.3.1)
- [add/search/update/delete实现](https://github.com/mem0ai/mem0/blob/ts-v3.3.1/mem0-ts/src/oss/src/memory/index.ts)
- [提取提示词](https://github.com/mem0ai/mem0/blob/ts-v3.3.1/mem0-ts/src/oss/src/prompts/index.ts)
- [实体提取](https://github.com/mem0ai/mem0/blob/ts-v3.3.1/mem0-ts/src/oss/src/utils/entity_extraction.ts)
- [混合评分](https://github.com/mem0ai/mem0/blob/ts-v3.3.1/mem0-ts/src/oss/src/utils/scoring.ts)、[词形处理](https://github.com/mem0ai/mem0/blob/ts-v3.3.1/mem0-ts/src/oss/src/utils/lemmatization.ts)、[默认SQLite向量/BM25存储](https://github.com/mem0ai/mem0/blob/ts-v3.3.1/mem0-ts/src/oss/src/vector_stores/memory.ts)

实际npm发布包经registry的SHA-512 integrity核验，发布包、源码包及版本元数据均保存在本地`eval/results/mem0-sdk331-20261014/`。运行对照直接加载发布包，不用Python SDK替代。

## 2. 目标与已有问题

目标是让同一批消息经过相同的提取准备，让同一组记忆、查询和向量经过相同的候选及评分规则。数据库仍采用SurrealKV，API仍保留项目、会话、异步任务和批次来源约定。

差异不只是版本号：

| 环节 | 3.3.1实际规则 | 本次调整 |
| --- | --- | --- |
| 新消息 | role/content用换行连接，末尾不额外换行，全文保留 | 替换Python式尾换行 |
| 历史裁剪 | 最近10条，300 UTF-16单位 | 不再按Python Unicode字符数计数 |
| 提示词 | TS运行时模板、紧凑JSON | 精确复制运行时值并更新NOTICE |
| 空批 | 拒绝全部由空白内容组成的消息数组 | 在接收任务前拒绝 |
| 形成后处理 | 词形处理、实体提取、批量向量、实体-记忆关联 | 增加持久实体投影及维护 |
| 公开搜索 | 语义候选池上叠加BM25/实体分数 | 替换仅语义排序 |
| 可选rerank | 混合topK之后重排，保留原score | 返回额外rerankScore；不可用时保留混合结果 |

此前已修复的历史资格仍保留：同一tenant/platform/project/session、前序inputSeq、infer不为false、formed=true且状态indexing/completed；零记忆成功批可继续提供上下文，raw和失败输入不进入历史区。

## 3. 写入设计

处理顺序为：完整输入落盘 → 纯向量召回10条已有事实 → 单次additive提取与严格校验 → 精确去重 → 批量嵌入事实 → 主事实/向量/来源事务提交 → 构建实体投影 → 发布可检索回执。

形成阶段的旧事实召回仍是纯向量top10，不拿公开混合搜索替代。`infer=false`跳过生成及实体提取，原文直接成为可检索事实；它不进入后续`Last k Messages`，但可以作为旧记忆被召回。

实体规则移植官方内建正则路径，包括PROPER、QUOTED、TOPIC和IDENTIFIER的抽取、过滤及顺序；词形处理移植官方内建stop-word/simple-stem路径。**本次明确对齐未安装`compromise`和`natural`的官方支持配置**，不宣称与安装这些扩展后的NLP输出完全一致。复制和改编代码保留Apache许可与来源。

实体先按归一化文本精确匹配；无精确命中时，在相同可见范围内以余弦≥0.95匹配既有实体。本批待新增实体不互相做语义合并，符合SDK“先查旧实体、后批量插入”的行为。新增事实记录formationPosition，保留批内次序；旧记录缺该位置时采用稳定ID顺序，不能恢复已经丢失的原始候选顺序。

### 可恢复实体投影

新增算法私有表：

- `mem0_entities`：规范实体文本、类型、范围、向量、linkedMemoryIds和规则版本。
- `mem0_entity_state`：规则/嵌入模型版本、主事实指纹、已确认的推断资格及实体数量。
- `mem0_entity_inputs`：按事实保存文本与规则版本匹配的实体提取中间结果；退役或失去推断资格时清理。

实体是从主事实重建的投影。实体集合与状态标记原子提交；嵌入失败不会发布新状态。形成已提交而投影未完成时，任务保留indexing，恢复时补建投影，不重复调用提取LLM。

首次查询或主事实变化后，评分前在形成锁内协调投影。通用存储在原写入事务中持久化提交版本，并在成功后发布有限长度的变更 ID 日志。正常热查询只核对内存版本并使用类型化实体向量，不扫描事实和实体表。算法缓存属于具体 Tenant/Store 实例，不按租户名称全局复用。

常规变更只点读受影响事实、重新提取变更文本，并差量持久化实体投影。为保持既有规范实体 ID、批次合并顺序和向量覆盖行为，仍在内存中重放缓存的历史实体合并；不是完全 O(新增量) 的图更新。词形/BM25 语料范围及混合评分公式不变。

纠错会撤销旧关联、加入新文本实体；删除会清除无效关联和派生提取缓存。查询组装前批量复读权威主记录，按原排序过滤失效或越界候选。此维护不调用生成模型，但可能调用本地嵌入模型。

投影发布在存储写锁内核对依赖版本，失配则重新准备，事务失败不得发布缓存。重启、变更日志淘汰、不透明写入或投影表变化走权威恢复；损坏的提取中间结果作为缓存未命中重新提取。固定正则只初始化一次，动态实体包含判断保留原 Unicode 空白边界，不使用无界缓存。

旧库缺实体表时自动回填；缺ingestionMode时依据已形成的Mem0输入来源或纠错前驱确认推断资格，无法确认的记录不冒充推断事实。原始记忆仍可语义/BM25检索。实体数量检查用于发现缺行，不是针对任意外部篡改的全量校验。

## 4. 检索设计

1. 对查询执行官方内建词形处理与实体提取，生成查询向量。
2. 语义候选池大小为`max(4 × topK, 60)`。
3. 对全部可见文档计算默认SQLite路径的BM25，参数k1=1.5、b=0.75；按查询长度选择官方sigmoid参数。统计语料不缩小到语义候选池。
4. 查询实体取前8个后去重；每个实体最多查500项，余弦≥0.5才加分。关联过多的实体按官方公式降权，同一记忆只取最大实体加分。
5. 先按语义阈值过滤，再融合、排序并取topK。关键词和实体只提供分数，不把未进入语义池的记忆加入候选。
6. 可选rerank只重排这topK条；最后在上下文预算内返回完整记录。

公式保持官方默认值：

```text
entityBoost = similarity × 0.5 / (1 + 0.001 × (linkedCount - 1)²)
score = min((semantic + normalizedBM25 + entityBoost)
            / (1 + hasBM25 + 0.5 × hasEntity), 1)
```

`hasBM25`和`hasEntity`由整张分数映射是否非空决定，不按单条候选决定。`mem0Threshold`仍是融合前的语义门槛。`mem0TopK=20`、`mem0Threshold=0.1`不根据样本得分调整。

官方JavaScript使用双精度计算实体余弦；在0.5/0.95边界之前使用f64累积和比较，避免f32四舍五入改变是否加分。最终公共score沿用服务f32表示，对照允许2e-6数值误差，不调整语义阈值。

### 分层

`memory-rules`保存消息格式化、实体正则、词形、BM25和评分纯规则；`memory-algo-mem0/entities.rs`协调实体投影；`repo.rs`集中数据库查询；formation和retrieval分别协调写读。公共API、运行时和存储引擎不引入Mem0专属NLP逻辑。

## 5. 保留的服务边界

- tenant/platform/project/session映射既有服务隔离语义，长期事实仍可同项目跨会话检索；不是把SDK全部entity参数原样映射。
- ObservationDate使用消息真实日期，避免回放旧对话时锚到今天；SDK默认使用当前日期。中文模式可追加语言要求。
- 原文完整留存；历史裁剪若恰好落在emoji代理对中间，退回完整字符边界，不产生SDK可能生成的孤立surrogate。
- 历史按inputSeq及批内数组顺序选最新10条，不复制SDK同批created_at相同造成的淘汰顺序问题。
- 保留严格契约、原始响应、可恢复任务和原子主存储；不复制SDK吞解析/部分写入失败的行为。
- 仍有服务自己的项目级精确去重、上下文预算、显式纠错与来源记录。这不是将全部SDK API和数据库逐字复刻。

## 6. 评测与部署

入口`eval/mem0_sdk331_compare.py`直接运行真实npm包，使用独立存储、相同固定输入和同一个服务mock嵌入端点，生成阶段使用同一确定性替身。检查系统提示词、新消息、历史、旧记忆JSON、落库文本、实体和关联，以及候选与分数；另有HTTP回归覆盖失败、重启、纠错、删除和范围。

该评测验证处理流程与算法，不是LLM回答质量、真实模型速度或数据库吞吐量。没有付费模型调用或全量LoCoMo重跑。最终数字及原始证据见`eval/results/mem0-sdk331-20261014/comparison.md`；前一版Python报告不作为新结果引用。

配置和启动仍沿用`.env.example`、`examples/service-config.json`、`docs/api.md`和现有CI。无需新增密钥或部署Node运行时；Node包仅用于eval对照。旧库可由新服务首次访问时回填实体，但旧版本已提取错误的事实不会因增加实体索引自动修正，仍需根据原始批次显式纠错或在独立库回放审核。
