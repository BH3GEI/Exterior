//! `exterior-kernel --profile <name>`：stdio JSON-RPC 服务（对应 `dsh --profile sdk-minimal`）。

use std::process::exit;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut profile = "sdk-minimal".to_string();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--profile" && i + 1 < args.len() {
            profile = args[i + 1].clone();
            i += 2;
        } else {
            i += 1;
        }
    }
    let (kernel, agents) = match exterior_kernel::boot(&profile).await {
        Ok(x) => x,
        Err(e) => {
            eprintln!("内核启动失败：{e}");
            exit(1);
        }
    };
    let dag = kernel
        .get::<std::sync::Arc<exterior_kernel::dag::DagProjection>>("dag")
        .map(|a| (*a).clone());
    let code = exterior_kernel::server::serve_stdio(agents, kernel.bus.clone(), dag).await;
    exit(code);
}
