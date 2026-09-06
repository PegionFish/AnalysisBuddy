//! ab-server 入口：解析 CLI → 装配引擎 → 绑定监听 → ctrl_c 优雅停机
//! （停 HTTP → 关停全部插件进程）。

use std::process::ExitCode;

use ab_engine::paths::EnginePaths;

use ab_server::args::{parse_args, resolve_paths, ServerArgs, USAGE};
use ab_server::routes::build_router;
use ab_server::state::{assemble, AssembleOptions};
use ab_server::PROTOCOL_VERSION;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let parsed = match parse_args(&argv) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("ab-server: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let paths = resolve_paths(&parsed);
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(run(parsed, paths))
}

async fn run(parsed: ServerArgs, paths: EnginePaths) -> ExitCode {
    let state = match assemble(
        paths.clone(),
        AssembleOptions {
            max_concurrent_imports: parsed.max_concurrent_imports,
            token: parsed.token.clone(),
            file_id_fn: None,
        },
    ) {
        Ok(state) => state,
        Err(e) => {
            eprintln!("ab-server: assemble failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let listener = match tokio::net::TcpListener::bind(&parsed.addr).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("ab-server: cannot bind {}: {e}", parsed.addr);
            return ExitCode::FAILURE;
        }
    };
    println!(
        "ab-server: listening on http://{} (protocol v{PROTOCOL_VERSION})",
        listener
            .local_addr()
            .map(|addr| addr.to_string())
            .unwrap_or_else(|_| parsed.addr.clone())
    );
    println!(
        "ab-server: plugins portable={} user={}; presets={}; sessions={}",
        paths.plugins_portable.display(),
        paths.plugins_user.display(),
        paths.presets_dir.display(),
        paths.sessions_dir.display(),
    );
    if state.token.is_some() {
        println!("ab-server: bearer token auth enabled");
    }

    let app = build_router(state.clone());
    let serve = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());
    if let Err(e) = serve.await {
        eprintln!("ab-server: server error: {e}");
        state.host.shutdown_all().await;
        return ExitCode::FAILURE;
    }
    state.host.shutdown_all().await;
    println!("ab-server: bye");
    ExitCode::SUCCESS
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    println!("ab-server: shutdown signal received");
}
