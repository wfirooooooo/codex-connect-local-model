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
#[command(name = "codex-local", version, about = "一键启动 llama.cpp + 转换层 + Codex（本地模型）")]
struct Cli {
    /// 模型：路径、文件名或简写（在 <root>/models 下搜索 .gguf）
    #[arg(short, long, default_value = "google_gemma-4-E4B-it-Q8_0.gguf")]
    model: String,

    /// llama.cpp 模型别名（默认取文件名第一个 '-' 之前）
    #[arg(long)]
    alias: Option<String>,

    /// llama.cpp 端口
    #[arg(long, default_value_t = 8001)]
    llama_port: u16,

    /// 转换层端口
    #[arg(long, default_value_t = 8010)]
    shim_port: u16,

    /// 让 Codex 以该目录作为工作根（映射到 codex 的 -C）
    #[arg(long, value_name = "DIR")]
    cd: Option<PathBuf>,

    /// 透传给 llama-server（仅本次启动时生效）
    #[arg(long)]
    ctx_size: Option<u32>,

    /// 透传给 llama-server（仅本次启动时生效）
    #[arg(long)]
    n_gpu_layers: Option<i32>,

    /// Codex 退出后关闭本次启动的 llama.cpp
    #[arg(long)]
    stop_after: bool,

    /// 列出可用模型并退出
    #[arg(short, long)]
    list: bool,

    /// 转发给 codex 的参数（首个位置参数开始）
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
                .with_context(|| format!("--cd 目录不存在：{}", dir.display()))?;
            if !dir.is_dir() {
                bail!("--cd 不是目录：{}", dir.display());
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

    let model_path = llama::resolve_model(&root, &cli.model)?;
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

    // [2/3] 进程内转换层
    let addr = SocketAddr::from(([127, 0, 0, 1], cli.shim_port));
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("转换层端口 {} 无法监听（可能已被占用）", cli.shim_port))?;
    let upstream: Uri = format!("{llama_url}/").parse().context("无效的上游地址")?;
    let shim_log = root.join("logs").join("shim.log");
    tokio::spawn(shim::serve(listener, upstream, client.clone(), Some(shim_log)));
    println!("[2/3] 转换层已就绪 http://127.0.0.1:{}", cli.shim_port);

    // [3/3] Codex
    println!("[3/3] 启动 Codex（model={alias}）");
    let code = codex::run(cli.shim_port, &alias, work_dir, cli.codex_args).await?;

    if cli.stop_after {
        if let Some(pid) = pid {
            llama::terminate(pid);
            println!("已停止本次启动的 llama.cpp（PID {pid}）");
        }
    } else if started {
        println!("llama.cpp 仍在后台运行（PID {}），停止：kill $(cat {}/run/llama-server.pid)", pid.unwrap_or(0), root.display());
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
            println!("[1/3] llama.cpp 已加载 {alias}");
            return Ok((false, None));
        }
        let Some(pid) = llama::read_pid(root) else {
            bail!("llama.cpp 正在运行但加载的是其它模型，且 pid 文件缺失，请先手动停止它再试");
        };
        println!("[1/3] llama.cpp 已加载其它模型，重启为 {alias} ...");
        llama::terminate(pid);
        if !wait_down(client, llama_url).await {
            bail!("无法停止正在运行的 llama.cpp，请手动停止后再试");
        }
    } else {
        println!("[1/3] 启动 llama.cpp：{alias} ...");
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
    println!("      llama.cpp 就绪（PID {pid}）");

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

/// `codex exec` 只肯在 git 仓库里跑，除非显式给 --skip-git-repo-check。
fn warn_if_untrusted(dir: &Path, codex_args: &[OsString]) {
    let is_exec = codex_args.iter().any(|arg| arg == "exec");
    let opted_out = codex_args.iter().any(|arg| arg == "--skip-git-repo-check");
    let in_git_repo = dir.ancestors().any(|parent| parent.join(".git").exists());

    if is_exec && !opted_out && !in_git_repo {
        eprintln!(
            "提示：{} 不是 git 仓库，codex exec 需要 --skip-git-repo-check，例如：\n  codex-local --cd {} exec --skip-git-repo-check \"...\"",
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
