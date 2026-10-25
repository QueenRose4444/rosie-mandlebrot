//! Live hardware telemetry for `progress.json`.
//!
//! * **GPU utilization** via NVML (nvml-wrapper dlopens libnvidia-ml at runtime,
//!   so this compiles everywhere and just reports `None` where NVML is absent,
//!   e.g. a machine without an NVIDIA driver, or a CPU-only container).
//! * **Process CPU usage** read straight from `/proc/self/stat` on Linux — no
//!   extra dependency, and cgroup-correct because it's *our*
//!   process's user+system ticks, normalized against the `--cpu-threads` budget.
//!
//! Both degrade gracefully to `None`; telemetry never affects the render.

use std::time::Instant;

use nvml_wrapper::Nvml;

/// USER_HZ on Linux x86_64 — clock ticks per second for /proc CPU times. It's
/// effectively always 100; reading sysconf(_SC_CLK_TCK) would need libc, and a
/// telemetry stat doesn't warrant the dependency.
#[cfg(target_os = "linux")]
const CLK_TCK: f64 = 100.0;

pub struct SysStats {
    nvml: Option<Nvml>,
    /// CPU-thread budget (`--cpu-threads`), used to normalize process CPU usage
    /// into a 0–100% "fraction of allotted cores" reading.
    // Read only on Linux, where CPU usage comes from /proc/self/stat. On other
    // platforms these are written and never read, which is correct, not dead.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    cpu_budget: f64,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    last_cpu_ticks: Option<u64>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    last_cpu_instant: Option<Instant>,
    /// Highest resident memory seen, where the OS does not keep it for us.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    peak_rss: u64,
}

impl SysStats {
    pub fn new(cpu_budget: usize) -> Self {
        let nvml = match Nvml::init() {
            Ok(n) => Some(n),
            Err(e) => {
                tracing::debug!("NVML unavailable ({e}); GPU utilization omitted from progress");
                None
            }
        };
        Self {
            nvml,
            cpu_budget: cpu_budget.max(1) as f64,
            last_cpu_ticks: None,
            last_cpu_instant: None,
            peak_rss: 0,
        }
    }

    /// Whole-GPU utilization 0–100. It is a whole-card figure, so it reflects
    /// our load only when the card is dedicated to the render. `None` if NVML
    /// isn't available.
    pub fn gpu_util(&self) -> Option<f64> {
        let nvml = self.nvml.as_ref()?;
        let dev = nvml.device_by_index(0).ok()?;
        let u = dev.utilization_rates().ok()?;
        Some(u.gpu as f64)
    }

    /// Resident memory now and the peak so far, in bytes. On Linux the peak is
    /// the kernel's own high-water mark (`VmHWM`), which catches spikes between
    /// samples; elsewhere it is the largest value this has sampled.
    pub fn memory(&mut self) -> Option<(u64, u64)> {
        #[cfg(target_os = "linux")]
        {
            let status = std::fs::read_to_string("/proc/self/status").ok()?;
            let kb = |key: &str| -> Option<u64> {
                let line = status.lines().find(|l| l.starts_with(key))?;
                line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|v| v * 1024)
            };
            Some((kb("VmRSS:")?, kb("VmHWM:")?))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let now = memory_stats::memory_stats()?.physical_mem as u64;
            self.peak_rss = self.peak_rss.max(now);
            Some((now, self.peak_rss))
        }
    }

    /// Render-process CPU usage as a percentage of its `--cpu-threads` budget
    /// (0–100). Needs two samples to diff, so the first call returns `None`.
    /// Linux only (reads `/proc`); `None` elsewhere.
    pub fn cpu_util(&mut self) -> Option<f64> {
        #[cfg(target_os = "linux")]
        {
            let now = Instant::now();
            let ticks = read_proc_cpu_ticks()?;
            let result = match (self.last_cpu_ticks, self.last_cpu_instant) {
                (Some(lt), Some(li)) => {
                    let dt = now.duration_since(li).as_secs_f64();
                    if dt <= 0.0 {
                        None
                    } else {
                        let cpu_secs = ticks.saturating_sub(lt) as f64 / CLK_TCK;
                        let cores_used = cpu_secs / dt;
                        Some((cores_used / self.cpu_budget * 100.0).clamp(0.0, 100.0))
                    }
                }
                _ => None,
            };
            self.last_cpu_ticks = Some(ticks);
            self.last_cpu_instant = Some(now);
            result
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }
}

/// utime + stime (clock ticks) for this process from `/proc/self/stat`. The
/// `comm` field can contain spaces/parens, so we parse fields after the final
/// ')': token 0 = state (field 3), so utime (field 14) = token 11, stime = 12.
#[cfg(target_os = "linux")]
fn read_proc_cpu_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let after = &stat[stat.rfind(')')? + 1..];
    let toks: Vec<&str> = after.split_whitespace().collect();
    let utime: u64 = toks.get(11)?.parse().ok()?;
    let stime: u64 = toks.get(12)?.parse().ok()?;
    Some(utime + stime)
}
