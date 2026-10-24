# rex

`idax`（IDA）与 `bnx`（Binary Ninja）的共同部分：命令行、会话与 worker 进程、`save`、`exec` 与 `ref` 的渲染、分析完成度。契约写在 [`docs/CONVENTIONS.md`](docs/CONVENTIONS.md)，建立在 [axkit](../axkit) 的约定之上：agent 相关的输出、截断、错误、daemon 在 axkit，逆向工具共有的部分在这里。

一个工具用 rex 只需要三样东西：一个 `Tool` 描述（名字、引擎名、数据库扩展名、帮助文本、`ref` 语料、引擎安装里的文档位置、skill），一个实现 `worker::Backend` 的引擎后端，以及 `main` 里一行 `rex::tool::main::<Backend>(&TOOL)`。

rex 不依赖任何引擎 SDK，不含任何脚本运行时；Python prelude 与 `ref` 语料属于各引擎。`fixtures/fw.c` 是两个工具的 skill 与语料示例共用的测试程序（`fw.bin`）。

**引擎版本**：只支持最新 stable release —— IDA 9.4、Binary Ninja 6.0 R2（`stable/6.0.10905`）。不做旧版本兼容，不支持 beta。

## 模块

| 模块 | 内容 | 约定 |
|---|---|---|
| `tool` | `Tool` 描述、`main`（worker / daemon 命令 / 转发 / 本进程执行的分流）、`Rex`：axkit `Handler` 的实现，即全部命令 | §1–§5 |
| `cli` | clap 命令树（运行时套上工具的名字、版本、帮助）、`Global` 全局 flag、`Shape`（flag → 客户端的 `<TOOL>_MAX_BYTES` / `<TOOL>_TIMEOUT` → 读者默认值）、`ExecArgs::source`、`parse_count`、`shell_word` | §1、§4 |
| `session` | daemon 侧：会话表、每会话一个 worker 子进程、超时 kill、取消后的宽限期、心跳进度、丢失时的 `Failure`、私有工作目录与过期清理、数据库类 target 先复制再打开 | §3 |
| `worker` | worker 侧：`Backend` trait、`serve` 循环、协议 fd 的接管、`Channel`（进度、取消标志、`interruptible`） | §3 |
| `proto` | daemon 与 worker 之间的 JSON 行协议：`Launch`、`Down`、`Up`、`Request`、`Reply`、`State`、`WorkerFailure` | §3 |
| `save` | `Databases`（扩展名、默认位置）、`plan`（拒绝覆盖非本会话写的文件）、`Save::commit`（临时文件 → rename） | §3 |
| `exec` | `ScriptRun` / `ScriptError`（prelude 答案的反序列化），连同分析完成度渲染到 axkit 的 `Sink` 与 stderr | §4 |
| `reference` | `ref` 语料（每主题一份 markdown）与检索；`Docs`：从运行时加载的引擎库找到安装目录，列出其中的 API 参考与官方示例 | §5 |
| `pyapi` | `ref <words>` 的 API 检索：逐行解析安装里的 `.py`，得到签名、docstring 和 `文件:行号`，并按名字优先排序；也检索官方示例 | §5 |
| `mcp` | `<tool> mcp`：五个工具（exec、ref、save、close、list），每个调用转成一条命令行，走 CLI 的同一条路径 | §5 |
| `skills` | `skills [show [FILE] \| list]`：把打包的 skill 原样输出，不截断，在客户端执行；不安装 | §5 |
| `example` | 从 skill 与 `ref` 语料里抽出可执行示例（代码块命令、缩进 4 格的命令或 Python、`code` 列），给工具的测试逐条跑 | §5 |
| `analysis` | `Analysis`：文本首行、JSON 字段、表格单元、退出码 | §2 |

## 命令与全局 flag（两个工具相同）

| 命令 | 作用 |
|---|---|
| `open <target>` | 打开或接管会话，等分析结束 |
| `status [<target>]` | 一个会话（向空闲 worker 要最新状态）：`target \| analysis \| idle`；不带 target 等于 `list` |
| `list` | 全部会话 |
| `save <target> [--to PATH] [--force]` | 落盘，stdout 写 `saved \| <路径>` |
| `close <target>` | 关闭会话，不保存 |
| `exec <target> (CODE \| -c CODE... \| -f FILE \| -) [--json]` | 执行 Python：代码就是参数，多行写在一个参数里；会话不存在时自动打开，先等分析完成 |
| `ref [QUERY...]` | 无参数：列出主题，以及安装里 API 参考与官方示例的位置和 grep 命令；带词：先给工具自己的说明，再检索安装里的 Python API 签名和官方示例 |
| `mcp` | stdio MCP server：exec、ref、save、close、list |
| `daemon serve\|status\|stop [--force]` | 管理 daemon |
| `skills [show [FILE] \| list]` | 打出打包的 agent skill；由 agent 装到自己 harness 的 skill 目录 |

全局 flag（不在各命令的 flag 列表里逐条展开，而是在 help 末尾列成一块，见约定 §6）：`--offset`、`--limit`、`--max-bytes`、`--full`、`-o FILE`、`--timeout SECS`（0 为不限；exec 默认 600，其余不限）、`--agent` / `--no-agent`。环境变量只有 `<TOOL>_MAX_BYTES` 与 `<TOOL>_TIMEOUT`，从客户端转发，不读 daemon 自己的环境。

`help [<command>]` 等同 `[<command>] --help`。每个报错都带 `hint:`，usage 报错以能粘贴的 `<tool> <command> --help` 结尾（约定 §6，`tests/hints.rs` 对 rex 与 axkit 的源码做检查）。

## 待做

- ~~生命周期命令的公共骨架与会话表~~（`tool`、`session`、`worker`）
- conformance 测试 helper（R-01 ～ R-09），目前由两个工具的集成测试各自覆盖
- `ref` 换成 BM25（随 axkit 的文档检索）
- ~~skill~~（`skills`、`example`）

## 开发

```sh
cargo clippy --all-targets
cargo test
cargo fmt --check
```

axkit 以 `path = "../axkit"` 依赖，两个目录需并列放置。

## 许可

Apache-2.0。
