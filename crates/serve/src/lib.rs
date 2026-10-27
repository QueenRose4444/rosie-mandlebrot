//! `rosie_mandlebrot serve`: the web UI. One long-lived HTTP server that runs
//! every GPU job (a render, another video from a render, a values file, the
//! preset pictures) as a child copy of this program, one at a time, and tells
//! the browser what happens over server-sent events.

#![allow(clippy::result_large_err)]

mod api;
mod app;
pub mod desktop;
mod estimate;
mod found;
mod jobs;
mod notify;
mod runs;
mod share;
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

    /// Windows: show an icon in the notification area (Open, Pause, Quit).
    #[arg(long)]
    pub tray: bool,
}

/// Why the queue is paused, when it was paused from the menu or the page.
pub(crate) const PAUSED_BY_HAND: &str = "paused by hand";
const PAUSED_FOR_GAME: &str = "a full-screen app (a game?) is running";
const PAUSED_BY_FILE: &str = "the pause file exists";

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
    let app = App::new(data, exe, version, a.previews);
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    if a.tray {
        return with_tray(app, a, rt);
    }
    rt.block_on(serve(app, a))
}

/// The server on its own thread, the tray icon's message loop on this one.
fn with_tray(app: Arc<App>, a: ServeArgs, rt: tokio::runtime::Runtime) -> anyhow::Result<()> {
    let url = format!("http://{}:{}", if a.host == "0.0.0.0" { "localhost" } else { a.host.as_str() }, a.port);
    let tray = desktop::Tray::new("Rosie_Mandlebrot")?;
    let handle = tray.handle();
    let rt = Arc::new(rt);
    let server = {
        let (rt, app) = (rt.clone(), app.clone());
        std::thread::spawn(move || {
            let r = rt.block_on(serve(app, a));
            handle.quit();
            r
        })
    };
    let state_app = app.clone();
    let state = move || {
        let paused = state_app.inner.lock().unwrap().paused;
        let jobs = state_app.jobs();
        let status = match jobs.iter().find(|j| j.state == jobs::JobState::Running) {
            Some(j) => format!("{} {}%", j.label, (j.progress.fraction.unwrap_or(0.0) * 100.0).round()),
            None if paused => "paused".into(),
            None => "idle".into(),
        };
        desktop::TrayState { paused, status }
    };
    let on = |cmd: desktop::TrayCmd| match cmd {
        desktop::TrayCmd::Open => open_browser(&url),
        desktop::TrayCmd::Pause => rt.block_on(app.set_paused(true, Some(PAUSED_BY_HAND.into()))),
        desktop::TrayCmd::Resume => rt.block_on(app.set_paused(false, None)),
        desktop::TrayCmd::Quit => {
            // A running render stops gracefully: its strip is kept and resumes next time.
            rt.block_on(async {
                if app.is_busy() {
                    app.set_paused(true, Some(PAUSED_BY_HAND.into())).await;
                }
                // Give the child a moment to finish its chunk and exit.
                for _ in 0..100 {
                    if !app.is_busy() {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            });
        }
    };
    tray.run(state, on)?;
    drop(server);
    Ok(())
}

/// Pause while a full-screen app is in front (when the setting is on) or while
/// a file named `pause` exists in the data folder (for a server: `touch` it
/// before another container needs the GPU, delete it afterwards).
async fn pause_watcher(app: Arc<App>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let settings: store::Settings = store::load(&app.data.join("settings.json"));
        let reason = if app.data.join("pause").exists() {
            Some(PAUSED_BY_FILE)
        } else if settings.pause_when_fullscreen && desktop::fullscreen_busy() {
            Some(PAUSED_FOR_GAME)
        } else {
            None
        };
        let (paused, current) = {
            let inner = app.inner.lock().unwrap();
            (inner.paused, inner.pause_reason.clone())
        };
        match reason {
            Some(r) if !paused => app.set_paused(true, Some(r.into())).await,
            None if paused && matches!(current.as_deref(), Some(PAUSED_BY_FILE | PAUSED_FOR_GAME)) => app.set_paused(false, None).await,
            _ => {}
        }
    }
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
            let ffmpeg = std::process::Command::new("ffmpeg").arg("-version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success());
            if !ffmpeg {
                tracing::warn!("ffmpeg is not on PATH: renders are saved, but no video can be made until it is installed");
            }
            *app.ffmpeg.lock().unwrap() = Some(ffmpeg);
            app.bump(false);
        });
    }
    tokio::spawn(app.clone().worker());
    tokio::spawn(pause_watcher(app.clone()));
    if a.resume_on_start {
        for meta in runs::list_meta(&app.data) {
            let run = runs::load_run(&app.data, meta.clone(), None);
            let marker = runs::run_dir(&app.data, &meta.id).join(RESUME_MARKER);
            let paused_by_shutdown = marker.exists();
            let _ = std::fs::remove_file(&marker);
            if run.status == "incomplete" || (paused_by_shutdown && run.status == "stopped") {
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
/// Ctrl+C, or SIGTERM (`docker stop`): a running render is paused (it
/// finishes the chunk in hand, keeps its strip and makes no video) and marked
/// to carry on at the next start with `--resume-on-start`; any other job is
/// stopped.
async fn shutdown(app: Arc<App>) {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("a SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
    println!("stopping…");
    let running: Vec<jobs::Job> = app.jobs().into_iter().filter(|j| j.state == jobs::JobState::Running).collect();
    for j in &running {
        if j.kind == "render" {
            if let Some(run) = &j.run_id {
                let _ = std::fs::write(runs::run_dir(&app.data, run).join(RESUME_MARKER), b"");
            }
        } else {
            app.stop(j.id).await;
        }
    }
    if running.iter().any(|j| j.kind == "render") {
        app.set_paused(true, Some("shutting down".into())).await;
    }
    // Docker waits 10 seconds before it kills; a chunk usually ends well within that.
    for _ in 0..90 {
        if !app.is_busy() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // Open pages hold their event streams forever, which a graceful HTTP
    // shutdown would wait for; the jobs are settled, so leave now.
    std::process::exit(0);
}

/// A run with this file was paused by a shutdown and resumes at the next start
/// (with `--resume-on-start`).
const RESUME_MARKER: &str = "resume-on-start";

pub fn open_browser(url: &str) {
    // For automated tests of the double-click start, which must not open pages.
    if std::env::var_os("ROSIE_MANDLEBROT_NO_BROWSER").is_some() {
        tracing::info!("not opening a browser (ROSIE_MANDLEBROT_NO_BROWSER is set): {url}");
        return;
    }
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
    if job.pausing {
        return;
    }
    let strip_kept = !(store::load::<store::Settings>(&app.data.join("settings.json")).delete_strip_after_video && job.result.video.is_some());
    if job.kind == "render" && job.state != jobs::JobState::Failed && strip_kept {
        if let Some(run) = &job.run_id {
            let body = jobs::ValuesBody { run_id: run.clone(), k: None, width: Some(640), height: None };
            let args = jobs::values_args(&body, &runs::run_dir(&app.data, run));
            app.enqueue("values", Some(run.clone()), format!("colours of {run}"), args, true, jobs::Progress::default());
        }
    }
    let settings: store::Settings = store::load(&app.data.join("settings.json"));
    // "Delete a run's render data once its video is made": the strip goes,
    // the videos and snapshots stay (the run can no longer be recoloured).
    if job.kind == "render" && job.state == jobs::JobState::Done && settings.delete_strip_after_video && job.result.video.is_some() {
        if let Some(run) = &job.run_id {
            let strip = runs::run_dir(&app.data, run).join("out").join("keyframes");
            match std::fs::remove_dir_all(&strip) {
                Ok(()) => tracing::info!("deleted the render data of {run} (the setting is on)"),
                Err(e) => tracing::warn!("could not delete {}: {e}", strip.display()),
            }
            app.bump(true);
        }
    }
    if matches!(job.kind, "render" | "assemble") {
        let n = settings.notify;
        if n.discord_webhook.is_empty() && n.upload_url.is_empty() {
            return;
        }
        // The video file, from its /files/… URL.
        let video = job.result.video.as_deref().and_then(|u| u.strip_prefix("/files/")).map(|rel| app.data.join(rel)).filter(|p| p.exists());
        let job = job.clone();
        tokio::task::spawn_blocking(move || {
            for line in notify::send(&n, &job, video.as_deref()) {
                tracing::info!("job {}: {line}", job.id);
            }
        });
    }
}
