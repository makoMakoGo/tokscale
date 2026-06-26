# Tokscale local-first fork

> 这是
> [junhoyeo/tokscale](https://github.com/junhoyeo/tokscale)
> 的独立维护 fork，重点是本地 AI 编码客户端用量统计、明确的数据语义，以及在大型 transcript
> 集合上的可预测资源占用。

> [!IMPORTANT]
> 这个仓库不是上游官方发布渠道，也不是上游的镜像。支持的客户端、成本语义和部分工作流会有意和上游不同。
>
> `npx tokscale@latest`、`bunx tokscale@latest` 以及 npm 上的 `tokscale`
> 包安装的是上游发行版，不是这个分支的代码。要验证本 fork 的行为，请按下面的源码构建流程运行。

![Tokscale TUI overview](.github/assets/tui-overview.png)

## 这个 fork 是什么

Tokscale 会读取本地 AI 编码客户端状态，把带 token 信息的记录转换成 CLI 和 TUI 报表。这个 fork
保留上游的终端优先体验，同时收紧本地数据、客户端身份、定价和资源占用规则。

当前维护分支是 `personal/local-clients`。

## 为什么维护这个 fork

- **本地优先统计。** 本地报表只从带 token 的记录派生。供应商上报的花费、积分、余额和只有金额没有
  token 的行不会混进 token 成本。
- **行为显式。** 解析失败、缺失数据、未知客户端和无法匹配的价格保持可见，不用猜测别名或假成功路径掩盖。
- **稳定客户端身份。** 客户端 id、展示信息和前端 registry 由
  `crates/tokscale-core/client-catalog.json` 统一定义。
- **共享聚合语义。** CLI 和 TUI 应该描述同一套本地用量，而不是对同一批 transcript 做两套解释。
- **更低内存占用。** 消息管线避免不必要的 clone，并在源文件没有变化时跳过完整 reload。
- **选择性吸收上游。** 上游修复会经过审查后选择性移植。这个 fork 不会自动接收每个上游客户端、托管功能或发布策略。

更多背景见 [fork 范围](docs/fork.md)、[维护者上下文](CONTEXT.md) 和
[架构决策](docs/adr/)。

## 构建这个 fork

前置要求：

- Bun
- 稳定 Rust 工具链

```bash
git clone --branch personal/local-clients --single-branch \
  https://github.com/makoMakoGo/tokscale.git

cd tokscale
bun install
bun run build:core
```

运行本地 wrapper：

```bash
# 打开交互式 TUI
bun run cli

# 适合脚本的报表
bun run cli -- --no-spinner --light

# 查看检测到的客户端和扫描位置
bun run cli -- clients
```

`bun run cli` 会通过 `packages/cli` 执行当前 checkout 中的代码。npm 上名为 `tokscale` 的公开包仍然是上游包。

## 常用命令

```bash
# TUI
tokscale
tokscale tui
tokscale models
tokscale monthly
tokscale hourly

# 脚本化报表
tokscale --no-spinner --light
tokscale models --no-spinner --json
tokscale graph --no-spinner --output graph.json

# 过滤
tokscale --client opencode,claude --week
tokscale models --since 2026-01-01 --until 2026-01-31
tokscale models --group-by client,provider,model --json

# 查询价格目录
tokscale pricing claude-sonnet-4-5 --no-spinner
tokscale pricing list-overrides --json
```

从源码运行时，把 `tokscale` 替换成 `bun run cli --`。

## 支持的客户端

规范客户端身份列表在 `crates/tokscale-core/client-catalog.json`。完整本地来源细节见
[支持的客户端](docs/clients.md)。

当前 catalog 包括：

OpenCode、Claude Code、Codex CLI、Cursor、Gemini CLI、Amp、Droid、OpenClaw、Pi、OMP、Kimi、Qwen CLI、Roo Code、KiloCode、Mux、Kilo CLI、Crush、Hermes Agent、Copilot、Goose、Codebuff、Antigravity、Zed Agent、ZCode、Kiro、Junie、Trae、Warp、Cline、Command Code 和 Grok Build。

部分 catalog 条目有明确边界：

- `crush` 和 `warp` 不参与普通本地 token 报表，因为它们没有被这个 fork 接受的 token 级来源。
- `commandcode` 是基于 transcript 的估算用量，不是供应商权威 token 记账。
- `cursor` 读取本地 API 缓存。已登录时，如果没有使用 `--home`，且 Cursor 在客户端范围内，并且缓存超过五分钟，普通本地报表和 TUI 可以自动刷新过期缓存；`tokscale cursor sync` 用于强制刷新。
- `antigravity` 和 `trae` 使用显式 sync 命令刷新的本地缓存。

## 数据和定价语义

本地报表只有一种成本含义：把解析出的 token bucket 套用 Tokscale 定价服务后得到的估算价格。普通本地报表会忽略应用自己上报的成本字段，因为那些字段可能代表订阅、积分、套餐余额、渠道加价、四舍五入后的 UI 总额或聚合花费。

`custom-pricing.json` 里的精确自定义覆盖会最先检查。否则，Tokscale 会用 provider-aware 的精确匹配和确定性规范化匹配搜索 LiteLLM、OpenRouter 和 models.dev。三个公开目录之间没有简单固定的全局优先级；规范化是匹配策略，不是单独的价格来源。

如果模型无法定价，派生成本保持 `$0.00`，不会使用私有猜测价格。细节见
[定价语义](docs/pricing.md)。

## 文档

- [Fork 范围和上游关系](docs/fork.md)
- [支持的客户端和数据位置](docs/clients.md)
- [CLI 用法](docs/cli.md)
- [配置](docs/configuration.md)
- [定价语义](docs/pricing.md)
- [开发和测试](docs/development.md)
- [架构决策](docs/adr/)
- [上游移植记录](docs/upstream/)

## 上游关系

这个仓库有意保留在 GitHub fork 网络中，以保留项目来源。`personal/local-clients` 分支按
content-ahead 变体维护：上游变更会经过审查并选择性移植，而不是整体合并。

不保证和上游行为兼容。上游官方包、托管服务、社区链接和文档请参考上游仓库。

## 许可和署名

基于 Junho Yeo 的 [Tokscale](https://github.com/junhoyeo/tokscale)。

这个 fork 仍按 MIT License 发布。见 [LICENSE](LICENSE)。
