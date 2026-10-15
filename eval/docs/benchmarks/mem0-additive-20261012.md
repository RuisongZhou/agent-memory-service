# Mem0 additive LoCoMo results — 2026-10-12

Historical evidence for the earlier Python OSS 2.2.1 comparison, **not a score for the current Node SDK 3.3.1-aligned implementation**. The original design record is available at [commit a0f76f1](https://github.com/RuisongZhou/agent-memory-service/blob/a0f76f1e3417d0cf6eca88e3828af71078798985/docs/design/mem0-strategy-design.md). Protocols, raw responses and local stores remain unchanged.

以下数据摘自本地已完成实验 `eval/results/locomo-full-mem0-20261011-v3`，与设计分析分开陈述。实验覆盖 10 段对话、272 个会话、5882 条原始发言；每个系统按两个视角处理 11764 条输入消息、1696 个批次，使用全新存储。

双方共用 Qwen3.8-Flash、非思考模式、温度 0、8192-token 抽取输出上限、BGE-M3 Q8_0 嵌入，以及相同回答和评判提示词。检索完成 3 遍，保留首遍与热缓存两遍；最终上下文的精确预算和稳定性经过审计。

| 指标 | 新方案 `mem0-strategy` | 本地 Mem0 OSS 2.2.1 |
| --- | --- | --- |
| 标准问答正确数 / 总数 | 1267 / 1540 | 1270 / 1540 |
| 标准 J 通过率 | 82.27% | 82.47% |
| 标准文本 F1 | 52.75% | 52.52% |
| 累计同步批次保存耗时 | 5789.9 秒 | 5266.2 秒 |
| 形成阶段实际墙钟时间 | 692.2 秒 | 618.8 秒 |
| 首遍检索均值 / P95 | 36.2 / 41.9 毫秒 | 30.9 / 38.4 毫秒 |
| 热缓存检索均值 / P95 | 24.1 / 26.0 毫秒 | 26.2 / 30.1 毫秒 |
| 保存记忆数 | 4000 | 3958 |
| 形成模型尝试数 | 1782 | 1698 |

J 指共用评判流程对答案正确性的通过率，F1 衡量与参考文本的重合。配对题目中，新方案独对 80 题，SDK 独对 83 题。此次标准正确率差为 0.20 个百分点，不能据单次结果宣称算法显著更优或更差。

保存耗时需要两个口径。累计耗时是 1696 个同步保存批次的耗时相加；实际墙钟是同等对话并发下的形成阶段耗时，包含启动和导出。前者不是单线程运行时间，也不是数据库吞吐量。

新方案累计保存耗时约多 9.95%。远端模型耗时占批次累计时间的 90.43%，SDK 为 94.09%；新方案有 86 次契约修复调用，SDK 有 2 次 JSON 修复。差额包括更多模型生成、退避和本地处理。现有记录没有把数据库和所有本地开销单独隔离，不能将剩余差额全部归因于 SurrealDB。

还有 446 道 category 5 对抗题，使用另行声明的语义拒答扩展，新方案通过率 50.90%，SDK 为 54.93%。它们没有纳入标准 J / F1；`summary-unified.json` 的混合总分也不能代替表中标准分数。

本次 SDK 使用本地 Qdrant，缺少可选 fastembed / spaCy 功能，只评估语义路径，不代表 Mem0 托管平台或论文中的全部能力。远端提示词缓存与执行顺序没有完全控制；完整数据也包含此前用于开发的小样本，不能称为未接触的独立测试集。全量测试只比较新方案和 SDK，未重跑 `memos`（历史标签为 `current`），故此处不拼接旧方案历史分数。

原始证据包括 `comparison.md`、`completion-audit.json`、`paired-analysis.json`、固定协议、源码和二进制指纹、各批次输入及响应、三遍检索、上下文、回答和评判结果。本报告只引用汇总事实，不复制数据库、密钥、模型权重或大量原始对话。
