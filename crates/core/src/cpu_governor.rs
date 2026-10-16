//! Sizes the number of PNG encode/save worker threads so they don't compete with
//! whatever else is running on the machine for CPU — and, just as
//! importantly, caps how many frames are encoded concurrently. Each in-flight
//! frame holds a full-resolution `PixelOut` buffer (~134 MB at 4K), so the
//! worker count plus the bounded encode channel together bound peak RAM.

/// Number of dedicated encoder threads. `0` = auto: half the cores, clamped to
/// 1..=4 (keeps headroom for other services and a container RAM cap).
pub fn encode_thread_count(requested: usize) -> usize {
    if requested == 0 {
        (num_cpus::get() / 2).clamp(1, 4)
    } else {
        requested.max(1)
    }
}

/// Number of threads for the CPU render paths (the rug glitch fallback and the
/// CPU perturbation tiers).
///
/// `0` = auto: every core but one.
///
/// Leaving a core free is the difference between a machine you can still use and
/// one that stops responding. Rayon's global pool defaults to *all* cores, and
/// the rug fallback is the heaviest CPU work in the project, so without a limit
/// a deep frame with glitches would take the whole machine.
///
/// One core is a deliberately small reservation: it costs a few percent of
/// throughput and keeps a desktop responsive. Anyone who wants the machine
/// saturated can ask for it explicitly.
pub fn render_thread_count(requested: usize) -> usize {
    if requested == 0 {
        num_cpus::get().saturating_sub(1).max(1)
    } else {
        requested.max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_leaves_a_core_free() {
        let n = render_thread_count(0);
        let total = num_cpus::get();
        assert!(n >= 1, "must always give at least one thread");
        if total > 1 {
            assert_eq!(n, total - 1, "auto should reserve exactly one core");
        } else {
            assert_eq!(n, 1, "a single-core machine still gets one thread");
        }
    }

    #[test]
    fn an_explicit_request_is_honoured_even_if_it_saturates() {
        // A dedicated machine can ask for every core, and get it.
        assert_eq!(render_thread_count(64), 64);
        // But zero threads is not a thing.
        assert_eq!(render_thread_count(1), 1);
    }
}
