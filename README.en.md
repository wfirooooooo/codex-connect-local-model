# codex-local

One command to run a local model and drop into Codex: it starts llama.cpp, starts the
in-process shim, then launches Codex.

Full documentation: [MANUAL.en.md](MANUAL.en.md). 中文文档见 [README.md](README.md) / [MANUAL.md](MANUAL.md)。

## Build

Run these from the repository root:

```bash
cargo build --release
cp target/release/codex-local bin/
```

## Usage

```bash
./bin/codex-local                                       # no model: list what is under models/
./bin/codex-local -m gemma-3-27b                        # pick a model (shorthand / filename / path)
./bin/codex-local -m gemma-3-27b exec "run the tests"   # everything else is forwarded to codex
./bin/codex-local -l                                    # list .gguf files under models/
```

Options:

```
-m, --model <path|shorthand>   lists available models and exits when omitted
    --alias <name>             llama.cpp model alias (default: filename before the first '-')
    --llama-port <port>        default 8001
    --shim-port <port>         default 8010
    --cd <dir>                 working root for Codex (maps to codex's -C)
    --ctx-size <n>             forwarded to llama-server
    --n-gpu-layers <n>         forwarded to llama-server
    --stop-after               stop the model started by this run when Codex exits
-l, --list
```

## Layout

- `src/` source (main / llama / shim / codex)
- `bin/` build output `codex-local`
- `models/` model files (currently a symlink to the original gguf)
- `logs/` `llama_server.log`, `shim.log`
- `run/` runtime pid files
- `config/` reserved (not used yet)

## Behavior

- Model identity is decided by the absolute `model_path` from `/props`, not by alias —
  aliases collide when two files share a prefix.
- llama.cpp keeps running in the background after Codex exits; `--stop-after` shuts down the
  instance this run started. The shim lives inside the launcher process and dies with it.
- When switching models, only an instance started by this tool (matching pid file) is
  terminated. A foreign process is left alone with a message asking you to stop it.

## "The directory Codex trusts"

Codex looks at the working directory itself; `[projects] trust_level = "trusted"` in
`~/.codex/config.toml` does not replace that check:

```bash
./bin/codex-local --cd /path/to/project          # set the working root
./bin/codex-local --cd /path/to/project exec --skip-git-repo-check "..."   # non-git directory
```

Without `--cd`, the current directory is used. When the directory is not inside a git
repository and the arguments contain `exec`, the tool prints a hint about
`--skip-git-repo-check`; it stays quiet if you already passed that flag.

Environment variables: `CODEX_LOCAL_ROOT` (project root), `LLAMA_SERVER_BIN` (llama-server
path), `LLAMA_WAIT` (readiness timeout in seconds, default 180).

## License

MIT — see [LICENSE](LICENSE).
