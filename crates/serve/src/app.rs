//! Shared state and the job runner: one child process at a time, the rest wait.

use crate::jobs::{Job, JobResult, JobState, Progress};
use crate::runs::file_url;
use rosie_mandlebrot_run::Event;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{watch, Notify};

/// Finished jobs kept in the state, newest last (the UI reads results there).
const FINISHED_KEPT: usize = 20;

pub struct App {
    pub data: PathBuf,
    /// Snapshot size for renders (0 = none).
    pub previews: u32,
    /// This program, run as each job's child.
    pub exe: PathBuf,
    pub version: &'static str,
    pub gpu_name: Mutex<Option<String>>,
    /// Whether ffmpeg runs (None until checked).
    pub ffmpeg: Mutex<Option<bool>>,
    pub inner: Mutex<Inner>,
    /// Bumped on every change: `.0` the state, `.1` the run list.
    pub changed: watch::Sender<(u64, u64)>,
    wake: Notify,
}

#[derive(Default)]
pub struct Inner {
    pub jobs: Vec<Job>,
    next_id: u64,
    /// The running child's stdin, for a graceful stop.
    stdin: Option<ChildStdin>,
    /// Paused (a game is full screen, the pause file exists, or someone
    /// pressed Pause): the queue holds, and a paused render resumes later.
    pub paused: bool,
    pub pause_reason: Option<String>,
    /// Runs whose render was paused, to resume when the pause ends.
    to_resume: Vec<String>,
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl App {
    pub fn new(data: PathBuf, exe: PathBuf, version: &'static str, previews: u32) -> Arc<App> {
        let (changed, _) = watch::channel((0, 0));
        Arc::new(App { data, previews, exe, version, gpu_name: Mutex::new(None), ffmpeg: Mutex::new(None), inner: Mutex::new(Inner::default()), changed, wake: Notify::new() })
    }

    /// Tell watchers something changed (`runs` too when the run list did).
    pub fn bump(&self, runs: bool) {
        self.changed.send_modify(|(s, r)| {
            *s += 1;
            if runs {
                *r += 1;
            }
        });
    }

    /// Queue a job; returns it as queued.
    #[allow(clippy::too_many_arguments)]
    pub fn enqueue(&self, kind: &'static str, run_id: Option<String>, label: String, args: Vec<String>, priority: bool, progress: Progress) -> Job {
        let job = {
            let mut inner = self.inner.lock().unwrap();
            inner.next_id += 1;
            let job = Job {
                id: inner.next_id,
                kind,
                run_id,
                label,
                state: JobState::Queued,
                queued_at: now(),
                started_at: None,
                finished_at: None,
                stage: None,
                progress,
                preview: None,
                result: JobResult::default(),
                error: None,
                log: Default::default(),
                args,
                priority,
                stop_requested: false,
                stage_started: None,
                pausing: false,
            };
            inner.jobs.push(job.clone());
            job
        };
        self.bump(true);
        self.wake.notify_one();
        job
    }

    /// Stop a job: a queued one is dropped, a running one is asked to stop
    /// (it finishes the chunk in hand, then makes its video).
    pub async fn stop(&self, id: u64) -> Option<Job> {
        let (job, stdin) = {
            let mut inner = self.inner.lock().unwrap();
            let pos = inner.jobs.iter().position(|j| j.id == id)?;
            match inner.jobs[pos].state {
                JobState::Queued => {
                    let mut j = inner.jobs.remove(pos);
                    j.state = JobState::Stopped;
                    j.finished_at = Some(now());
                    (j, None)
                }
                JobState::Running => {
                    inner.jobs[pos].stop_requested = true;
                    (inner.jobs[pos].clone(), inner.stdin.take())
                }
                _ => (inner.jobs[pos].clone(), None),
            }
        };
        if let Some(mut s) = stdin {
            let _ = s.write_all(b"stop\n").await;
            let _ = s.flush().await;
            // Keep the pipe open: closing it is not a stop request.
            self.inner.lock().unwrap().stdin = Some(s);
        }
        self.bump(true);
        Some(job)
    }

    /// The jobs list as the UI sees it.
    pub fn jobs(&self) -> Vec<Job> {
        self.inner.lock().unwrap().jobs.clone()
    }

    /// The active job for a run: (id, running).
    pub fn job_for_run(&self, run: &str) -> Option<(u64, bool)> {
        let inner = self.inner.lock().unwrap();
        inner.jobs.iter().find(|j| j.state.is_active() && j.kind != "values" && j.run_id.as_deref() == Some(run)).map(|j| (j.id, j.state == JobState::Running))
    }

    pub fn is_busy(&self) -> bool {
        self.inner.lock().unwrap().jobs.iter().any(|j| j.state == JobState::Running)
    }

    /// The next job to run: priority jobs first, then in order.
    fn take_next(&self) -> Option<Job> {
        let mut inner = self.inner.lock().unwrap();
        if inner.paused || inner.jobs.iter().any(|j| j.state == JobState::Running) {
            return None;
        }
        let pick = inner
            .jobs
            .iter()
            .position(|j| j.state == JobState::Queued && j.priority)
            .or_else(|| inner.jobs.iter().position(|j| j.state == JobState::Queued))?;
        let j = &mut inner.jobs[pick];
        j.state = JobState::Running;
        j.started_at = Some(now());
        Some(j.clone())
    }

    fn update(&self, id: u64, runs: bool, f: impl FnOnce(&mut Job)) {
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(j) = inner.jobs.iter_mut().find(|j| j.id == id) {
                f(j);
            }
        }
        self.bump(runs);
    }

    /// Pause or resume the queue. Pausing ends a running render without its
    /// video step (its strip is kept); resuming queues it again, first.
    pub async fn set_paused(self: &Arc<Self>, paused: bool, reason: Option<String>) {
        let (stdin, resume) = {
            let mut inner = self.inner.lock().unwrap();
            if inner.paused == paused {
                inner.pause_reason = reason.clone().or(inner.pause_reason.take());
                return;
            }
            inner.paused = paused;
            inner.pause_reason = if paused { reason } else { None };
            if paused {
                let running = inner.jobs.iter_mut().find(|j| j.state == JobState::Running && j.kind == "render");
                let mut run = None;
                if let Some(j) = running {
                    j.stop_requested = true;
                    j.pausing = true;
                    run = j.run_id.clone();
                }
                if let Some(r) = run {
                    inner.to_resume.push(r);
                    (inner.stdin.take(), Vec::new())
                } else {
                    (None, Vec::new())
                }
            } else {
                (None, std::mem::take(&mut inner.to_resume))
            }
        };
        if let Some(mut s) = stdin {
            let _ = s.write_all(b"pause\n").await;
            let _ = s.flush().await;
            self.inner.lock().unwrap().stdin = Some(s);
        }
        for run in resume {
            if let Some(meta) = crate::runs::read_meta(&self.data, &run) {
                match crate::api::queue_render_with(self, &meta, true, self.previews, true) {
                    Ok(j) => tracing::info!("resuming {run} after the pause (job {})", j.id),
                    Err(e) => tracing::warn!("could not resume {run}: {e}"),
                }
            }
        }
        tracing::info!("{}", if paused { "paused" } else { "resumed" });
        self.bump(true);
        self.wake.notify_one();
    }

    /// Runs jobs one at a time, forever.
    pub async fn worker(self: Arc<Self>) {
        loop {
            let Some(job) = self.take_next() else {
                self.wake.notified().await;
                continue;
            };
            self.bump(true);
            let outcome = self.run_child(&job).await;
            let finished = {
                let mut inner = self.inner.lock().unwrap();
                inner.stdin = None;
                if let Some(j) = inner.jobs.iter_mut().find(|j| j.id == job.id) {
                    j.finished_at = Some(now());
                    match &outcome {
                        Ok(0) if j.pausing => {
                            j.state = JobState::Stopped;
                            j.push_log("paused: resumes when the pause ends".into());
                        }
                        Ok(0) if j.stop_requested => j.state = JobState::Stopped,
                        Ok(0) => j.state = JobState::Done,
                        Ok(code) => {
                            j.state = JobState::Failed;
                            j.error = Some(j.error.clone().unwrap_or_else(|| last_error(&j.log).unwrap_or_else(|| format!("exit code {code}"))));
                        }
                        Err(e) => {
                            j.state = JobState::Failed;
                            j.error = Some(format!("{e:#}"));
                        }
                    }
                }
                // Keep the latest finished jobs only.
                let finished: Vec<u64> = inner.jobs.iter().filter(|j| !j.state.is_active()).map(|j| j.id).collect();
                if finished.len() > FINISHED_KEPT {
                    let drop: Vec<u64> = finished[..finished.len() - FINISHED_KEPT].to_vec();
                    inner.jobs.retain(|j| !drop.contains(&j.id));
                }
                inner.jobs.iter().find(|j| j.id == job.id).cloned()
            };
            self.bump(true);
            if let Some(j) = finished {
                crate::after_job(&self, &j);
            }
        }
    }

    async fn run_child(self: &Arc<Self>, job: &Job) -> anyhow::Result<i32> {
        let mut cmd = Command::new(&self.exe);
        cmd.args(&job.args).arg("--events");
        // The CPU limit applies to whatever the job is.
        let settings: crate::store::Settings = crate::store::load(&self.data.join("settings.json"));
        if let Some(p) = settings.cpu_percent {
            cmd.args(["--cpu-percent", &p.to_string()]);
        }
        cmd
            .current_dir(&self.data)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            // No console window for the child.
            cmd.creation_flags(0x0800_0000);
        }
        tracing::info!("job {} ({}): {}", job.id, job.kind, job.args.join(" "));
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        self.inner.lock().unwrap().stdin = child.stdin.take();
        let id = job.id;
        let err_task = {
            let app = Arc::clone(self);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    app.update(id, false, |j| j.push_log(l));
                }
            })
        };
        let mut lines = BufReader::new(stdout).lines();
        while let Some(l) = lines.next_line().await? {
            if l.starts_with("{\"ev\":") {
                match serde_json::from_str::<serde_json::Value>(&l) {
                    Ok(v) => self.on_event(id, &v),
                    Err(_) => self.update(id, false, |j| j.push_log(l)),
                }
            } else {
                self.update(id, false, |j| j.push_log(strip_ansi(&l)));
            }
        }
        let status = child.wait().await?;
        let _ = err_task.await;
        Ok(status.code().unwrap_or(1))
    }

    fn on_event(&self, id: u64, v: &serde_json::Value) {
        let Ok(ev) = serde_json::from_value::<EventIn>(v.clone()) else { return };
        let data = self.data.clone();
        let url = |p: &str| {
            let path = PathBuf::from(p);
            let path = if path.is_absolute() { path } else { data.join(path) };
            file_url(&data, &path)
        };
        match ev {
            EventIn::Started { stage, frames } => self.update(id, false, |j| {
                j.stage = Some(stage);
                j.stage_started = Some(std::time::Instant::now());
                j.progress.frames_total = frames;
                j.progress.frames = Some(0);
                j.progress.fraction = Some(0.0);
                j.progress.eta_secs = None;
            }),
            EventIn::Chunk { frames, span, bytes, elapsed_secs } => self.update(id, false, |j| {
                let p = &mut j.progress;
                p.frames = Some(frames);
                p.span = Some(span);
                p.bytes = Some(bytes);
                p.elapsed_secs = Some(elapsed_secs);
                if let (Some(a), Some(b)) = (p.start_span, p.end_span) {
                    if a > b && span > 0.0 {
                        let f = ((a / span).ln() / (a / b).ln()).clamp(0.0, 1.0);
                        p.fraction = Some(f);
                        p.eta_secs = (f > 0.02).then(|| elapsed_secs * (1.0 - f) / f);
                    }
                }
            }),
            EventIn::Frame { k, frames } => self.update(id, false, |j| {
                if j.stage.as_deref() == Some("assemble") {
                    j.progress.frames = Some(k + 1);
                    j.progress.frames_total = frames;
                    if let Some(n) = frames.filter(|&n| n > 0) {
                        let f = (k + 1) as f64 / n as f64;
                        j.progress.fraction = Some(f);
                        if let Some(t) = j.stage_started {
                            let secs = t.elapsed().as_secs_f64();
                            j.progress.eta_secs = (f > 0.05).then(|| secs * (1.0 - f) / f);
                        }
                    }
                }
            }),
            EventIn::Image { path } => {
                let u = url(&path);
                let values = path.ends_with(".rmw");
                self.update(id, !values, |j| {
                    if values {
                        j.result.values = u;
                    } else if j.kind == "render" {
                        j.preview = u;
                    }
                })
            }
            EventIn::Video { path } => {
                let u = url(&path);
                self.update(id, true, |j| j.result.video = u)
            }
            EventIn::Finished => self.update(id, true, |_| {}),
        }
    }
}

/// The events a child prints, as far as the runner uses them.
#[derive(serde::Deserialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
enum EventIn {
    Started {
        stage: String,
        frames: Option<u64>,
    },
    Chunk {
        frames: u64,
        span: f64,
        bytes: u64,
        elapsed_secs: f64,
    },
    Frame {
        k: u64,
        frames: Option<u64>,
    },
    Image {
        path: String,
    },
    Video {
        path: String,
    },
    Finished,
}

// Kept in step with the program's own event type: a renamed or removed event
// must fail to compile here rather than be silently ignored.
#[allow(dead_code)]
fn _events_in_step(e: Event) {
    match e {
        Event::Started { .. } | Event::Chunk { .. } | Event::Frame { .. } | Event::Image { .. } | Event::Video { .. } | Event::Place(_) | Event::Finished { .. } => {}
    }
}

/// The child's last error line, without the log prefix.
fn last_error(log: &std::collections::VecDeque<String>) -> Option<String> {
    log.iter().rev().find(|l| l.contains("ERROR") || l.starts_with("error:")).map(|l| {
        let l = l.trim();
        match l.find("ERROR") {
            Some(i) => l[i + 5..].trim().to_string(),
            None => l.trim_start_matches("error:").trim().to_string(),
        }
    })
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
