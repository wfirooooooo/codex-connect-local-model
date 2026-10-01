# codex-local

一条命令启动本地模型并进入 Codex：起 llama.cpp、起进程内转换层、再拉起 Codex。

完整说明见 [MANUAL.md](MANUAL.md)。

English: [README.en.md](README.en.md) / [MANUAL.en.md](MANUAL.en.md)

## 构建

在仓库根目录执行：

```bash
cargo build --release
cp target/release/codex-local bin/
```

## 用法

```bash
./bin/codex-local                                       # 不指定模型：列出 models/ 下可用的模型
./bin/codex-local -m gemma-3-27b                        # 指定模型（简写/文件名/路径）
./bin/codex-local -m gemma-3-27b exec "run the tests"   # 其余参数透传给 codex
./bin/codex-local -l                                    # 列出 models/ 下的 .gguf
```

选项：

```
-m, --model <路径|简写>   不指定则列出 models/ 下的模型并退出
    --alias <名字>        llama.cpp 模型别名（默认文件名第一个 '-' 之前）
    --llama-port <端口>   默认 8001
    --shim-port <端口>    默认 8010
    --cd <目录>          让 Codex 以该目录作为工作根（映射到 codex 的 -C）
    --ctx-size <n>        透传给 llama-server
    --n-gpu-layers <n>    透传给 llama-server
    --stop-after          Codex 退出后关闭本次启动的模型
-l, --list
```

## 目录

- `src/` 源码（main/llama/shim/codex 四个模块）
- `bin/` 构建产物 `codex-local`
- `models/` 模型文件（当前是指向原 gguf 的软链）
- `logs/` `llama_server.log`
- `run/` 运行期 pid 文件
- `config/` 可选（暂未使用，留给 codex profile）

## 行为说明

- 模型身份用 `/props` 的 `model_path` 绝对路径判断，不用别名，避免同名别名误判。
- 默认 Codex 退出后 llama.cpp 继续在后台运行；`--stop-after` 则关闭本次启动的实例。转换层随本进程退出，无需单独管理。
- 换模型时只终止「本工具启动且 pid 文件能对上」的实例；陌生进程会拒绝操作并提示手动停止。

## 关于「Codex 信任的目录」

Codex 判断的是工作目录本身，`~/.codex/config.toml` 里的 `[projects] trust_level` 不能替代这个判断：

```bash
./bin/codex-local --cd /path/to/project          # 指定工作根
./bin/codex-local --cd /path/to/project exec --skip-git-repo-check "..."   # 非 git 目录
```

不指定 `--cd` 时用的是当前目录。目录不在 git 仓库内且参数里带 `exec` 时，工具会提示需要 `--skip-git-repo-check`；若已带上该参数则不提示。

环境变量：`CODEX_LOCAL_ROOT`（项目根）、`LLAMA_SERVER_BIN`（llama-server 路径）、`LLAMA_WAIT`（就绪等待秒数，默认 180）。

## 许可

MIT License，见 [LICENSE](LICENSE)。
