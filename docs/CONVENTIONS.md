# rex 约定 —— idax / bnx 共同遵守的逆向 CLI 契约

> 版本：**v0.7（草案）**　｜　编制：2026-10-24　｜　v0.7：精简——prelude 去掉 helper、名字解析与 `main()` 约定，只留执行器；去掉 `--keep`、`open --no-wait`、退出码 2（先等分析完成再执行）、`--truncate`、列表的 JSON（`--json` 只属于 exec）、`unsaved` 跟踪；会话行只剩 `target \| analysis \| idle`；无输出写 `# no output`；v0.6：exec 的代码就是位置参数（`-f FILE` 读文件，去掉脚本参数）；help 的排版与 `help` 子命令；每个报错都带 hint（§6）；零结果行；名字找不到时给事实 hint；NameError 查模块属性；v0.5：macOS 支持（安装目录定位、按平台的参考路径、预编译 fixture）；v0.4：语料只放工具特有内容，引擎 API 指向安装目录；skill 只交出不安装；名字解析与跨 thunk 的 xrefs；共用 fixture；v0.3：skill 的内容约定与示例测试（§5）；v0.2：idax / bnx 合并到 rex 之后的形态（命令树、会话与 worker、save、exec 参数与结果语义、取消、ref 语料）
> 适用：`idax`（IDA）、`bnx`（Binary Ninja）。
> 本文建立在 axkit 约定之上：输出格式、截断、JSON、错误与退出码、进度、daemon、agent 识别都按 axkit 的 `docs/CONVENTIONS.md`（下文写作 **AX §n**），这里只写逆向工具共有、与 agent 无关的部分。
> **必须** / **应当** / **〔待定〕** 的含义同 axkit。

---

## 0. 定位

- **少命令 + 写脚本。** 工具面由「生命周期命令 + `exec` + `ref`」构成，长尾能力交给脚本，不为每个查询做一个命令。
- **rex 是 idax / bnx 的共同部分**：命令树与全局 flag、会话表与 worker 进程、保存、exec 的参数与结果渲染、`ref` 检索、分析完成度。各工具只提供引擎后端（worker 里调 idalib / BN SDK）、Python prelude（只是执行器）、`ref` 语料与帮助文本。
- **不替模型写 API。** 脚本直接用引擎的原生 API；工具不包 helper、不做名字解析，不确定 API 时去 `ref` 和安装目录里查。helper 是模型要另学的一套名字，而模型多半已经会原生 API；哪条确实需要补回，由 eval 的数据决定。
- **rex 不依赖任何引擎 SDK，不含任何脚本运行时。**

### 0.1 引擎版本

**只支持各引擎最新的 stable release，不做旧版本兼容。** 新的 stable 发布后，工具跟进升级并直接放弃上一个版本；不支持 beta。

| 工具 | 引擎 | 当前目标版本 |
|---|---|---|
| idax | IDA Pro（idalib） | **9.4**（2026-07-13；9.5 仍是 beta） |
| bnx | Binary Ninja（官方 Rust API） | **6.0 R2**，`stable/6.0.10905`（2026-10-20） |

- 构建只针对一个 SDK 版本，不保留多 SDK feature、多份 manifest 或条件编译。
- 运行时检查加载到的引擎版本，与构建时不符就报 `engine_error` 并给出 hint，不尝试降级运行。

---

## 1. 命令面

```
<tool> <command> [<target>] [args...] [flags...]
```

| 规则 | 级别 |
|---|---|
| 需要数据库的命令，`<target>` 是**第一个位置参数**（文件路径：原始二进制或引擎的数据库文件） | 必须 |
| `<target>` 按客户端 cwd 解析、规范化（绝对路径 + 解析符号链接）后作为会话键；**同一个规范路径永远对应同一个会话** | 必须 |
| 计数参数（`--limit`、`--max-bytes` 等）接受十进制和 `0x` 十六进制 | 必须 |
| 访问数据库的命令在会话不存在时**自动打开**，不要求 agent 先 `open` | 必须 |

最小命令集（两个工具都必须有，语义相同）：

| 命令 | 作用 |
|---|---|
| `open <target>` | 打开或接管会话，等分析完成 |
| `status [<target>]` | 会话状态：分析进度、空闲时长（正在执行时是 `busy: <what> for <n>s`） |
| `list` | 列出当前用户的全部会话 |
| `save <target>` | 落盘；stdout 写 `saved \| <最终路径>` |
| `close <target>` | 关闭会话（不保存；要保存先 `save`） |
| `exec <target> …` | 执行脚本，见 §4 |
| `ref <query>` | 检索工具自己的说明；无参数时另列引擎安装里的 API 参考，见 §5 |

加上 AX §1 的管理命令（`daemon`、`skills`）。

全局 flag 两个工具相同：`--offset`、`--limit`、`--max-bytes`、`--full`、`-o`、`--timeout SECS`（0 为不限）、`--agent` / `--no-agent`。环境变量只有 `<TOOL>_MAX_BYTES` 与 `<TOOL>_TIMEOUT`。`--json` 只属于 `exec`。

| 规则 | 级别 |
|---|---|
| 不存在的 target 报 `not_found`，hint 说明路径按哪个目录解析 | 必须 |
| 对没有会话的 target 执行 `status` / `save`：`not_found`，hint 给出 `<tool> open <target>` | 必须 |
| `close` 没有会话的 target 不算错误：输出 `# no session for …`，退出 0 | 必须 |

---

## 2. 输出补充

在 AX §3 之上：

| 规则 | 级别 |
|---|---|
| 地址统一 `0x` 小写十六进制，不补零 | 必须 |
| 名字按引擎原样给出，不做 demangle 之外的改写 | 应当 |
| 多行的单条记录（反编译、反汇编块）以 `# <kind> <name> @ <addr>` 开头，块内原样 | 必须 |
| `open` 与 `exec` 先等自动分析完成再返回或执行（与 ida-mcp 一致），所以脚本看到的总是分析完的库；万一脚本开始时分析仍未完成，文本输出开头写一行 `# analysis incomplete (62%)`，`--json` 带 `"analysis":{"complete":false,"progress":0.62}`，退出码不变 | 必须 |
| 会话行：`# target \| analysis \| idle`。`open` / `status` 输出一行，`list` 每会话一行 | 必须 |

hint 的典型内容（AX §6.3）：最接近的属性名、「你传的是数据库旁边的原始文件」、要粘贴的下一条命令。

---

## 3. 会话与保存

- **除非显式 `save`，否则不落盘。** `close`、daemon 空闲退出、worker 崩溃都不会保存。
- 会话住在 axkit 的 daemon 里（AX §8），会话键是 §1 的规范路径。daemon 不可达时命令**失败**（`target_unavailable`），不退回本进程执行：那样会悄无声息地丢掉会话（AX §8）。
- **每个会话一个 worker 子进程**（`<tool> __worker <launch JSON>`）：崩溃、超时只作废这一个会话。会话有私有工作目录 `<cache>/<tool>/sessions/<daemon pid>-<n>`，旁边是 worker 的日志；关闭会话或丢失 worker 时删掉工作目录，下一个 daemon 启动时清理已退出 daemon 留下的目录。
- target 是引擎的数据库文件时，先复制进工作目录再打开副本，所以 `save` 之前 target 旁边什么都不会变。
- worker 崩溃或被杀：该会话作废，报 `session_lost`（退出码 3），hint 给出重新打开的命令和 worker 日志；其他会话不受影响，下一条命令按需重新打开。
- 超时（`--timeout`）：杀掉 worker，报 `timeout`（退出码 3）。
- 客户端断开（Ctrl-C）：daemon 向 worker 发取消，引擎在脚本里抛出 `KeyboardInterrupt`，会话保留；10 秒宽限期内没有停下才杀掉 worker（`session_lost`）。能否打断阻塞调用由引擎决定，各工具 README 写明。
- worker 自己没说进度时，daemon 在约 3 秒后补第一行 `# progress: <what>, <n>s elapsed`，之后每 5 秒一行（AX §7）；打开时 `<what>` 是 `opening <target> in <引擎>`，很快就打开的不打。
- 会话背后是自己起的 worker 之外，是否还能接管外部已有的实例（例如 idax 兼容 ida-nexus 的实例发现协议、连上 GUI 里开着的库），由引擎决定。

保存：

| 规则 | 级别 |
|---|---|
| 默认去处：二进制存到旁边的 `<name>.<ext>`（`fw.bin.i64`、`fw.bin.bndb`）；数据库覆盖它自己；`--to` 指定其他位置 | 必须 |
| 目标已存在、又不是本会话写的（本会话上次保存的文件和打开的数据库算本会话的）：拒绝，`usage`，hint 给出 `--force` | 必须 |
| 先写到目标所在目录的临时文件，再 rename 到目标；失败时原文件不变 | 必须 |

---

## 4. exec

```
<tool> exec <target> '<code>'                 # 代码就是参数，多行也写在这一个参数里
<tool> exec <target> 'for f in …:
    print(f)'
<tool> exec <target> -c 'a = 1' -c 'a + 1'    # -c 与位置参数同义；多个 -c 按序以换行拼成一段
<tool> exec <target> -f script.py             # 文件
<tool> exec <target> - <<'EOF'                # 或 -f -：从客户端 stdin 读，随请求转发给 daemon（AX §8）
…
EOF
```

参数约定：代码、`-c`、`-f` 三者取一。代码是 target 之后的那个参数，和 `sqlite3 db 'SQL'`、`jq 'filter'` 一样。v0.6 之前那个位置是脚本路径，评测里模型按上面的先验把代码写进去，得到 `cannot read the script`，所以改成代码。没有脚本参数：exec 之后的 flag 都归工具，脚本里 `sys.argv` 是 `[<filename>]`，filename 是 `<exec>`、`<stdin>` 或文件路径。位置参数只是一个已存在的文件名（一个词、不含代码字符）时报 usage，hint 给出 `-f` 写法。

**分工**：CLI 层与脚本语言无关，由 rex 渲染；脚本语言层由引擎在自己的解释器里实现。引擎把一次执行的结果（stdout、stderr、结果值、错误）交给 rex，由 rex 统一渲染。

CLI 层（rex 渲染，conformance 校验）：

| 规则 | 级别 |
|---|---|
| 输出顺序：先是脚本的 stdout（每行一条记录，受 AX §4 窗口约束），再是结果的渲染 | 必须 |
| 结果渲染：字符串原样作为一条记录；其他标量直接输出；数组每个元素一条记录；元素是对象时先输出一行 `# key \| key` 表头，每条记录按表头顺序取值用 ` \| ` 连接；对象每个键一条 `key \| value` 记录；更深的嵌套写成紧凑 JSON；`null` 不输出 | 必须 |
| `--json` 时 stdout 是一个对象：`{"stdout": …, "result": …, "analysis": {…}}`，`analysis` 总是出现；不受窗口截断（AX §5） | 必须 |
| 脚本的 stderr 原样写到 stderr | 必须 |
| 脚本异常：退出码 1，`kind = script_error`；message 是「异常类型: 消息」，有位置时附 ` (at <位置>)`；`hint:` 是改名建议，没有时指向 traceback 里的那一行（§6）。异常之前脚本已经打印的 stdout / stderr 照常输出 | 必须 |
| 无输出：没有错误、stdout 为空、没有结果值（`None`）时输出一行注释 `# no output`，表示脚本跑完了但什么都没输出，不暗示「结果为空」；空的 `[]` / `{}` 原样输出；`--json` 不变 | 必须 |
| `--timeout SECS`：超时后杀掉 worker，退出码 3；exec 默认 600 秒，0 为不限 | 必须 |
| 每次 exec 用新的命名空间：上一次的变量不会留到下一次 | 必须 |

脚本语言层（idax / bnx 各自的 Python prelude，只是执行器）：

| 规则 | 级别 |
|---|---|
| 预置：引擎有代表整个数据库的对象就绑定它（BN：`bv`）；没有就不绑定，不硬造（IDA：预先导入常用的原生 IDAPython 模块）。不预置 helper | 必须 |
| 结果：最后一个表达式的值；没有则只输出 stdout | 必须 |
| 结果值转成 JSON；转不了的（引擎对象等）用 `str()`，不报错也不另加说明 | 必须 |
| traceback 只保留用户代码帧，引用源码行，标题为 `Traceback (most recent call last, user code only):` | 必须 |
| `AttributeError` / `NameError`：对 `dir()` 做近似匹配，给出候选；对模块同样有效（`ida_funcs.get_fnc` → `get_func`）。`NameError` 还在预置的模块（BN 还有 `bv`）里找同名或近似的属性：`get_bytes` → `ida_bytes.get_bytes` | 必须 |
| 取消时脚本里抛出 `KeyboardInterrupt`，作为普通异常回答 | 应当 |

---

## 5. ref 与 skill

**语料只放工具特有的内容，引擎 API 指向安装目录。**

- `ref` 走 AX §9 的检索机制。语料是手写的、工具自己的东西：exec 的结果语义、会话与保存、headless 下的坑。引擎的完整 API 参考与官方示例**不**打包：用 idax 必然装了 IDA，用 bnx 必然装了 Binary Ninja，安装里的文件与运行版本精确对应，比任何手抄的语料都准。
- 不带查询的 `ref` 在主题列表之后列出引擎安装里的参考位置（`# path | holds | try`）。安装目录取运行时实际加载的引擎共享库所在目录（`Tool.docs`：idax 是 `libidalib`，bnx 是 `libbinaryninjacore`；Linux 用 `dl_iterate_phdr`，macOS 用 dyld 的映像表），与 worker 用的是同一份；每个参考位置可以给出多个候选相对路径，按平台布局取第一个存在的（BN 的 macOS app 把 Python 放在 `../Resources/`），显示前折叠掉 `..`；找不到时退到 `IDADIR` / `BINARYNINJADIR`，再找不到就一行 `hint:` 说明，退出码不变，`ref` 仍不需要会话或 license。
  - idax：`<IDADIR>/python`（`ida_*.py`、`idautils.py`、`idc.py` 的签名与 docstring）、`<IDADIR>/python/examples`（官方示例，`index.md` 是目录）。
  - bnx：`<BNDIR>/python/binaryninja`、`<BNDIR>/examples/python`。`api-docs/` 是几十 MB 的 HTML，不推荐给 agent，不列。
  - 每个位置附一两条可直接粘贴的 grep，由测试实际执行并断言有输出。
- 带词的 `ref` 先给语料里包含全部查询词的条目（最多 3 条），再检索安装里的 Python API（`rex::pyapi`）。这份 API 检索每次查询都直接读 `.py`，不打包、不缓存（daemon 进程内只读一次）。
  - 索引范围：模块函数、类、方法、属性，以及有文档的大写常量。
  - 匹配与排序：按下划线、点、驼峰拆词。名字命中的权重远高于 docstring 命中。
  - 每条输出一行：`[class |property |const ]限定名(参数) -> 返回 | 绝对路径:行号 | 摘要`。查询正好是限定名或唯一的名字时，附完整 docstring（最多 40 行）。
  - 官方示例最多 3 条：查询里有标识符时按示例调用它的次数排，否则按文件名和头部说明排。
  - 限定名：IDA 用模块名（`ida_funcs.get_func`）；BN 对 `__init__.py` 星号导入的模块用顶层名（`binaryninja.BinaryView.get_functions_at`）。`Tool.docs.naming` 指定用哪种，`Place.role` 指明哪个位置是 API、哪个是示例。
- `<tool> mcp` 是 stdio MCP server（AX §9.2），只有五个工具：`exec {target, code, timeout?}`、`ref {query}`、`save {target, path?, force?}`、`close {target}`、`list {}`。每个调用就是对应的那条命令行加 `--agent`，在客户端进程里转发给 daemon。
- 语料格式：每个主题一份 markdown，首行 `# <topic>: <about>`，每条 `## <title>`，内容是一行说明加最短示例（缩进 4 格）。不带查询列出主题；查询正好是主题名时给出整个主题；其他按词频打分（标题中的词权重 ×3），默认最多 10 条。两个工具用同一组主题：`exec`、`session`、`pitfalls`。
- 缩进的示例行以工具名开头的是命令，否则是 Python，连续几行合成一段 `<tool> exec fw.bin '…'`；行尾 `# exit N` 标注预期退出码。每条都必须在目标引擎版本上跑通，且不触发 `DeprecationWarning`，由工具的测试用 `rex::example::examples` 抽出后逐条执行。
- skill 的**不可违反的规则**至少包含：先读后改、改完回读、自己打开的会话要自己关、要保留修改必须显式 `save`。
- skill 放在工具仓库的 `skills/<tool>/SKILL.md`，由 `Tool::skill` 打包，`<tool> skills` 原样输出（AX §9.1），frontmatter 的 `metadata.version` 等于工具版本。工具不安装 skill。More 节写明完整 API 与官方示例在安装目录，用 `<tool> ref` 看路径。
- 两个工具的 skill 结构、措辞和示例风格一致，只在引擎差异处不同：原生 API、`bv`、数据库扩展名。
- skill 与语料用固定的示例 target `fw.bin`，即 `rex/fixtures/fw.c` 编出的 AArch64 程序（反编译示例需要 ARM 的 Hex-Rays）。有 `aarch64-linux-gnu-gcc` 时现编；没有时（macOS）拷 `rex/fixtures/bin/` 的预编译产物，那里存着编译所用的源码，与当前源码不一致就跳过并提示 `make -C rex/fixtures prebuilt`。两个引擎、两个平台由此看到同一个二进制。skill 的示例出现在代码块里以工具名开头的命令（续行的引号与 `<<'EOF'` 跟着走），以及表头含 `code` 列的表格（每格是 `<tool> exec fw.bin -c '…'` 的代码）。

---

## 6. 报错与 help

AX §6 之上的两条：

- **每个报错都带 `hint:`**：非 usage 的报错给出下一步（能粘贴的命令、要看的文件、引擎给的事实）。usage 报错保留 clap 的提示和用法行，命令不认识时列出全部命令，最后是能粘贴的 `` `<tool> <command> --help` ``。worker 没给 hint 的引擎失败，hint 指向 worker 的日志。由 rex 的源码检查守住（R-13）：rex 与 axkit 里每个 `Failure::new(...)` 都紧跟 `.with_hint(`。
- **help 先给 agent 要的**：顶层 `--help` 依次是一句话说明、命令、示例、退出码、skill 提示，`--help | head -30` 看得到示例。全局 flag 不逐条展开，只在各命令的 help 末尾用一块列出，写真实的 `<TOOL>_*` 变量名。子命令 help 只列自己的参数。`<tool> help` 与 `<tool> help <command>` 等同 `--help`。

---

## 附：conformance 断言清单（初版）

| ID | 断言 | 对应章节 |
|---|---|---|
| R-01 | 同一文件的不同写法（相对、绝对、符号链接）落到同一个会话 | §1 |
| R-02 | 计数参数接受十进制与 `0x` | §1 |
| R-03 | 未 `save` 的修改在 `close` 后不在磁盘上 | §3 |
| R-04 | exec 先输出脚本 stdout、再输出结果；结果的渲染符合 §4 | §4 |
| R-05 | 脚本异常以 1 退出，`kind = script_error` | §4 |
| R-06 | `open` / `exec` 等分析完成后才返回或执行 | §2 |
| R-07 | 加载到的引擎版本与构建时不符时报 `engine_error` | §0.1 |
| R-08 | 已存在、非本会话写的文件，`save` 不加 `--force` 时拒绝覆盖；保存后 target 旁边只多出数据库文件 | §3 |
| R-09 | 客户端断开后脚本被中断，会话保留 | §3 |
| R-10 | `ref` 语料的每条示例都能跑通；无参数的 `ref` 列出的每条 grep 都有输出 | §5 |
| R-11 | skill 的每条示例（代码块与 `code` 列）都按标注的退出码跑通 | §5 |
| R-13 | 每个报错都带 `hint:`；usage 报错以能粘贴的 `--help` 命令结尾 | §6 |
| R-14 | `exec <target> '<多行代码>'` 直接执行；`-f FILE` 读文件；多个 `-c` 拼接；只写了文件名时报 usage 并给出 `-f` | §4 |
| R-15 | 什么都没输出的 exec 输出 `# no output`；`--json` 时仍是一个对象 | §4 |
| R-16 | 顶层 `--help` 的示例在前 30 行内；`help <command>` 等同 `<command> --help` | §6 |
