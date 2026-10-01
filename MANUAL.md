# codex-local 使用手册

用一条命令，让 Codex CLI 跑在本机 llama.cpp 加载的模型上。

English version: [MANUAL.en.md](MANUAL.en.md)

## 1. 它解决什么问题

手动跑通一次「本地模型 + Codex」原本要三步：起 `llama-server`、起转换层、起 Codex。而且中间有两个坑：

一是 Codex 较新版本废弃了 `wire_api = "chat"`，自定义 provider 只能用 `responses`，而 llama.cpp 的 `/v1/responses` 只接受 `type = "function"` 的工具定义；Codex 每次请求还会附上 `web_search`、`namespace`（多智能体与 MCP 工具组）、`custom` 等类型，llama.cpp 会直接返回 400 `'type' of tool must be 'function'`。

二是判断「llama.cpp 现在装的是哪个模型」不能靠别名，因为别名取文件名第一个 `-` 之前的部分，`google_gemma-4-E4B-it-Q8_0.gguf` 和 `google_gemma-3-27b-it-Q4.gguf` 会撞成同一个名字。

`codex-local` 把三件事合进一个进程，用 `/props` 的 `model_path` 做模型身份判断，并在转发的路上剔掉 llama.cpp 不认的工具定义。

## 2. 构建

在仓库根目录执行：

```bash
cargo build --release
cp target/release/codex-local bin/
```

依赖 tokio / hyper / hyper-util / http-body-util / serde_json / clap / libc / anyhow。运行只需要 `bin/codex-local`，不依赖 node。

## 3. 快速开始

在仓库根目录执行：

```bash
./bin/codex-local
```

它会依次输出三个阶段，然后进入 Codex 交互界面：

```
[1/3] 启动 llama.cpp：google_gemma ...
      llama.cpp 就绪（PID 3250）
[2/3] 转换层已就绪 http://127.0.0.1:8010
[3/3] 启动 Codex（model=google_gemma）
```

在项目目录里用：

```bash
cd ~/code/my-project
/path/to/codex-connect-local-model/bin/codex-local
```

## 4. 命令参考

```
codex-local [选项] [codex 参数...]
```

| 选项 | 默认值 | 说明 |
| --- | --- | --- |
| `-m, --model <路径\|简写>` | `google_gemma-4-E4B-it-Q8_0.gguf` | 模型。支持绝对路径、文件名、简写 |
| `--alias <名字>` | 文件名第一个 `-` 之前 | llama.cpp 的模型别名，也是传给 Codex 的 model |
| `--llama-port <端口>` | `8001` | llama.cpp 监听端口 |
| `--shim-port <端口>` | `8010` | 进程内转换层监听端口 |
| `--cd <目录>` | 当前目录 | Codex 的工作根，映射到 codex 的 `-C` |
| `--ctx-size <n>` | 用 llama.cpp 默认 | 透传 `--ctx-size` 给 llama-server |
| `--n-gpu-layers <n>` | 用 llama.cpp 默认 | 透传 `--n-gpu-layers` 给 llama-server |
| `--stop-after` | 关 | Codex 退出后关闭「本次启动」的 llama.cpp |
| `-l, --list` | | 列出 `models/` 下所有 `.gguf` 后退出 |
| `-h, --help` / `-V, --version` | | 帮助与版本 |

`--ctx-size` 和 `--n-gpu-layers` 只在本次真的启动了 llama-server 时才生效；复用已在运行的实例时会被忽略（换这两个参数需要先停掉现有实例）。

### 模型名怎么写

解析顺序，命中即停：

1. 参数本身是存在的文件路径
2. `<项目根>/models/<参数>` 存在
3. 前缀匹配 `<参数>*.gguf`
4. 子串匹配 `*<参数>*.gguf`

第 3、4 步若命中多个文件，会列出候选并退出，不猜。

```bash
./bin/codex-local -m gemma-3-27b
./bin/codex-local -m google_gemma-4-E4B-it-Q8_0.gguf
./bin/codex-local -m /Volumes/models/qwen3-32b-Q4.gguf
```

### 参数怎么传给 codex

从第一个不属于本工具的选项开始，后面的内容原样交给 codex：

```bash
./bin/codex-local exec "run the tests"          # codex exec "run the tests"
./bin/codex-local -c model_reasoning_effort=low # codex -c model_reasoning_effort=low
./bin/codex-local -- "修一下这个 bug"            # -- 强制分隔
```

本工具自己的选项必须写在最前面，一旦出现不认识的参数，后面就全部归 codex。

## 5. 环境变量

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `CODEX_LOCAL_ROOT` | 可执行文件所在目录的上一级 | 项目根，决定 `models/`、`logs/`、`run/` 的位置 |
| `LLAMA_SERVER_BIN` | `llama-server` | llama-server 可执行文件位置或名字 |
| `LLAMA_WAIT` | `180` | 等待 llama.cpp 就绪的秒数 |

## 6. 目录结构

```
codex-connect-local-model/
├── src/            main.rs / llama.rs / shim.rs / codex.rs
├── bin/            codex-local（构建产物）
├── models/         .gguf 模型文件
├── logs/           llama_server.log、shim.log
├── run/            llama-server.pid
├── config/         预留
└── Cargo.toml
```

`models/` 放 `.gguf` 模型文件，可以是指向模型库的软链，也可以直接把文件放进来（`.gitignore` 已排除 `models/*.gguf`，模型不会进仓库）。`logs/shim.log` 会逐次记录被剔除的工具名，排查用。

## 7. 工作流程

第一阶段是模型。先探测 `http://127.0.0.1:<llama-port>`，用 `/props` 返回的 `model_path` 和目标文件（已做软链解析）比对：一致就直接复用；不一致且 pid 文件里的进程还活着，就发 SIGTERM 停掉再起新的；llama.cpp 没在跑就直接拉起。然后轮询 `/v1/models`，每秒一次，直到就绪或超时。

第二阶段是转换层。它跑在本进程内，绑定 `--shim-port`，把 `/v1/responses` 请求里非 `function` 类型的工具定义去掉后转发给 llama.cpp，其余请求原样透传，响应以流式回传，所以 SSE 不受影响。

第三阶段是 Codex。以子进程方式启动，注入这些配置：

```
-c model_provider=llama_cpp
-c model_providers.llama_cpp.base_url=http://127.0.0.1:<shim-port>/v1
-c model_providers.llama_cpp.wire_api=responses
-c model=<别名>
```

所以它不依赖 `~/.codex/llama.config.toml`，端口或模型变了也不用改配置文件。

进程模型上，`codex-local` 本身就是转换层，同时是 Codex 的父进程。Ctrl-C 由终端发给整个前台进程组，Codex 直接收到，父进程吞掉信号以免先于 Codex 退出；`kill <codex-local>` 只发给父进程，所以父进程会把 SIGTERM 转发给 Codex。Codex 退出后，父进程原样返回它的退出码，转换层随进程结束。

## 8. 常见任务

### 换一个本地模型

```bash
./bin/codex-local -m gemma-3-27b
```

工具会检测到当前加载的不是这个模型，停掉旧实例（仅限本工具启动过的）、加载新模型、等就绪，再进 Codex。别名随文件名自动变，不需要额外指定。

### 指定 Codex 的工作目录

```bash
./bin/codex-local --cd ~/code/my-project
```

不指定时用当前目录。

### 目录不是 git 仓库

Codex 的 `exec` 只在 git 仓库里运行，`~/.codex/config.toml` 里的 `[projects] trust_level = "trusted"` 不能替代这一点。提示出现时按给的命令加参数即可：

```bash
./bin/codex-local --cd ~/code/my-project exec --skip-git-repo-check "..."
```

目录不在 git 仓库内、参数里又带 `exec`、且没写 `--skip-git-repo-check` 时，工具会在启动前提示一次。

### 用完就关掉模型

```bash
./bin/codex-local --stop-after
```

默认 Codex 退出后 llama.cpp 继续在后台待命（加载 8GB 要几十秒，反复起停不划算），只打印停止命令。加 `--stop-after` 则只关闭「本次启动的」实例；如果这次是复用已有实例，则不会去动它。

### 手动停止后台的 llama.cpp

```bash
kill $(cat run/llama-server.pid)    # 在仓库根目录执行
```

## 9. 故障排查

| 现象 | 原因与处理 |
| --- | --- |
| `转换层端口 8010 无法监听（可能已被占用）` | 旧转换层或别的程序占着端口。`lsof -nP -iTCP:8010 -sTCP:LISTEN` 查出来停掉，或用 `--shim-port` 换端口 |
| `llama.cpp 未在 180 秒内就绪` | 看 `logs/llama_server.log`；模型大或磁盘慢时用 `LLAMA_WAIT=600` 放宽 |
| `无法启动 llama-server…` | `llama-server` 不在 PATH 里，用 `LLAMA_SERVER_BIN=/path/to/llama-server` 指定 |
| `无法启动 codex…` | `codex` 不在 PATH 里 |
| `Not inside a trusted directory…` | 目录不是 git 仓库，加 `--skip-git-repo-check`（见上一节） |
| `llama.cpp 正在运行但加载的是其它模型，且 pid 文件缺失` | 现有实例不是本工具启动的，工具拒绝擅自杀。手动停掉后再跑 |
| `无法停止正在运行的 llama.cpp` | 发了 SIGTERM 但 30 秒内没退出，手动处理后重试 |
| `Unknown model google_gemma is used…` | Codex 没有该模型的元数据，走 fallback，不影响使用 |
| Codex 报 `'type' of tool must be 'function'` | 说明请求没经过转换层，检查是不是绕开本工具直接连了 llama.cpp |

## 10. 与旧工具的关系

早期方案是几个独立脚本，已被 `codex-local` 取代。如果你本地还留着它们，可以不再使用：

| 旧组件 | 现在的对应 |
| --- | --- |
| `llama-local` 脚本 | 工具内部直接启动 llama-server |
| `codex-llama-ctl` 脚本 | 转换层随主进程启停，无需单独管理 |
| `codex-llama` 二进制 | 转换层内嵌进主进程 |
| `codex-llama-shim/`（含 Node 版） | 源码已迁到 `src/` |

## 11. 已知限制

被剔除的工具是真实代价：本地模型没有联网搜索、没有子智能体、也没有 `node_repl` / `cua_repl` 这类 MCP 工具，改文件只能走 `exec_command`（也就是 shell）。

性能参考（MacBook，Gemma 4 E4B Q8_0）：生成约 39 tok/s、预填约 122 tok/s，而 Codex 的系统提示约 9k tokens，所以每轮对话起步大概二三十秒。上下文按模型的 131072 计算。
