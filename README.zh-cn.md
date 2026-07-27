# Tokenx

> 本地 AI 编码客户端用量统计，强调明确的数据语义，以及在大型 transcript
> 集合上的可预测资源占用。

![Tokenx TUI overview](.github/assets/tui-overview.png)

## Tokenx 做什么

Tokenx 会读取本地 AI 编码客户端状态，把带 token 信息的记录转换成 CLI 和 TUI
报表，并对本地数据、客户端身份、定价和资源占用采用明确规则。

## 为什么维护这个项目

- **本地优先统计。** 本地报表只从带 token 的记录派生。供应商上报的花费、积分、余额和只有金额没有
  token 的行不会混进 token 成本。
- **行为显式。** 解析失败、缺失数据、未知客户端和无法匹配的价格保持可见，不用猜测别名或假成功路径掩盖。
- **稳定客户端身份。** 客户端 id、展示信息和前端 registry 由
  `crates/tokenx-engine/client-catalog.json` 统一定义。
- **唯一报表模型。** 完整 TUI 与无头 Models 投影消费同一份规范用量数据。
- **更低内存占用。** 消息管线避免不必要的 clone，并在源文件没有变化时跳过完整 reload。

更多背景见 [维护者上下文](CONTEXT.md) 和
[架构决策](docs/adr/)。

## 从源码构建

前置要求：

- Bun
- 稳定 Rust 工具链

```bash
# 在 Tokenx 源码 checkout 中
bun install
bun run build:native
```

运行本地 launcher：

```bash
# 打开交互式 TUI
bun run cli

# 适合脚本的报表
bun run cli -- models --no-spinner

# 执行一个 Client integration 并查看 Data Health
bun run cli -- models --client codex --json --no-spinner
```

`bun run cli` 会通过 `packages/tokenx` 执行当前 checkout 中的代码。发布包名为
`@juya-ai/tokenx`。

## 常用命令

```bash
# TUI
tokenx
tokenx tui
tokenx tui --tab models

# 唯一无头 Models 投影
tokenx models --no-spinner
tokenx models --no-spinner --json
tokenx models --group-by client,model --no-spinner

# 过滤
tokenx tui --client opencode,claude --week
tokenx models --since 2026-01-01 --until 2026-01-31
tokenx models --group-by client,provider,model --json

# TUI 内的期间报表与 Sessions
tokenx tui --tab subscription
tokenx tui --tab monthly
tokenx tui --tab sessions

# 查询价格目录
tokenx pricing lookup claude-sonnet-4-5 --no-spinner
tokenx pricing overrides --json
```

从源码运行时，把 `tokenx` 替换成 `bun run cli --`。

## 支持的客户端

规范客户端身份列表在 `crates/tokenx-engine/client-catalog.json`。完整本地来源细节见
[支持的客户端](docs/clients.md)。

当前 catalog 包括：

OpenCode、Claude Code、Codex、Gemini CLI、Amp、Droid、OpenClaw、Pi、OMP、Kimi、Qwen CLI、Roo Code、Mux、Kilo、Hermes Agent、Copilot、Goose、Codebuff、CodeBuddy、Antigravity、Zed Agent、ZCode、Kiro、Junie、Warp、Cline、Command Code 和 Grok Build。

部分 catalog 条目有明确边界：

- `grok` 和本地 `warp.sqlite` 只提供没有 bucket 拆分的 token 总数，因此 Tokenx 使用 ADR 0010 定义的固定 bucket 分配。
- `commandcode` 是基于 transcript 的估算用量，不是供应商权威 token 记账。
- `antigravity` 通过已注册 integration 直接读取当前 AGY CLI 的 SQLite/WAL 数据
  （ADR 0007）。

## 数据和定价语义

本地报表只有一种成本含义：把解析出的 token bucket 套用 Tokenx 定价服务后得到的估算价格。普通本地报表会忽略应用自己上报的成本字段，因为那些字段可能代表订阅、积分、套餐余额、渠道加价、四舍五入后的 UI 总额或聚合花费。

`custom-pricing.json` 里的精确自定义覆盖会最先检查。否则，Tokenx
只用规范模型 ID 的精确匹配或 provider-scoped 模型 ID 的精确匹配搜索
LiteLLM、OpenRouter 和 models.dev；不会按前缀、子串或模糊匹配猜价格。

如果模型无法定价，派生成本保持 `$0.00`，不会使用私有猜测价格。细节见
[定价语义](docs/pricing.md)。

## 文档

- [支持的客户端和数据位置](docs/clients.md)
- [CLI 用法](docs/cli.md)
- [配置](docs/configuration.md)
- [定价语义](docs/pricing.md)
- [开发和测试](docs/development.md)
- [架构决策](docs/adr/)

## 许可和署名

Tokenx 起源于 Junho Yeo 的
[Tokscale](https://github.com/junhoyeo/tokscale)。

这个项目仍按 MIT License 发布。见 [LICENSE](LICENSE)。
