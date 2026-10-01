use std::ffi::OsString;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use tokio::process::Command;
use tokio::signal::unix::{SignalKind, signal};

pub async fn run(
    shim_port: u16,
    alias: &str,
    work_dir: Option<PathBuf>,
    args: Vec<OsString>,
) -> Result<i32> {
    let mut cmd = Command::new("codex");
    cmd.arg("-c")
        .arg("model_provider=llama_cpp")
        .arg("-c")
        .arg("model_providers.llama_cpp.name=llama_cpp")
        .arg("-c")
        .arg(format!("model_providers.llama_cpp.base_url=http://127.0.0.1:{shim_port}/v1"))
        .arg("-c")
        .arg("model_providers.llama_cpp.wire_api=responses")
        .arg("-c")
        .arg(format!("model={alias}"));
    if let Some(dir) = &work_dir {
        cmd.arg("-C").arg(dir);
    }
    cmd.args(args);

    let mut child = cmd.spawn().context("无法启动 codex，请确认它在 PATH 中")?;
    let child_pid = child.id().map(|pid| pid as i32).unwrap_or(0);

    // Ctrl-C 由终端发给整个前台进程组，codex 会直接收到；这里只吞掉信号，防止本进程先死。
    let mut sigint = signal(SignalKind::interrupt())?;
    tokio::spawn(async move { loop { sigint.recv().await; } });
    // `kill <codex-local>` 只发给本进程，需要手动转发给 codex。
    let mut sigterm = signal(SignalKind::terminate())?;
    tokio::spawn(async move {
        loop {
            sigterm.recv().await;
            if child_pid > 0 {
                unsafe { libc::kill(child_pid, libc::SIGTERM) };
            }
        }
    });

    let status = child.wait().await?;
    Ok(status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}
