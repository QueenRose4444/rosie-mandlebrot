//! `rosie_mandlebrot serve`: the web UI. One long-lived HTTP server that runs
//! every GPU job (a render, another video from a render, a values file, the
//! preset pictures) as a child copy of this program, one at a time, and tells
//! the browser what happens over server-sent events.

#![allow(clippy::result_large_err)]

mod api;
mod app;
mod estimate;
mod jobs;
mod runs;
mod store;
mod ui;

use app::App;
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Parser)]
#[command(name = "rosie_mandlebrot serve", about = "Rosie_Mandlebrot's web UI: open the address it prints in a browser.")]
pub struct ServeArgs {
    /// Address to listen on. 127.0.0.1 = this computer only; 0.0.0.0 = every
    /// network the computer is on (there is no password).
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    #[arg(long, default_value_t = 8460)]
    pub port: u16,

    /// Where runs, videos and settings are kept [default: an `app` folder
    /// beside the program, or in the user data folder when installed].
    #[arg(long)]
    pub data: Option<PathBuf>,

    /// Open the page in the default browser once the server is up.
    #[arg(long)]
    pub open: bool,

    /// Carry on with renders that were interrupted (for a container that restarts).
    #[arg(long)]
    pub resume_on_start: bool,

    /// Snapshot size while rendering, pixels on the long side (0 = none).
    #[arg(long, default_value_t = 640)]
    pub previews: u32,

    /// Use the folder beside the program for everything.
    #[arg(long)]
    pub portable: bool,
}

/// Run the server until Ctrl+C. `args` are the arguments after `serve`.
pub fn main(args: Vec<String>, version: &'static str) -> anyhow::Result<()> {
    let a = ServeArgs::parse_from(std::iter::once("rosie_mandlebrot serve".to_string()).chain(args));
    let data = match &a.data {
        Some(d) => d.clone(),
        None => rosie_mandlebrot_core::paths::Layout::detect(a.portable).root().join("app"),
    };
    std::fs::create_dir_all(runs::runs_dir(&data))?;
    let data = data.canonicalize().unwrap_or(data);
    let data = strip_verbatim(data);
    let exe = std::env::current_exe()?;
    let app = App::new(data, exe, version);
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(serve(app, a))
}

/// `\\?\C:\…` from canonicalize would leak into every path the children see.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

async fn serve(app: Arc<App>, a: ServeArgs) -> anyhow::Result<()> {
    {
        let app = app.clone();
        tokio::task::spawn_blocking(move || {
            let name = rosie_mandlebrot_render::gpu::adapter_name();
            *app.gpu_name.lock().unwrap() = name;
            app.bump(false);
        });
    }
    tokio::spawn(app.clone().worker());
    if a.resume_on_start {
        for meta in runs::list_meta(&app.data) {
            let run = runs::load_run(&app.data, meta.clone(), None);
            if run.status == "incomplete" {
                match api::queue_render(&app, &meta, true, a.previews) {
                    Ok(j) => tracing::info!("resuming run {} (job {})", meta.id, j.id),
                    Err(e) => tracing::warn!("could not resume run {}: {e}", meta.id),
                }
            }
        }
    }
    let router = api::router(app.clone(), a.previews);
    let addr: SocketAddr = format!("{}:{}", a.host, a.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let shown = if a.host == "0.0.0.0" { format!("http://localhost:{}", a.port) } else { format!("http://{addr}") };
    println!("Rosie_Mandlebrot {} is at {shown}  (data: {})", app.version, app.data.display());
    tracing::info!("serving {shown}, data in {}", app.data.display());
    if a.open {
        open_browser(&shown);
    }
    axum::serve(listener, router).with_graceful_shutdown(shutdown(app.clone())).await?;
    Ok(())
}

/// Ctrl+C: stop the running job gracefully (its strip is kept and can resume).
async fn shutdown(app: Arc<App>) {
    let _ = tokio::signal::ctrl_c().await;
    let running: Vec<u64> = app.jobs().iter().filter(|j| j.state == jobs::JobState::Running).map(|j| j.id).collect();
    for id in running {
        app.stop(id).await;
    }
    println!("stopping…");
}

pub fn open_browser(url: &str) {
    #[cfg(windows)]
    let r = std::process::Command::new("rundll32.exe").args(["url.dll,FileProtocolHandler", url]).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(e) = r {
        tracing::warn!("could not open a browser ({e}); open {url} yourself");
    }
}

/// What happens when a job ends: a finished render gets its colour-editor
/// values made straight away, and notifications go out.
pub(crate) fn after_job(app: &Arc<App>, job: &jobs::Job) {
    if job.kind == "render" && job.state != jobs::JobState::Failed {
        if let Some(run) = &job.run_id {
            let body = jobs::ValuesBody { run_id: run.clone(), k: None, width: Some(640), height: None };
            let args = jobs::values_args(&body, &runs::run_dir(&app.data, run));
            app.enqueue("values", Some(run.clone()), format!("colours of {run}"), args, true, jobs::Progress::default());
        }
    }
}
