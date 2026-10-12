# LoCoMo 数据

`locomo10.json` 下载自官方仓库：
https://raw.githubusercontent.com/snap-research/locomo/main/data/locomo10.json

下载日期：2026-10-09。SHA256：
`79fa87e90f04081343b8c8debecb80a9a6842b76a7aa537dc9fdf651ea698ff4`。

本地测试核验 10 段对话、272 个有内容的会话、5,882 句；按连续两句配对
产生 3,011 个 turn，其中 140 个会话以空 assistant 补齐最后一句。
conv-26 另有 16 个无对应内容的日期键，映射时忽略。
类别 1–4 共 1,540 题：多跳 282、时间 321、开放域 96、单跳 841；类别 5 不评测。

会话时间按 UTC 解释，句子依次加 1 秒；图片描述追加到正文。
所有题目与对话保留原文，具体映射见 `../common/locomo.py`。
