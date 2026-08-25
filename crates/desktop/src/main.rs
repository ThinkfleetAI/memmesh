// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! MemMesh Desktop — the Rust engine + the console UI in one native app.
//!
//! No browser, no Node, no Tauri CLI: the engine (`memory-server`) runs
//! in-process on a background tokio runtime, and a system webview (`wry`)
//! renders the same console UI the OSS `memmesh console` serves. One binary
//! that is the whole product.

use anyhow::Result;
use memory_storage::Storage;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

const ADDR: &str = "127.0.0.1:7878";

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();

    // The engine + console HTTP server run on their own tokio runtime in a
    // background thread — the native event loop must own the main thread.
    std::thread::spawn(|| {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = rt.block_on(run_engine()) {
            eprintln!("memmesh-desktop: engine failed: {e}");
        }
    });

    // Wait for the server to bind before pointing the webview at it.
    wait_for_server(ADDR);

    run_window()
}

/// Open the SQLite store, migrate, and serve the console REST API + UI.
async fn run_engine() -> Result<()> {
    let db = default_db_path();
    let url = format!("sqlite://{db}?mode=rwc");
    let store = memory_storage::sqlite::SqliteStore::connect(&url).await?;
    store.migrate().await?;
    memory_server::serve_http(Arc::new(store), ADDR).await
}

fn run_window() -> Result<()> {
    use tao::{
        dpi::LogicalSize,
        event::{Event, WindowEvent},
        event_loop::{ControlFlow, EventLoop},
        window::WindowBuilder,
    };
    use wry::WebViewBuilder;

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("MemMesh")
        .with_inner_size(LogicalSize::new(1220.0, 840.0))
        .with_min_inner_size(LogicalSize::new(760.0, 560.0))
        .build(&event_loop)?;

    let _webview = WebViewBuilder::new(&window)
        .with_url(format!("http://{ADDR}"))
        .build()?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    });
}

fn default_db_path() -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let dir = std::path::Path::new(&home).join(".memmesh");
        std::fs::create_dir_all(&dir).ok();
        return dir.join("memory.db").to_string_lossy().to_string();
    }
    "./memory.db".to_string()
}

/// Poll the console port for readiness (embedding-model load can take a few
/// seconds on cold start).
fn wait_for_server(addr: &str) {
    let sa: SocketAddr = addr.parse().expect("valid addr");
    for _ in 0..60 {
        if TcpStream::connect_timeout(&sa, Duration::from_millis(200)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}
