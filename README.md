# rex

Shared layer of idax and bnx: sessions and workers, exec, explicit save, ref search over the engine's installed Python API, and code-mode MCP.

rex 是 [idax](https://github.com/VibRev/idax)（IDA）和 [bnx](https://github.com/VibRev/bnx)（Binary Ninja）的共同部分：命令行、会话与 worker 进程、`save`、`exec` 和 `ref` 的渲染、安装目录里的 API 检索，以及 MCP。契约写在 [`docs/CONVENTIONS.md`](https://github.com/VibRev/rex/blob/main/docs/CONVENTIONS.md)，建立在 [axkit 的约定](https://github.com/VibRev/axkit/blob/main/docs/CONVENTIONS.md) 之上：输出、截断、错误、daemon 这些和 agent 相关的部分在 axkit，逆向工具共有的部分在这里。

## 获取与构建

rex 通过 `path = "../axkit"` 依赖 axkit，所以两个仓库要 clone 到同一个父目录下：

```sh
mkdir VibRev && cd VibRev
git clone https://github.com/VibRev/axkit
git clone https://github.com/VibRev/rex
cd rex && cargo test
```

- rex 不依赖任何引擎 SDK，也不含脚本运行时；Python prelude 和 `ref` 语料都在各引擎自己的仓库里。
- **引擎版本**：只支持最新的 stable release，即 IDA 9.4 和 Binary Ninja 6.0 R2（`stable/6.0.10905`）；不兼容旧版本，也不支持 beta。

## 接入一个工具

接入 rex 只需要三样东西：

- 一个 `Tool` 描述，包括名字、引擎名、数据库扩展名、帮助文本、`ref` 语料、引擎安装里的文档位置和 skill；
- 一个实现 `worker::Backend` 的引擎后端；
- `main` 里的一行 `rex::tool::main::<Backend>(&TOOL)`。

`fixtures/fw.c` 是两个工具测 skill 和 `ref` 示例时共用的程序，编出来是 AArch64 ELF `fw.bin`。`fixtures/bin/` 放着预编译好的产物和对应的源码，给没有交叉编译器的机器用。

## 命令与全局 flag（两个工具相同）

| 命令 | 作用 |
|---|---|
| `open <target>` | 打开会话或接管已有会话，等分析结束 |
| `status [<target>]` | 一个会话的状态：`target \| analysis \| idle`；不带 target 时等同于 `list` |
| `list` | 列出全部会话 |
| `exec <target> (CODE \| -c CODE... \| -f FILE \| -) [--json]` | 执行 Python。代码直接作为参数，多行代码也写在同一个参数里；会话不存在时会自动打开，并先等分析完成 |
| `save <target> [--to PATH] [--force]` | 落盘，stdout 输出 `saved \| <路径>` |
| `close <target>` | 关闭会话，不保存 |
| `ref [WORDS...]` | 不带参数时，列出主题，以及安装里 API 参考和官方示例的位置、对应的 grep 命令；带词时，检索工具自己的笔记，以及安装里的 Python API 签名和官方示例 |
| `mcp` | 启动 stdio MCP server，提供 exec、ref、save、close、list 五个工具 |
| `daemon serve\|status\|stop [--force]` | 管理 daemon |
| `skills [show [FILE] \| list]` | 打印打包在二进制里的 agent skill，由 agent 自己装到所在 harness 的 skill 目录 |

**全局 flag**：help 末尾统一列出，不在每个命令下重复。

- 输出范围：`--offset`、`--limit`、`--max-bytes`、`--full`
- 输出位置：`-o FILE`
- 超时：`--timeout SECS`，0 表示不限；exec 默认 600 秒，其他命令不限
- 模式：`--agent` / `--no-agent`

**环境变量**：只有 `<TOOL>_MAX_BYTES` 和 `<TOOL>_TIMEOUT`，由客户端转发给 daemon。

**帮助与报错**：

- `help [<command>]` 等同于 `[<command>] --help`。
- 每个报错都带 `hint:`；usage 报错的最后是一条能直接粘贴运行的 `<tool> <command> --help`。
- `tests/hints.rs` 扫描 rex 和 axkit 的源码，检查每处报错都带了 hint。

## 模块

| 模块 | 内容 | 约定 |
|---|---|---|
| `tool` | `Tool` 描述；`main`：在 worker、daemon 命令、转发给 daemon、本进程执行之间分流；`Rex`：实现 axkit 的 `Handler`，也就是全部命令 | §1–§5 |
| `cli` | clap 命令树（运行时填入工具的名字、版本和帮助）、全局 flag、`Shape`、`ExecArgs::source`、`parse_count`、`shell_word` | §1、§4 |
| `session` | daemon 侧：会话表，每个会话一个 worker 子进程；超时时 kill，取消后有宽限期，用心跳报进度；私有工作目录和过期清理；数据库类 target 先复制再打开 | §3 |
| `worker` | worker 侧：`Backend` trait、`serve` 循环、接管协议 fd、`Channel`（进度、取消标志、`interruptible`） | §3 |
| `proto` | daemon 和 worker 之间按行传的 JSON 协议 | §3 |
| `save` | `Databases`（数据库扩展名、默认保存位置）、`plan`（拒绝覆盖不是本会话写的文件）、`Save::commit`（先写临时文件再 rename） | §3 |
| `exec` | 把 prelude 返回的结果反序列化，渲染到 axkit 的 `Sink` 和 stderr | §4 |
| `reference` | `ref` 语料，每个主题一份 markdown；`Docs`：通过运行时加载的引擎库找到安装目录 | §5 |
| `pyapi` | 安装目录里的 API 检索：逐行解析 `.py`，取出签名、docstring 和 `文件:行号`，名字命中的排在前面；也检索官方示例 | §5 |
| `mcp` | `<tool> mcp`：把每个工具调用转成一条命令行，与 CLI 走同一条路径 | §5 |
| `skills` | 原样输出 skill，不截断，在客户端执行 | §5 |
| `example` | 从 skill 和 `ref` 语料里抽出可执行的示例，供工具的测试逐条运行 | §5 |
| `analysis` | 分析完成度 | §2 |

## 平台

Linux、macOS、Windows（10 1803 及以上，x86_64）。daemon 的传输层在 axkit 里。worker 接管协议通道：Unix 上重定向 fd，Windows 上复制出 stdin/stdout 的句柄，再把 C 运行时的 fd 0/1 和进程的标准句柄改到 `NUL` 与 stderr（引擎和 Python 往 stdout 打的东西不会混进协议）；Windows 上 worker 以 `CREATE_NO_WINDOW` 启动，不弹控制台窗口。引擎安装目录在 Windows 上靠 `EnumProcessModules` 找到已加载的 `idalib.dll` / `binaryninjacore.dll`。Windows 部分目前只做过交叉编译与 CI。

## 待做

- conformance 测试 helper（R-01～R-09）。目前由两个工具的集成测试各自覆盖。

## 开发

```sh
cargo clippy --all-targets
cargo test
cargo fmt --check
```

## 许可

Apache-2.0。
