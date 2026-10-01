# codex-local Manual

**English** | [中文](MANUAL.zh-CN.md)

Run Codex CLI against a model served by your local llama.cpp, with a single command.

## 1. What it solves

Getting "local model + Codex" running by hand takes three steps: start `llama-server`,
start a shim, start Codex. Two traps sit in the middle.

First, recent Codex versions removed `wire_api = "chat"` for custom providers — only
`responses` is accepted. But llama.cpp's `/v1/responses` only accepts tool definitions of
`type = "function"`, while Codex sends `web_search`, `namespace` (multi-agent and MCP tool
groups) and `custom` entries as well. llama.cpp answers 400
`'type' of tool must be 'function'`.

Second, you cannot tell which model llama.cpp has loaded by alias, because the alias is the
filename up to the first `-`. `google_gemma-4-E4B-it-Q8_0.gguf` and
`google_gemma-3-27b-it-Q4.gguf` both become `google_gemma`.

`codex-local` merges the three steps into one process, decides model identity from the
absolute `model_path` in `/props`, and strips the tool definitions llama.cpp rejects while
forwarding.

## 2. Build

Run these from the repository root:

```bash
cargo build --release
cp target/release/codex-local bin/
```

Depends on tokio / hyper / hyper-util / http-body-util / serde_json / clap / libc / anyhow.
Running it needs only `bin/codex-local`; no node required.

## 3. Quick start

Put a `.gguf` into `models/`, then run this from the repository root:

```bash
./bin/codex-local -m <model-name>
```

Not sure what is available? List first:

```bash
./bin/codex-local -l
```

It prints three stages and then hands over to Codex:

```
[1/3] starting llama.cpp: google_gemma ...
      llama.cpp ready (PID 3250)
[2/3] shim ready on http://127.0.0.1:8010
[3/3] launching Codex (model=google_gemma)
```

From inside a project:

```bash
cd ~/code/my-project
/path/to/codex-connect-local-model/bin/codex-local
```

## 4. Command reference

```
codex-local [options] [codex args...]
```

| Option | Default | Description |
| --- | --- | --- |
| `-m, --model <path\|shorthand>` | none | Model: absolute path, filename, or shorthand; lists available models when omitted |
| `--alias <name>` | filename before the first `-` | llama.cpp alias, also passed to Codex as its model |
| `--llama-port <port>` | `8001` | Port llama.cpp listens on |
| `--shim-port <port>` | `8010` | Port the in-process shim listens on |
| `--cd <dir>` | current directory | Working root for Codex (maps to codex's `-C`) |
| `--ctx-size <n>` | llama.cpp default | Forwards `--ctx-size` to llama-server |
| `--n-gpu-layers <n>` | llama.cpp default | Forwards `--n-gpu-layers` to llama-server |
| `--stop-after` | off | Stop the llama.cpp started *by this run* when Codex exits |
| `-l, --list` | | List every `.gguf` under `models/` and exit |
| `-h, --help` / `-V, --version` | | Help and version |

`--ctx-size` and `--n-gpu-layers` only apply when this run actually launches llama-server.
They are ignored when an already-loaded instance is reused — stop that instance first if you
want to change them.

### How a model name is resolved

First match wins:

1. the argument is an existing file path
2. `<root>/models/<argument>` exists
3. prefix match `<argument>*.gguf`
4. substring match `*<argument>*.gguf`

If steps 3 or 4 match more than one file, the tool lists the candidates and exits instead of
guessing.

Omitting `-m` does not guess either: the tool lists every `.gguf` under `models/` and exits
non-zero. An empty `models/` reports that there are no model files.

```bash
./bin/codex-local -m gemma-3-27b
./bin/codex-local -m google_gemma-4-E4B-it-Q8_0.gguf
./bin/codex-local -m /Volumes/models/qwen3-32b-Q4.gguf
```

### Passing arguments to codex

From the first argument this tool does not recognize, everything is handed to codex verbatim:

```bash
./bin/codex-local exec "run the tests"          # codex exec "run the tests"
./bin/codex-local -c model_reasoning_effort=low # codex -c model_reasoning_effort=low
./bin/codex-local -- "fix this bug"             # -- forces the split
```

The tool's own options must come first. Once an unknown argument appears, everything after it
belongs to codex.

## 5. Environment variables

| Variable | Default | Description |
| --- | --- | --- |
| `CODEX_LOCAL_ROOT` | parent of the executable's directory | Project root; decides where `models/`, `logs/` and `run/` live |
| `LLAMA_SERVER_BIN` | `llama-server` | Path or name of the llama-server executable |
| `LLAMA_WAIT` | `180` | Seconds to wait for llama.cpp to become ready |

## 6. Layout

```
codex-connect-local-model/
├── src/            main.rs / llama.rs / shim.rs / codex.rs
├── bin/            codex-local (build output)
├── models/         .gguf model files
├── logs/           llama_server.log, shim.log
├── run/            llama-server.pid
├── config/         reserved
└── Cargo.toml
```

`models/` is where `.gguf` files live — either symlinks into a model store or the files
themselves (`.gitignore` excludes `models/*.gguf`, so models never enter the repository).
`logs/shim.log` records the tool names dropped on each request, which is handy when
debugging.

## 7. How it works

Stage one is the model. The tool probes `http://127.0.0.1:<llama-port>` and compares the
`model_path` from `/props` against the target file (with symlinks resolved). A match means it
reuses the running instance. A mismatch with a live pid file means it sends SIGTERM and
starts the new model. Nothing running means it launches llama-server directly. It then polls
`/v1/models` once per second until the server is ready or the timeout expires.

Stage two is the shim. It runs inside this process, binds `--shim-port`, removes tool
definitions that are not `type: "function"` from `/v1/responses` requests, and forwards them
to llama.cpp. Every other request is passed through untouched, and responses stream back, so
SSE is unaffected.

Stage three is Codex, launched as a child process with these overrides injected:

```
-c model_provider=llama_cpp
-c model_providers.llama_cpp.base_url=http://127.0.0.1:<shim-port>/v1
-c model_providers.llama_cpp.wire_api=responses
-c model=<alias>
```

That is why it does not depend on `~/.codex/llama.config.toml` and why changing ports or
models needs no config edits.

On process structure: `codex-local` *is* the shim and also the parent of Codex. Ctrl-C is
delivered by the terminal to the whole foreground process group, so Codex receives it
directly while the parent swallows it to avoid dying first. `kill <codex-local>` reaches only
the parent, so the parent forwards SIGTERM to Codex. When Codex exits, the parent returns its
exit code unchanged and the shim goes away with the process.

## 8. Common tasks

### Switch to another local model

```bash
./bin/codex-local -m gemma-3-27b
```

The tool notices the loaded model differs, stops the old instance (only if this tool started
it), loads the new model, waits for readiness, then enters Codex. The alias follows the
filename automatically; no extra flag needed.

### Set Codex's working directory

```bash
./bin/codex-local --cd ~/code/my-project
```

Without it, the current directory is used.

### The directory is not a git repository

Codex's `exec` only runs inside a git repository, and `[projects] trust_level = "trusted"` in
`~/.codex/config.toml` does not replace that. Add the flag the hint suggests:

```bash
./bin/codex-local --cd ~/code/my-project exec --skip-git-repo-check "..."
```

When the directory is not inside a git repository, the arguments contain `exec` and
`--skip-git-repo-check` is missing, the tool prints that hint once before starting.

### Shut the model down when you are done

```bash
./bin/codex-local --stop-after
```

By default llama.cpp stays in the background after Codex exits (loading 8 GB takes tens of
seconds, so repeated start/stop is wasteful) and the tool prints the stop command. With
`--stop-after` it shuts down only the instance this run started; a reused instance is left
alone.

### Stop the background llama.cpp by hand

```bash
kill $(cat run/llama-server.pid)    # run from the repository root
```

## 9. Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `cannot bind shim port 8010 (already in use?)` | Another shim or program holds the port. Find it with `lsof -nP -iTCP:8010 -sTCP:LISTEN`, stop it, or use `--shim-port` |
| `llama.cpp was not ready within 180s` | Check `logs/llama_server.log`; for large models or slow disks raise it with `LLAMA_WAIT=600` |
| `cannot start llama-server…` | `llama-server` is not on PATH; set `LLAMA_SERVER_BIN=/path/to/llama-server` |
| `cannot start codex…` | `codex` is not on PATH |
| `Not inside a trusted directory…` | The directory is not a git repository; add `--skip-git-repo-check` (see above) |
| `llama.cpp is running with a different model and the pid file is missing` | The running instance was not started by this tool, so it refuses to kill it. Stop it manually and retry |
| `could not stop the running llama.cpp` | SIGTERM was sent but it did not exit within 30 seconds; handle it manually and retry |
| `no .gguf model files in …`, or `cannot find model 'xxx', and … contains no .gguf files` | `models/` is empty. Drop a model in, or pass `-m /absolute/path/xxx.gguf` |
| `Unknown model google_gemma is used…` | Codex has no metadata for this model and falls back; harmless |
| Codex reports `'type' of tool must be 'function'` | The request bypassed the shim — check whether something connected straight to llama.cpp |

## 10. Relationship to the older tools

The earlier approach used a handful of standalone scripts; `codex-local` replaces them. If
you still have them locally, you can stop using them:

| Old component | Where it lives now |
| --- | --- |
| `llama-local` script | The tool launches llama-server itself |
| `codex-llama-ctl` script | The shim starts and stops with the main process |
| `codex-llama` binary | The shim is embedded in the main process |
| `codex-llama-shim/` (incl. the Node version) | Source moved to `src/` |

## 11. Known limitations

Dropping those tools has a real cost: the local model has no web search, no sub-agents, and
none of the `node_repl` / `cua_repl` MCP tools. Editing files happens through `exec_command`
(that is, the shell).

Performance reference (MacBook, Gemma 4 E4B Q8_0): about 39 tok/s generation and 122 tok/s
prefill, while the Codex system prompt is roughly 9k tokens — so expect each turn to start
around twenty to thirty seconds. Context is sized from the model's 131072.

## 12. License

MIT — see [LICENSE](LICENSE) in the repository root.
