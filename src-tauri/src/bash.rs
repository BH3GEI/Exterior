use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

pub struct BashResult {
    pub output: String,
    pub exit_code: i32,
    pub aborted: bool,
}

/// SPEC §3.3：bash -lc，cwd=$HOME，无沙箱无超时；输出边跑边推；中止 = 杀进程组。
pub async fn run_bash(
    cmd: &str,
    cancel: Arc<AtomicBool>,
    mut on_chunk: impl FnMut(&str),
) -> Result<BashResult, String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let mut child = Command::new("bash")
        .arg("-lc")
        .arg(cmd)
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0) // 独立进程组，便于整组杀掉
        .spawn()
        .map_err(|e| format!("启动 bash 失败：{e}"))?;

    let pid = child.id().unwrap_or(0) as i32;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut output = String::new();
    let mut aborted = false;
    let mut buf = [0u8; 8192];

    // stderr 合并进同一输出（SPEC §3.3：合并后的 stdout+stderr）
    let err_out = tokio::spawn(async move {
        let mut s = String::new();
        let mut b = [0u8; 8192];
        loop {
            match stderr.read(&mut b).await {
                Ok(0) | Err(_) => break,
                Ok(n) => s.push_str(&String::from_utf8_lossy(&b[..n])),
            }
        }
        s
    });

    let code = loop {
        if cancel.load(Ordering::Relaxed) && !aborted {
            aborted = true;
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
        }
        tokio::select! {
            r = stdout.read(&mut buf) => {
                match r {
                    Ok(0) => break child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1),
                    Ok(n) => {
                        let s = String::from_utf8_lossy(&buf[..n]);
                        output.push_str(&s);
                        on_chunk(&s);
                    }
                    Err(_) => break -1,
                }
            }
            s = child.wait() => {
                let code = s.ok().and_then(|x| x.code()).unwrap_or(-1);
                // 读完剩余输出
                loop {
                    match stdout.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let s = String::from_utf8_lossy(&buf[..n]);
                            output.push_str(&s);
                            on_chunk(&s);
                        }
                    }
                }
                break code;
            }
        }
    };

    Ok(BashResult {
        output: output + &err_out.await.unwrap_or_default(),
        exit_code: if aborted { -1 } else { code }, // SPEC §3.3：中止记为中止（-1）
        aborted,
    })
}
