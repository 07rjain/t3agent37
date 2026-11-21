//! Headless VPS / CLI daemon for `t3agent37`: runs all machine localhost proxies,
//! proxy-driven sleep/wake controller, cost accrual, and the local MCP server (:47730)
//! without needing GTK/WebKit2GTK or a desktop tray.
//!
//! Reads commands from stdin:
//!   list
//!   create <name> [small|medium|large|xlarge|max] [default|vm|performance] [us|eu|ap-south] [disk_gb]
//!   pair <id|name>
//!   wake <id|name>
//!   sleep <id|name>
//!   keep <id|name> [minutes]        (omit minutes: forever; 0: auto-sleep on)
//!   resize <id|name> <size> [disk_gb]
//!   grow-disk <id|name> <disk_gb>
//!   github <id|name> [pat]
//!   signin <id|name> <Claude|Codex>
//!   connect <id|name>
//!   upgrade <id|name>
//!   delete <id|name> [discard]
//!   costs
//!   help

use std::sync::Arc;
use std::time::Duration;
use t3boat_lib::agent37::{InstanceType, Region};
use t3boat_lib::app::{self, App, Setup};
use t3boat_lib::machine::signin::Agent;
use t3boat_lib::machine::Event;
use t3boat_lib::{mcp, webui};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info,t3boat_lib=debug".into()))
        .init();

    let dir = std::env::var("T3AGENT37_DATA_DIR")
        .or_else(|_| std::env::var("T3BOAT_DATA_DIR"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| dirs::data_dir().unwrap().join("t3agent37"));

    let (setup, agent37, t3) = app::check_setup(Some(&dir)).await;
    if setup != Setup::Ready {
        anyhow::bail!(
            "setup incomplete: {setup:?} (set AGENT37_API_KEY or save key to {}/agent37-key, and ensure ~/.t3/bin/t3 exists)",
            dir.display()
        );
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let app = App::new(agent37.unwrap(), t3.unwrap(), dir, tx)?;
    let ui = webui::WebUi::new(app.clone(), webui::locate_web_dir());
    let saver = app.clone();
    let ui_events = ui.clone();
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                Event::Persist => saver.save(),
                Event::Changed => ui_events.emit_state(),
                Event::Notify { title, body } => {
                    println!("NOTIFY {title}: {body}");
                    ui_events.emit_notify(&title, &body);
                }
                Event::SignInCode(code) => {
                    if let Some(ref c) = code {
                        println!("SIGN_IN_CODE {c}");
                    }
                    ui_events.set_sign_in_code(code);
                }
            }
        }
    });

    // Periodically refresh Agent37 wallet balance & accrue compute/disk spend.
    let cost_app = app.clone();
    let cost_ui = ui.clone();
    tokio::spawn(async move {
        loop {
            cost_app.refresh_costs().await;
            cost_ui.emit_state();
            tokio::time::sleep(Duration::from_secs(180)).await;
        }
    });

    // Serve MCP (/mcp) + live Web UI (/, /assets/*, /api/events SSE, /api/cmd/*) on 127.0.0.1:47730.
    let http_router = mcp::app_router(app.clone()).merge(webui::router(ui));
    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(("127.0.0.1", mcp::MCP_PORT)).await {
            Ok(listener) => {
                if let Err(e) = axum::serve(listener, http_router).await {
                    tracing::warn!("HTTP server failed on port {}: {e:#}", mcp::MCP_PORT);
                }
            }
            Err(e) => tracing::warn!("binding 127.0.0.1:{} failed: {e:#}", mcp::MCP_PORT),
        }
    });

    tracing::info!(
        "t3agent37-daemon ready (Web UI at http://127.0.0.1:{}/ · MCP at {})",
        mcp::MCP_PORT,
        mcp::url()
    );
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let words: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        if words.is_empty() {
            continue;
        }
        let app = app.clone();
        tokio::spawn(async move {
            if let Err(e) = command(&app, &words).await {
                println!("ERROR {e:#}");
            }
        });
    }
    std::future::pending::<()>().await;
    Ok(())
}

async fn command(app: &Arc<App>, words: &[String]) -> anyhow::Result<()> {
    let arg = |i: usize| words.get(i).cloned().unwrap_or_default();
    match words.first().map(String::as_str) {
        Some("list") => {
            for v in app.views() {
                println!(
                    "{} {} ({:?}/{:?}/{}GB) [{:?}] {} paired={} gh={}",
                    v.id, v.name, v.instance_type, v.region, v.disk_gb, v.state, v.detail, v.paired, v.github_signed_in
                );
            }
        }
        Some("create") => {
            let size = words.get(2).cloned().unwrap_or_else(|| "small".into());
            let ty = words.get(3).map(|s| InstanceType::from_str(s)).transpose()?.unwrap_or_default();
            let region = words.get(4).map(|s| Region::from_str(s)).transpose()?.unwrap_or_default();
            let disk = words.get(5).map(|s| s.parse::<u32>()).transpose()?;
            let m = app.create_machine(&arg(1), &size, ty, region, disk, Vec::new()).await?;
            println!("CREATED {} on http://localhost:{}", m.id(), m.record().local_port);
        }
        Some("pair") => app.pair(&*app.machine(&arg(1))?)?,
        Some("upgrade") => app.machine(&arg(1))?.upgrade_if_needed().await?,
        Some("wake") => app.machine(&arg(1))?.ensure_awake().await?,
        Some("sleep") => app.machine(&arg(1))?.sleep_now().await?,
        Some("connect") => app.machine(&arg(1))?.turn_on_connect().await?,
        Some("github") => app.machine(&arg(1))?.sign_in_github(words.get(2).map(String::as_str)).await?,
        Some("signin") => {
            let agent = Agent::from_name(&arg(2))
                .ok_or_else(|| anyhow::anyhow!("agent must be Claude or Codex"))?;
            app.machine(&arg(1))?.sign_in(agent).await?;
        }
        Some("keep") => app.machine(&arg(1))?.keep_awake(words.get(2).map(|m| m.parse()).transpose()?).await?,
        Some("resize") => {
            let disk = words.get(3).map(|s| s.parse::<u32>()).transpose()?;
            app.machine(&arg(1))?.resize(words.get(2).map(String::as_str), disk).await?;
        }
        Some("grow-disk") => {
            let disk: u32 = arg(2).parse()?;
            app.machine(&arg(1))?.resize(None, Some(disk)).await?;
        }
        Some("delete") => app.delete_machine(&*app.machine(&arg(1))?, arg(2) == "discard").await?,
        Some("costs") => {
            app.refresh_costs().await;
            println!("{:?}", app.costs());
        }
        Some("help") => {
            println!("Commands: list | create <name> [size] [type] [region] [disk_gb] | pair | wake | sleep | keep <id> [min] | resize <id> <size> [disk_gb] | grow-disk <id> <gb> | github <id> [pat] | signin <id> <Claude|Codex> | connect | upgrade | delete <id> [discard] | costs");
        }
        _ => println!("Unknown command (type `help`)"),
    }
    Ok(())
}
