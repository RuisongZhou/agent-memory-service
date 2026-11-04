# 在网页里设置模型，一键安装本机检索模型

> 状态：已实现。接口和字段以 [API 与配置](../api.md) 和代码为准。

日期：2026-11-04。范围：网页「模型设置」页（`#/settings`）。在这一页可以：

- 改抽取用的生成模型和检索用的 embedding/rerank 模型，包括地址、模型名和 API key；
- 一键装好 llama.cpp 和 BGE 模型，在本机运行两个 llama-server，然后把 daemon 切过去。

## 1. 现在的情况

- daemon 只在启动时读模型设置，来源有命令行参数、环境变量和 `serve --env-file` 指向的文件。
  `install.sh service` 装的服务用 `~/.config/agent-memory-service/.env`（0600）。改设置就要手动改文件，再重启服务。
- 本机模型只有 `scripts/self_hosted_models.sh`，llama.cpp 要自己装，进程也要自己托管（见
  [自托管检索模型](../self-hosted-models.md)）。

## 2. 保存设置

```text
网页 ──POST /admin/model_settings────────▶ daemon：各项的值和来源，key 只报"已设置"
     ──POST /admin/model_settings_test───▶ daemon：用新设置连一次真实的接口
     ──POST /admin/model_settings_apply──▶ daemon：测试 → 写 env 文件 → 原地重启
```

- **来源**：每项设置报告来源，可能是命令行参数、环境变量、配置文件或默认值。
  命令行参数和环境变量的优先级高于 env 文件，网页改不了这两种来源的项，改了会被拒绝。
  daemon 启动时没有 `--env-file`，页面就只能看。
- **入口检查**：每个值都只能占一行，不能有首尾空格或引号，也不能有控制字符，否则写进 env 文件后会被读成别的内容。
  URL 必须是 http(s)。模型提供方只能是 `api` 或 `self-hosted`。
- **key**：只进不出，页面只知道 key 设没设。留空表示不改。
  生成接口，或 embedding/rerank 接口，换到另一个主机时（按 scheme、主机、端口判断），必须重新填对应的 key；
  已保存的 key 不会发到新地址，测试时也不会。检索模型没有自己的 key 时用生成模型的 key，这条规则也照样适用。
- **测试**：保存前一定先测试，测试不过就不保存。测试内容：
  - 检索模型：按 daemon 启动时的做法连一次（`ModelHub::connect`）。本机 llama-server 先探一下 `/health`，
    连不上就立刻报错，不必等完 30 秒。测试结果里报告 embedding 模型或维度是否会变；一变，重启后就会重算全部向量，
    页面会提前提醒。
  - 生成模型：只请求 `GET {base}/models`，不调用生成，不花钱。没有 key，或接口不提供模型列表时，报"无法确认"，
    不拦保存；key 被拒（401/403）或连不上时，拦住保存。
- **写文件**：只替换要改的那几行（同一个键写了多处时，删掉后面重复的），注释和其他行保持原样。
  先写到临时文件，再改名替换，权限 0600。原文件先存为 `.env.previous`。
- **重启**：daemon 先回应页面，再停掉存储，然后用同样的命令行 `exec` 自己，PID 不变。所以不管 daemon 由 launchd、
  systemd 还是终端启动都能用。重启前，要从环境里去掉上次从 env 文件读进来的键，否则它们会盖过新文件。
  重启期间看守进程不会因为存储停掉而退出（`settings::RESTARTING`）。
- **回滚**：新设置启动失败（模型连不上等）而 `.env.previous` 还在时，daemon 会做三件事：放回原文件，
  把原因写进 `.env.rolled-back.json`，再重启一次。页面顶部会显示这次回滚。启动成功就删掉 `.env.previous`。
- **页面等待**：页面记下保存前 `/health` 的 `startedAt`，然后轮询到它变了为止，再重新读取设置。

## 3. 一键安装本机检索模型

```text
网页 ──POST /admin/llama─────────▶ daemon ──运行──▶ installer/run models --status --json
     ──POST /admin/llama_install──▶ daemon ──后台──▶ installer/run models --apply --json --source S
```

`install.sh models --apply`（`scripts/install/models.py`）在终端里也能直接用，步骤是：

1. **llama-server**：依次找 `--llama-server`、PATH 和 Homebrew 的目录。macOS 上找不到时运行
   `brew install llama.cpp`；Linux 上找不到就报错，说明到哪里获取。第一版不在 Linux 上自动下载二进制。
2. **权重**：下载 BGE-M3 和 bge-reranker-v2-m3 的 Q8_0 GGUF，共约 1.27 GB，放在
   `~/.local/share/agent-memory-service/models/`。
   - 下载地址固定到 commit，并记有每个文件的 LFS 大小和 SHA-256。
   - 下载中断后可以续传，服务器不支持 Range 时从头下载。
   - 下完先核对大小和 SHA-256，对不上就删掉。
   - 来源：网页可选 huggingface.co 或 hf-mirror.com；终端里还可以给一个镜像地址。
3. **服务**：每个模型注册一个用户服务，macOS 上是 LaunchAgent `local.agent-memory-{embed,rerank}`，Linux 上是
   systemd 用户单元 `agent-memory-{embed,rerank}.service`。
   - 两个服务分别跑在 `127.0.0.1:8081` 和 `:8082`，参数与 [自托管检索模型](../self-hosted-models.md) 里的命令一致，
     开机自启，退出后自动重启。
   - 单元文件里有标记。不是这个命令写的单元不会被覆盖。
   - 重跑时单元没变、服务也在运行，就什么都不做。
4. **就绪**：等两个 `/health` 都答复，最多 180 秒。

这一步只装模型，不改 daemon 的设置。模型就绪后，页面上的「改用本机模型」会把表单填成 `self-hosted` 和默认地址，
保存仍走第 2 节的测试、写文件、重启流程。

- **后台运行**：下载可能超过一次请求能等的时间（600 秒），所以 daemon 通过一个 shell 在后台启动安装器，shell 随即退出。
  这样 daemon 重启或页面关掉，都不会打断安装。daemon 一直等到这次运行登记了自己，才回应页面。
- **运行记录**：每一步都写进 `~/.local/state/agent-memory-service/llama-install.json`，写法是原子替换。
  `--status` 读这个文件，再加上当前状态：llama-server 在哪、权重有没有下完、服务装没装、有没有答复。
  记录里的进程已经不在，就报"中断"。有一次正在运行时，第二次 `--apply` 会被拒绝。
- **来源限定**：网页只能选两个固定的来源名，daemon 不接受页面传来的 URL。
- **页面轮询**：安装进行时，页面每 2 秒读一次状态，切到别的标签页也继续，权重下载显示进度条。

## 4. 决定

- **不做热更新，走重启**：模型客户端、向量维度和索引元数据都是启动时确定的；热切换要在运行中重建这些，风险大。
  原地重启只要几秒，换 embedding 模型时，向量的重算本来也发生在启动阶段。
- **key 仍存 env 文件（0600）**：与安装器现有做法一致，Linux 上也能用。不接 macOS 钥匙串，
  因为那样还要改 daemon 的启动方式。
- **本机模型只装检索用的一对**：抽取仍用配置的生成接口。本机生成模型的抽取质量和 JSON 格式需要另外评估。
- **测试只用替身**：测试替身包括 `llama-server` 和 `launchctl`/`systemctl`，权重由本地服务器提供，
  见 `scripts/tests/test_models.py`。仓库测试从不运行 `brew install`，也不下载真实权重。

## 5. 测试与验证

- Rust：`envfile`（写入保留其他行、0600）、`settings`（命令行参数识别、保存后启动失败的回滚）、`api::settings`
  （来源、key 不回显、入口检查、没有 `--env-file` 时不能保存）、`api::platforms`（两个 llama 接口：
  后台启动、拒绝并发、拒绝 URL 来源）。
- Python：`scripts/tests/test_models.py`，在 launchd 和 systemd 两种模式下各跑一次完整的 `--apply`：
  下载、核对、启动、重跑无改动。另外覆盖续传、服务器不支持 Range、摘要不符时删除、失败的记录、
  并发拒绝、拒绝覆盖手写单元、缺少 llama-server 时的提示。
- Web：`web/src/lib/settings.test.ts`，覆盖表单、换主机必须重填 key、换模型前的重算提醒。
- 本机端到端：一个真实的 daemon，带 `--env-file` 和 `--installer`；检索模型和生成接口都是本地替身。
  - 在页面上保存后，daemon 在 3 到 4 秒内原地重启，换上了新的 embedding 模型。
  - 新设置启动失败时恢复了原文件，页面说明了原因。
  - 一键安装在替身 `launchctl` 下注册并启动了两个服务；「改用本机模型」保存后，daemon 连上了 8081/8082。
