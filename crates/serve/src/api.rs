//! The HTTP API the browser UI uses.

use crate::app::{now, App};
use crate::jobs::{self, JobBody, Progress};
use crate::runs::{self, RunMeta};
use crate::store::{self, Favourites, Settings};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::{self, Stream, StreamExt};
use rosie_mandlebrot_core::palette::{to_hex, ColorScheme};
use rosie_mandlebrot_core::presets::{load_presets, Preset};
use serde::Deserialize;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
struct Ctx {
    app: Arc<App>,
    previews: u32,
}

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

fn not_found(what: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, format!("no {what}"))
}

type ApiResult<T> = Result<T, ApiError>;

pub fn router(app: Arc<App>, previews: u32) -> Router {
    let data = app.data.clone();
    let ctx = Ctx { app, previews };
    Router::new()
        .route("/", get(|| async { Html(crate::ui::INDEX) }))
        .route("/ui/{*path}", get(ui_file))
        .route("/api/state", get(state))
        .route("/api/events", get(events))
        .route("/api/presets", get(presets))
        .route("/api/palettes", get(palettes))
        .route("/api/runs", get(list_runs))
        .route("/api/runs/{id}", get(one_run).delete(delete_run))
        .route("/api/runs/{id}/resume", post(resume_run))
        .route("/api/jobs", post(new_job))
        .route("/api/jobs/{id}/stop", post(stop_job))
        .route("/api/estimate", get(estimate))
        .route("/api/pause", post(pause))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/favourites", get(get_favourites).put(put_favourites))
        .nest_service("/files/runs", tower_http::services::ServeDir::new(runs::runs_dir(&data)))
        .nest_service("/heroes", tower_http::services::ServeDir::new(data.join("heroes")))
        .with_state(ctx)
}

async fn ui_file(UrlPath(path): UrlPath<String>) -> Response {
    match crate::ui::file(&path) {
        Some((ct, body)) => ([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "no-cache")], body).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn state_json(app: &App) -> serde_json::Value {
    serde_json::json!({
        "version": app.version,
        "gpu": { "name": app.gpu_name.lock().unwrap().clone(), "busy": app.is_busy() },
        "paused": app.inner.lock().unwrap().paused,
        "pause_reason": app.inner.lock().unwrap().pause_reason.clone(),
        "jobs": app.jobs(),
        "data_dir": app.data.display().to_string(),
    })
}

async fn state(State(c): State<Ctx>) -> Json<serde_json::Value> {
    Json(state_json(&c.app))
}

/// `state` on every change (at most four a second), `runs` when the run list
/// changed too.
async fn events(State(c): State<Ctx>) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = c.app.changed.subscribe();
    let start = (c.app, rx, true, u64::MAX);
    let s = stream::unfold(start, |(app, mut rx, first, last_runs)| async move {
        if !first {
            rx.changed().await.ok()?;
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let (_, runs_v) = *rx.borrow_and_update();
        let mut out = vec![Ok(SseEvent::default().event("state").data(state_json(&app).to_string()))];
        if runs_v != last_runs {
            out.push(Ok(SseEvent::default().event("runs").data(runs_v.to_string())));
        }
        Some((stream::iter(out), (app, rx, false, runs_v)))
    })
    .flatten();
    Sse::new(s).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

fn preset_list() -> ApiResult<Vec<Preset>> {
    load_presets(None).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn presets(State(c): State<Ctx>) -> ApiResult<Json<serde_json::Value>> {
    let heroes = c.app.data.join("heroes");
    let list: Vec<serde_json::Value> = preset_list()?
        .into_iter()
        .map(|p| {
            let hero = heroes.join(format!("{}.jpg", p.name)).exists().then(|| format!("/heroes/{}.jpg", p.name));
            serde_json::json!({
                "name": p.name,
                "kind": p.kind.map(|k| format!("{k:?}").to_lowercase()),
                "description": p.description,
                "depth": p.suggested_zoom_end.to_string(),
                "palette": p.suggested_palette,
                "center": format!("{}, {}", p.center_re, p.center_im),
                "hero": hero,
            })
        })
        .collect();
    Ok(Json(serde_json::Value::Array(list)))
}

async fn palettes() -> Json<serde_json::Value> {
    let list: Vec<serde_json::Value> = ColorScheme::ALL
        .iter()
        .map(|s| {
            let stops: Vec<(f64, String)> = s.stops().iter().map(|st| (st.pos, to_hex(st.rgb))).collect();
            let lut = s.lut_1024();
            serde_json::json!({ "name": s.name(), "stops": stops, "inside": to_hex(lut[1023]) })
        })
        .collect();
    Json(serde_json::Value::Array(list))
}

async fn list_runs(State(c): State<Ctx>) -> Json<Vec<runs::Run>> {
    let app = c.app.clone();
    let list = tokio::task::spawn_blocking(move || runs::list_meta(&app.data).into_iter().map(|m| {
        let job = app.job_for_run(&m.id);
        runs::load_run(&app.data, m, job)
    }).collect())
    .await
    .unwrap_or_default();
    Json(list)
}

fn meta_of(app: &App, id: &str) -> ApiResult<RunMeta> {
    if !jobs::valid_run_id(id) {
        return Err(bad("not a run id"));
    }
    runs::read_meta(&app.data, id).ok_or_else(|| not_found("such run"))
}

async fn one_run(State(c): State<Ctx>, UrlPath(id): UrlPath<String>) -> ApiResult<Json<runs::Run>> {
    let meta = meta_of(&c.app, &id)?;
    let job = c.app.job_for_run(&id);
    Ok(Json(runs::load_run(&c.app.data, meta, job)))
}

async fn delete_run(State(c): State<Ctx>, UrlPath(id): UrlPath<String>) -> ApiResult<StatusCode> {
    meta_of(&c.app, &id)?;
    if c.app.jobs().iter().any(|j| j.state.is_active() && j.run_id.as_deref() == Some(id.as_str())) {
        return Err(ApiError(StatusCode::CONFLICT, "a job is using this run; stop it first".into()));
    }
    std::fs::remove_dir_all(runs::run_dir(&c.app.data, &id)).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    c.app.bump(true);
    Ok(StatusCode::NO_CONTENT)
}

/// This computer's offset from UTC, so run names carry the local time.
fn local_offset_secs() -> i64 {
    time::UtcOffset::current_local_offset().map_or(0, |o| i64::from(o.whole_seconds()))
}

/// Start and end span of a render, for its progress bar.
fn spans(preset: &str, zoom_end: Option<f64>) -> (Option<f64>, Option<f64>) {
    let p = load_presets(None).ok().and_then(|ps| ps.into_iter().find(|p| p.name == preset));
    let start = p.as_ref().and_then(|p| p.initial_span.to_f64());
    let end = zoom_end.or_else(|| p.and_then(|p| p.suggested_zoom_end.to_f64())).map(|e| e.max(1e-300));
    (start, end)
}

/// Queue a render for a run (new, or resuming it).
pub fn queue_render(app: &Arc<App>, meta: &RunMeta, resume: bool, previews: u32) -> Result<jobs::Job, String> {
    queue_render_with(app, meta, resume, previews, false)
}

/// [`queue_render`], ahead of the queue when `first`.
pub fn queue_render_with(app: &Arc<App>, meta: &RunMeta, resume: bool, previews: u32, first: bool) -> Result<jobs::Job, String> {
    let dir = runs::run_dir(&app.data, &meta.id);
    let args = jobs::render_args(&meta.settings, &dir, resume, previews)?;
    let (start_span, end_span) = spans(&meta.preset, meta.settings.zoom_end);
    let progress = Progress { start_span, end_span, ..Default::default() };
    Ok(app.enqueue("render", Some(meta.id.clone()), meta.label.clone(), args, first, progress))
}

async fn resume_run(State(c): State<Ctx>, UrlPath(id): UrlPath<String>) -> ApiResult<Json<jobs::Job>> {
    let meta = meta_of(&c.app, &id)?;
    if c.app.job_for_run(&id).is_some() {
        return Err(ApiError(StatusCode::CONFLICT, "this run already has a job".into()));
    }
    queue_render(&c.app, &meta, true, c.previews).map(Json).map_err(bad)
}

async fn new_job(State(c): State<Ctx>, Json(body): Json<JobBody>) -> ApiResult<Json<jobs::Job>> {
    let app = &c.app;
    let job = match body {
        JobBody::Render(b) => {
            // Checked before the run folder exists.
            jobs::render_args(&b, std::path::Path::new("x"), false, 0).map_err(bad)?;
            let id = runs::new_run_id(&app.data, &b.preset, now() + local_offset_secs());
            let label = b.preset.clone();
            let meta = RunMeta { id, preset: b.preset.clone(), label, created: now(), settings: b };
            runs::write_meta(&app.data, &meta).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            queue_render(app, &meta, false, c.previews).map_err(bad)?
        }
        JobBody::Assemble(b) => {
            let meta = meta_of(app, &b.run_id)?;
            let size = (meta.settings.width, meta.settings.height);
            let args = jobs::assemble_args(&b, &runs::run_dir(&app.data, &b.run_id), size).map_err(bad)?;
            app.enqueue("assemble", Some(b.run_id.clone()), format!("new video of {}", meta.label), args, false, Progress::default())
        }
        JobBody::Values(b) => {
            let meta = meta_of(app, &b.run_id)?;
            let args = jobs::values_args(&b, &runs::run_dir(&app.data, &b.run_id));
            app.enqueue("values", Some(b.run_id.clone()), format!("colours of {}", meta.label), args, true, Progress::default())
        }
        JobBody::Heroes(b) => {
            let mut args = vec!["--heroes".to_string(), app.data.join("heroes").display().to_string()];
            args.extend(["--log-file".to_string(), app.data.join("logs").join("heroes.log").display().to_string()]);
            let label = match &b.presets {
                Some(list) if list.len() == 1 => {
                    if !jobs::valid_name(&list[0]) {
                        return Err(bad("not a preset name"));
                    }
                    args.extend(["--preset".to_string(), list[0].clone()]);
                    format!("picture of {}", list[0])
                }
                _ => "preset pictures".to_string(),
            };
            app.enqueue("heroes", None, label, args, true, Progress::default())
        }
    };
    Ok(Json(job))
}

async fn stop_job(State(c): State<Ctx>, UrlPath(id): UrlPath<u64>) -> ApiResult<Json<jobs::Job>> {
    c.app.stop(id).await.map(Json).ok_or_else(|| not_found("such job"))
}

#[derive(Deserialize)]
struct PauseBody {
    paused: bool,
}

async fn pause(State(c): State<Ctx>, Json(b): Json<PauseBody>) -> Json<serde_json::Value> {
    c.app.set_paused(b.paused, b.paused.then(|| crate::PAUSED_BY_HAND.to_string())).await;
    Json(state_json(&c.app))
}

#[derive(Deserialize)]
struct EstimateQuery {
    preset: String,
    width: u32,
    height: u32,
    zoom_end: Option<f64>,
    zoom_factor: Option<f64>,
    memory_limit_mb: Option<u32>,
}

async fn estimate(State(c): State<Ctx>, Query(q): Query<EstimateQuery>) -> ApiResult<Json<crate::estimate::Estimate>> {
    if !jobs::valid_name(&q.preset) || !(16..=16384).contains(&q.width) || !(16..=16384).contains(&q.height) {
        return Err(bad("preset, width and height are needed"));
    }
    let p = preset_list()?.into_iter().find(|p| p.name == q.preset).ok_or_else(|| not_found("such preset"))?;
    let start = p.initial_span.to_f64().unwrap_or(3.0);
    let end = q.zoom_end.or_else(|| p.suggested_zoom_end.to_f64()).unwrap_or(1e-300).max(1e-300);
    let zoom = q.zoom_factor.or(p.zoom_factor).unwrap_or(0.98).clamp(0.5, 0.9999);
    let data = c.app.data.clone();
    let e = tokio::task::spawn_blocking(move || crate::estimate::estimate(&data, q.width, q.height, start, end, zoom, q.memory_limit_mb))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(e))
}

async fn get_settings(State(c): State<Ctx>) -> Json<Settings> {
    Json(store::load(&c.app.data.join("settings.json")))
}

async fn put_settings(State(c): State<Ctx>, Json(s): Json<Settings>) -> ApiResult<Json<Settings>> {
    s.check().map_err(bad)?;
    store::save(&c.app.data.join("settings.json"), &s).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    c.app.bump(false);
    Ok(Json(s))
}

async fn get_favourites(State(c): State<Ctx>) -> Json<Favourites> {
    Json(store::load(&c.app.data.join("favourites.json")))
}

async fn put_favourites(State(c): State<Ctx>, Json(f): Json<Favourites>) -> ApiResult<Json<Favourites>> {
    f.check().map_err(bad)?;
    store::save(&c.app.data.join("favourites.json"), &f).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(f))
}
