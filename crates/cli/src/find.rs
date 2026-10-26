//! `rosie_mandlebrot find`: the finder from the command line, and the job the
//! web UI runs for a search. The search runs on worker threads; each place it
//! confirms is pictured down its zoom on the GPU here, scored, written to
//! `places.json` and reported as an event.

use clap::Parser;
use rosie_mandlebrot_core::config::{parse_center, DeepEngine};
use rosie_mandlebrot_core::encode::encode_rgb;
use rosie_mandlebrot_core::palette::ColorScheme;
use rosie_mandlebrot_core::sequence::SequenceConfig;
use rosie_mandlebrot_ref::find::{find_deeper, find_new, Interest, Place, Search};
use rosie_mandlebrot_render::{render_frame, FrameRequest, GpuCtx};
use rosie_mandlebrot_run::assemble::write_jpeg;
use rosie_mandlebrot_run::signals::{spawn_listener, StdinSignals};
use rosie_mandlebrot_run::{Event, Events, JsonLines, NoEvents, PlaceEvent};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

#[derive(Debug, Parser)]
#[command(name = "rosie_mandlebrot find", about = "Find places to zoom into: deeper ones beside a place you know, or new ones.")]
pub struct FindCli {
    /// Look below the end of this preset's zoom.
    #[arg(long, conflicts_with_all = ["center", "new"])]
    pub like: Option<String>,
    /// Look below the end of the zoom into this centre ("re,im").
    #[arg(long, value_parser = parse_center, conflicts_with = "new")]
    pub center: Option<(String, String)>,
    /// Look for new places instead, at least --min-depth deep.
    #[arg(long)]
    pub new: bool,
    /// For --new: the shallowest end of zoom to keep, as a span.
    #[arg(long, default_value_t = 1e-30)]
    pub min_depth: f64,
    /// Keep only places at least this interesting (0–100).
    #[arg(long, default_value_t = 0.0)]
    pub interest: f64,
    /// Picture and score one place without searching: --center and its end of zoom (a span).
    #[arg(long, requires = "center")]
    pub describe: Option<f64>,
    /// Search for this long, in seconds.
    #[arg(long, default_value_t = 60)]
    pub seconds: u64,
    /// How far from the end of the zoom so far a deeper place may be, as a
    /// power of that minibrot's size: 1 keeps close (the zoom matches the
    /// original all the way down), lower strays into its embedded Julia sets
    /// (it matches less of the way, then goes deeper faster).
    #[arg(long, default_value_t = 0.75)]
    pub reach: f64,
    /// Stop after this many places.
    #[arg(long)]
    pub max: Option<u32>,
    /// Folder for the pictures and places.json.
    #[arg(long, default_value = "found")]
    pub out: PathBuf,
    /// Pictures down each place's zoom (the last is near its end).
    #[arg(long, default_value_t = 5)]
    pub pictures: u32,
    /// Picture size in pixels (square).
    #[arg(long, default_value_t = 192)]
    pub picture_size: u32,
    #[arg(long, default_value = "hot")]
    pub palette: String,
    /// Starting point of the search's random choices: the same seed and time
    /// limit give the same places.
    #[arg(long, default_value_t = 1)]
    pub seed: u64,
    /// One JSON object per line on stdout for each step (for the web UI).
    #[arg(long)]
    pub events: bool,
    /// Let a software renderer stand in for a GPU (slow).
    #[arg(long)]
    pub allow_software_gpu: bool,
}

pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let cli = FindCli::parse_from(std::iter::once("rosie_mandlebrot find".to_string()).chain(args));
    let events: Box<dyn Events> = if cli.events { Box::new(JsonLines::new(std::io::stdout())) } else { Box::new(NoEvents) };
    let ev = events.as_ref();
    let palette = ColorScheme::parse(&cli.palette)?;
    std::fs::create_dir_all(&cli.out)?;
    let gpu = GpuCtx::new(None, cli.allow_software_gpu)?;
    let started = Instant::now();
    ev.emit(Event::Started { stage: "find", width: cli.picture_size, height: cli.picture_size, frames: None });

    let start = match (&cli.like, &cli.center) {
        (Some(name), _) => {
            let p = rosie_mandlebrot_core::presets::load_presets(None)?
                .into_iter()
                .find(|p| p.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| anyhow::anyhow!("no preset called {name:?}"))?;
            Some((p.center_re, p.center_im))
        }
        (None, Some(c)) => Some(c.clone()),
        _ => None,
    };

    if let (Some(end), Some((re, im))) = (cli.describe, &start) {
        let rungs = rosie_mandlebrot_ref::ladder::atom_ladder(re, im, 200_000);
        let interest = Interest::from_rungs(&rungs, end.log10());
        let ev_place = picture_place(&gpu, &cli, palette, 1, re, im, 0, end, interest, 0)?;
        println!("{}", serde_json::to_string(&ev_place)?);
        ev.emit(Event::Place(Box::new(ev_place)));
        ev.emit(Event::Finished { stage: "find", status: "stopped", reason: "described".into(), elapsed_secs: started.elapsed().as_secs_f64() });
        return Ok(());
    }

    let signals = StdinSignals::default();
    let _stdin = spawn_listener(signals.clone());
    let stop = signals.stop.clone();
    let deadline = started + Duration::from_secs(cli.seconds);
    let (tx, rx) = std::sync::mpsc::channel::<Place>();
    let (reach, seed, min_depth) = (cli.reach, cli.seed, cli.min_depth);
    let searcher = {
        let stop = stop.clone();
        let start = start.clone();
        std::thread::Builder::new().name("finder".into()).spawn(move || {
            let s = Search { deadline, stop: &stop, samples: 256, solves: 8, iter_cap: 200_000, reach, seed };
            let mut send = |p: &Place| {
                let _ = tx.send(p.clone());
            };
            match start {
                Some((re, im)) => find_deeper(&s, &re, &im, &mut send),
                None => find_new(&s, min_depth.log10(), &mut send),
            }
        })?
    };

    let mut places: Vec<PlaceEvent> = Vec::new();
    let mut n = 0;
    loop {
        let p = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(p) => p,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        n += 1;
        let end = 10f64.powf(p.end_log10());
        let place = match picture_place(&gpu, &cli, palette, n, &p.nucleus.re, &p.nucleus.im, p.nucleus.period, end, p.interest, p.level) {
            Ok(place) => place,
            Err(e) => {
                // A place the renderer cannot draw all the way down is no use.
                tracing::info!("place {n} (period {}, end {end:.2e}) left out: {e}", p.nucleus.period);
                n -= 1;
                continue;
            }
        };
        tracing::info!(
            "place {n}: period {}, end {end:.2e}, interest {:.0}%, level {}",
            place.period,
            place.interest,
            place.level
        );
        if place.interest < cli.interest {
            continue;
        }
        places.push(place.clone());
        std::fs::write(cli.out.join("places.json"), serde_json::to_vec_pretty(&places)?)?;
        ev.emit(Event::Place(Box::new(place)));
        if cli.max.is_some_and(|m| places.len() as u32 >= m) {
            stop.store(true, Ordering::Relaxed);
        }
    }
    let _ = searcher.join();
    let reason = if signals.stop.load(Ordering::Relaxed) { "stopped" } else { "time limit" };
    tracing::info!("finder: {} places in {:.0} s ({reason})", places.len(), started.elapsed().as_secs_f64());
    ev.emit(Event::Finished { stage: "find", status: "stopped", reason: reason.into(), elapsed_secs: started.elapsed().as_secs_f64() });
    Ok(())
}

/// Pictures down the zoom into (`re`, `im`) — spans spaced evenly in log from
/// a fifth of the way down to 8× the end — with their detail, and the place's
/// event.
#[allow(clippy::too_many_arguments)]
fn picture_place(
    gpu: &GpuCtx,
    cli: &FindCli,
    palette: ColorScheme,
    n: u32,
    re: &str,
    im: &str,
    period: u32,
    end: f64,
    interest: Interest,
    level: u32,
) -> anyhow::Result<PlaceEvent> {
    let seq = SequenceConfig::default();
    let (top, bottom) = (seq.initial_span.log10(), (end * 8.0).log10());
    let k = cli.pictures.max(1);
    let mut pictures = Vec::new();
    let mut details = Vec::new();
    for i in 0..k {
        let span = 10f64.powf(top + (bottom - top) * (i + 1) as f64 / k as f64);
        let (rgba, detail) = picture(gpu, re, im, span, period, cli.picture_size, palette)?;
        let path = cli.out.join(format!("place-{n:03}-{i}.jpg"));
        write_jpeg(&rgba, cli.picture_size, cli.picture_size, &path)?;
        pictures.push(path.display().to_string());
        details.push(detail);
    }
    let detail = details.iter().sum::<f64>() / details.len() as f64;
    let detail_min = details.iter().copied().fold(f64::INFINITY, f64::min);
    Ok(PlaceEvent {
        n,
        re: re.to_string(),
        im: im.to_string(),
        period,
        end,
        interest: interest_percent(&interest, detail, detail_min),
        events: interest.events,
        events_per_decade: interest.events_per_decade,
        close_share: interest.close_share,
        longest_gap: interest.longest_gap,
        detail,
        detail_min,
        level,
        pictures,
    })
}

/// One frame, as RGBA in `palette`, and its detail: the mean difference
/// between neighbouring pixels' grey levels (0 = flat, about 0.3 = noise).
fn picture(gpu: &GpuCtx, re: &str, im: &str, span: f64, period: u32, size: u32, palette: ColorScheme) -> anyhow::Result<(Vec<u8>, f64)> {
    let seq = SequenceConfig::default();
    // A minibrot of period p needs some multiple of p iterations to show.
    let max_iter = seq.max_iter_for_span(span).max(period.saturating_mul(4)).min(2_000_000);
    let req = FrameRequest {
        frame_idx: 0,
        width: size,
        height: size,
        center_re: re.to_string(),
        center_im: im.to_string(),
        span,
        max_iter,
        max_retries: 0,
        stop_flag: None,
        progress: None,
        deep_engine: DeepEngine::default(),
        interior_exit: true,
    };
    let frame = render_frame(gpu, &req)?;
    let rgb = encode_rgb(&frame.pixels, frame.width, frame.height, frame.max_iter, palette);
    let rgba: Vec<u8> = rgb.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    let grey = encode_rgb(&frame.pixels, frame.width, frame.height, frame.max_iter, ColorScheme::Grayscale);
    Ok((rgba, detail(&grey, frame.width, frame.height)))
}

fn detail(rgb: &[u8], w: u32, h: u32) -> f64 {
    let (w, h) = (w as usize, h as usize);
    let g = |x: usize, y: usize| f64::from(rgb[(y * w + x) * 3]);
    let mut sum = 0.0;
    let mut count = 0usize;
    for y in 0..h {
        for x in 0..w {
            if x + 1 < w {
                sum += (g(x + 1, y) - g(x, y)).abs();
                count += 1;
            }
            if y + 1 < h {
                sum += (g(x, y + 1) - g(x, y)).abs();
                count += 1;
            }
        }
    }
    sum / count.max(1) as f64 / 255.0
}

/// Interest, 0–100. Provisional until calibrated against rated places: the
/// detail of the pictures down the zoom, with the least detailed counting
/// double, scaled so 0.15 is 100.
pub fn interest_percent(_ladder: &Interest, detail: f64, detail_min: f64) -> f64 {
    ((detail + 2.0 * detail_min) / 3.0 / 0.15 * 100.0).clamp(0.0, 100.0)
}
