mod codex;
mod llama;
mod shim;

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use clap::Parser;
use http_body_util::Full;
use hyper::Uri;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use tokio::net::TcpListener;

pub type HttpClient = Client<HttpConnector, Full<Bytes>>;

#[derive(Parser)]
#[command(
    name = "codex-local",
    version,
    about = "Start llama.cpp, an in-process shim and Codex against a local model"
)]
struct Cli {
    /// Model: path, filename or shorthand (searched under <root>/models for .gguf). Lists available models when omitted
    #[arg(short, long)]
    model: Option<String>,

    /// llama.cpp model alias (default: the model filename up to the first '-')
    #[arg(long)]
    alias: Option<String>,

    /// Port for llama.cpp to listen on
    #[arg(long, default_value_t = 8001)]
    llama_port: u16,

    /// Port for the in-process shim to listen on
    #[arg(long, default_value_t = 8010)]
    shim_port: u16,

    /// Working root for Codex (maps to codex's -C)
    #[arg(long, value_name = "DIR")]
    cd: Option<PathBuf>,

    /// Forwarded to llama-server (only when this run starts it)
    #[arg(long)]
    ctx_size: Option<u32>,

    /// Forwarded to llama-server (only when this run starts it)
    #[arg(long)]
    n_gpu_layers: Option<i32>,

    /// Stop the llama.cpp started by this run when Codex exits
    #[arg(long)]
    stop_after: bool,

    /// List available models and exit
    #[arg(short, long)]
    list: bool,

    /// Arguments forwarded to codex (starting at the first positional argument)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    codex_args: Vec<OsString>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = project_root();

    if cli.list {
        llama::list_models(&root);
        return Ok(());
    }

    let work_dir = match &cli.cd {
        Some(dir) => {
            let dir = std::fs::canonicalize(dir)
                .with_context(|| format!("--cd directory does not exist: {}", dir.display()))?;
            if !dir.is_dir() {
                bail!("--cd is not a directory: {}", dir.display());
            }
            warn_if_untrusted(&dir, &cli.codex_args);
            Some(dir)
        }
        None => {
            if let Ok(cwd) = std::env::current_dir() {
                warn_if_untrusted(&cwd, &cli.codex_args);
            }
            None
        }
    };

    let model_arg = match cli.model.clone() {
        Some(model) => model,
        None => {
            let models = llama::available_models(&root);
            let dir = root.join("models");
            if models.is_empty() {
                bail!(
                    "no .gguf model files in {}. Add one there, or point -m at any .gguf path.",
                    dir.display()
                );
            }
            println!("No model specified; available under {}:", dir.display());
            for (index, path) in models.iter().enumerate() {
                println!(
                    "{:>3}. {}",
                    index + 1,
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
            }
            let example = models[0].file_name().unwrap_or_default().to_string_lossy().to_string();
            println!();
            println!("Pick one with -m, for example: codex-local -m {example}");
            std::process::exit(1);
        }
    };

    let model_path = llama::resolve_model(&root, &model_arg)?;
    let alias = cli
        .alias
        .unwrap_or_else(|| llama::alias_from_path(&model_path));
    let client: HttpClient = Client::builder(TokioExecutor::new()).build_http();
    let llama_url = format!("http://127.0.0.1:{}", cli.llama_port);

    // [1/3] llama.cpp
    let launch = llama::LaunchConfig {
        port: cli.llama_port,
        ctx_size: cli.ctx_size,
        n_gpu_layers: cli.n_gpu_layers,
    };
    let (started, pid) =
        ensure_llama(&client, &root, &llama_url, &model_path, &alias, &launch).await?;

    // [2/3] in-process shim
    let addr = SocketAddr::from(([127, 0, 0, 1], cli.shim_port));
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot bind shim port {} (already in use?)", cli.shim_port))?;
    let upstream: Uri = format!("{llama_url}/").parse().context("invalid upstream URL")?;
    let shim_log = root.join("logs").join("shim.log");
    tokio::spawn(shim::serve(listener, upstream, client.clone(), Some(shim_log)));
    println!("[2/3] shim ready on http://127.0.0.1:{}", cli.shim_port);

    // [3/3] Codex
    println!("[3/3] launching Codex (model={alias})");
    let code = codex::run(cli.shim_port, &alias, work_dir, cli.codex_args).await?;

    if cli.stop_after {
        if let Some(pid) = pid {
            llama::terminate(pid);
            println!("Stopped the llama.cpp started by this run (PID {pid})");
        }
    } else if started {
        println!(
            "llama.cpp is still running in the background (PID {}); stop it with: kill $(cat {}/run/llama-server.pid)",
            pid.unwrap_or(0),
            root.display()
        );
    }

    std::process::exit(code);
}

async fn ensure_llama(
    client: &HttpClient,
    root: &Path,
    llama_url: &str,
    model_path: &Path,
    alias: &str,
    launch: &llama::LaunchConfig,
) -> Result<(bool, Option<u32>)> {
    let expected = model_path.to_string_lossy().to_string();
    let st = llama::status(client, llama_url).await;

    if st.up {
        if st.model_path.as_deref() == Some(expected.as_str()) {
            println!("[1/3] llama.cpp already loaded {alias}");
            return Ok((false, None));
        }
        let Some(pid) = llama::read_pid(root) else {
            bail!(
                "llama.cpp is running with a different model and the pid file is missing; stop it manually and retry"
            );
        };
        println!("[1/3] llama.cpp has a different model loaded; restarting with {alias} ...");
        llama::terminate(pid);
        if !wait_down(client, llama_url).await {
            bail!("could not stop the running llama.cpp; stop it manually and retry");
        }
    } else {
        println!("[1/3] starting llama.cpp: {alias} ...");
    }

    let pid = llama::spawn_server(
        root,
        &llama::LlamaTarget { path: model_path.to_path_buf(), alias: alias.to_string() },
        launch,
    )?;
    llama::write_pid(root, pid);

    let timeout = std::env::var("LLAMA_WAIT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(180));
    llama::wait_ready(client, llama_url, timeout).await?;
    println!("      llama.cpp ready (PID {pid})");

    Ok((true, Some(pid)))
}

async fn wait_down(client: &HttpClient, llama_url: &str) -> bool {
    for _ in 0..30 {
        if !llama::status(client, llama_url).await.up {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    false
}

/// `codex exec` only runs inside a git repository unless --skip-git-repo-check is passed.
fn warn_if_untrusted(dir: &Path, codex_args: &[OsString]) {
    let is_exec = codex_args.iter().any(|arg| arg == "exec");
    let opted_out = codex_args.iter().any(|arg| arg == "--skip-git-repo-check");
    let in_git_repo = dir.ancestors().any(|parent| parent.join(".git").exists());

    if is_exec && !opted_out && !in_git_repo {
        eprintln!(
            "Note: {} is not a git repository; codex exec needs --skip-git-repo-check, for example:\n  codex-local --cd {} exec --skip-git-repo-check \"...\"",
            dir.display(),
            dir.display()
        );
    }
}

fn project_root() -> PathBuf {
    if let Ok(root) = std::env::var("CODEX_LOCAL_ROOT") {
        return PathBuf::from(root);
    }
    let mut dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    match dir.file_name().and_then(|s| s.to_str()) {
        Some("bin") => {
            dir.pop();
        }
        Some("release") | Some("debug") => {
            dir.pop();
            dir.pop();
        }
        _ => {}
    }
    dir
}
