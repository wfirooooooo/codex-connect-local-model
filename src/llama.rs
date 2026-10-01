use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Uri};
use serde_json::Value;

use crate::HttpClient;

pub struct LlamaTarget {
    pub path: PathBuf,
    pub alias: String,
}

pub struct LaunchConfig {
    pub port: u16,
    pub ctx_size: Option<u32>,
    pub n_gpu_layers: Option<i32>,
}

pub struct LlamaStatus {
    pub up: bool,
    pub model_path: Option<String>,
}

pub fn resolve_model(root: &Path, arg: &str) -> Result<PathBuf> {
    let direct = Path::new(arg);
    if direct.is_file() {
        return std::fs::canonicalize(direct).context("cannot resolve model path");
    }
    let models = root.join("models");
    let exact = models.join(arg);
    if exact.is_file() {
        return std::fs::canonicalize(exact).context("cannot resolve model path");
    }

    let prefix = glob(&models, |name| name.starts_with(arg) && name.ends_with(".gguf"));
    match prefix.len() {
        1 => return std::fs::canonicalize(prefix.into_iter().next().unwrap()).context("cannot resolve model path"),
        n if n > 1 => bail!("'{arg}' matches multiple model files:\n{}", format_list(prefix)),
        _ => {}
    }

    let substring = glob(&models, |name| name.contains(arg) && name.ends_with(".gguf"));
    match substring.len() {
        1 => return std::fs::canonicalize(substring.into_iter().next().unwrap()).context("cannot resolve model path"),
        n if n > 1 => bail!("'{arg}' matches multiple model files:\n{}", format_list(substring)),
        _ => {}
    }

    let all = glob(&models, |name| name.ends_with(".gguf"));
    if all.is_empty() {
        bail!(
            "cannot find model '{arg}', and {} contains no .gguf files",
            models.display()
        );
    }
    bail!(
        "cannot find model '{arg}'. .gguf files under models/:\n{}",
        format_list(all)
    )
}

pub fn alias_from_path(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("model");
    stem.split('-').next().unwrap_or(stem).to_string()
}

pub fn available_models(root: &Path) -> Vec<PathBuf> {
    glob(&root.join("models"), |name| name.ends_with(".gguf"))
}

pub fn list_models(root: &Path) {
    let models = available_models(root);
    if models.is_empty() {
        println!("{} contains no .gguf model files", root.join("models").display());
        return;
    }
    println!("Model directory: {}", root.join("models").display());
    for (index, path) in models.iter().enumerate() {
        println!("{:>3}. {}", index + 1, path.file_name().unwrap_or_default().to_string_lossy());
    }
}

pub async fn status(client: &HttpClient, llama_url: &str) -> LlamaStatus {
    if let Some(props) = fetch_json(client, &format!("{llama_url}/props")).await {
        let model_path = props.get("model_path").and_then(|v| v.as_str()).map(String::from);
        return LlamaStatus { up: true, model_path };
    }
    LlamaStatus { up: false, model_path: None }
}

pub fn spawn_server(
    root: &Path,
    target: &LlamaTarget,
    config: &LaunchConfig,
) -> Result<u32> {
    let log_dir = root.join("logs");
    std::fs::create_dir_all(&log_dir).ok();
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("llama_server.log"))
        .context("cannot open logs/llama_server.log")?;

    let bin = std::env::var("LLAMA_SERVER_BIN").unwrap_or_else(|_| "llama-server".to_string());
    let mut cmd = Command::new(&bin);
    cmd.arg("--model")
        .arg(&target.path)
        .arg("--alias")
        .arg(&target.alias)
        .arg("--port")
        .arg(config.port.to_string());
    if let Some(ctx) = config.ctx_size {
        cmd.arg("--ctx-size").arg(ctx.to_string());
    }
    if let Some(layers) = config.n_gpu_layers {
        cmd.arg("--n-gpu-layers").arg(layers.to_string());
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log_file.try_clone().context("cannot redirect output")?))
        .stderr(Stdio::from(log_file));
    cmd.process_group(0);

    let child = cmd
        .spawn()
        .with_context(|| format!("cannot start {bin}; make sure llama.cpp is installed and {bin} is on PATH"))?;
    Ok(child.id())
}

pub async fn wait_ready(client: &HttpClient, llama_url: &str, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        if fetch_json(client, &format!("{llama_url}/v1/models")).await.is_some() {
            return Ok(());
        }
        if start.elapsed() > timeout {
            bail!(
                "llama.cpp was not ready within {}s; check logs/llama_server.log",
                timeout.as_secs()
            );
        }
        let waited = start.elapsed().as_secs();
        if waited > 0 && waited.is_multiple_of(5) {
            println!("  waiting for the model to load ({waited}s)");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub fn read_pid(root: &Path) -> Option<u32> {
    let pid = std::fs::read_to_string(root.join("run").join("llama-server.pid"))
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()?;
    (unsafe { libc::kill(pid as i32, 0) } == 0).then_some(pid)
}

pub fn write_pid(root: &Path, pid: u32) {
    std::fs::create_dir_all(root.join("run")).ok();
    std::fs::write(root.join("run").join("llama-server.pid"), pid.to_string()).ok();
}

pub fn terminate(pid: u32) {
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
}

async fn fetch_json(client: &HttpClient, url: &str) -> Option<Value> {
    let uri: Uri = url.parse().ok()?;
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Full::new(Bytes::new()))
        .ok()?;
    let response = client.request(request).await.ok()?;
    let body = response.into_body().collect().await.ok()?.to_bytes();
    serde_json::from_slice(&body).ok()
}

fn glob(root: &Path, predicate: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut matches: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(&predicate)
                .unwrap_or(false)
        })
        .collect();
    matches.sort();
    matches
}

fn format_list(paths: Vec<PathBuf>) -> String {
    paths
        .into_iter()
        .map(|p| format!("  - {}", p.display()))
        .collect::<Vec<_>>()
        .join("\n")
}
