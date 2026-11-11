//! How busy this machine's processors are with other programs' work (`SystemLoad.swift`): the
//! speed check starts once the machine has settled (just after the model's download a virus
//! scanner reads the new files, and after a login everything starts at once), and its reply says
//! what else ran while it timed. A check timed at such a moment took one PC for thirty times
//! slower than it was.
//!
//! The Linux reading, as the engine's: the first line of `/proc/stat` (user nice system idle
//! iowait irq softirq steal — the guests are inside user) and `getrusage` for this process's own
//! processor seconds.

use std::time::{Duration, Instant};

/// `SystemLoad.Sample`: the processors' busy and total time since the machine started (100 ns
/// ticks over all processors, the same unit for both), and this process's own processor seconds.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub busy: f64,
    pub total: f64,
    pub own: f64,
    pub time: Instant,
}

/// The number of logical processors (`activeProcessorCount`).
fn processors() -> f64 {
    std::thread::available_parallelism()
        .map(|n| n.get() as f64)
        .unwrap_or(1.0)
}

/// `SystemLoad.sample`.
pub fn sample() -> Option<Sample> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        let line = stat.lines().next()?;
        let fields: Vec<f64> = line
            .split_whitespace()
            .skip(1)
            .filter_map(|field| field.parse().ok())
            .collect();
        if fields.len() < 5 {
            return None;
        }
        let idle = fields[3] + fields[4];
        let busy: f64 = fields.iter().take(8).sum::<f64>() - idle;
        Some(Sample {
            busy,
            total: busy + idle,
            own: own_seconds(),
            time: Instant::now(),
        })
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// This process's own processor seconds (`ru_utime + ru_stime`).
#[cfg(target_os = "linux")]
fn own_seconds() -> f64 {
    let mut usage = unsafe { std::mem::zeroed::<libc::rusage>() };
    // RUSAGE_SELF: the glibc constant is the C enum's 0.
    if unsafe { libc::getrusage(0, &mut usage) } != 0 {
        return 0.0;
    }
    let time = |val: libc::c_long, micro: libc::c_long| val as f64 + micro as f64 / 1e6;
    time(
        usage.ru_utime.tv_sec,
        usage.ru_utime.tv_usec as libc::c_long,
    ) + time(
        usage.ru_stime.tv_sec,
        usage.ru_stime.tv_usec as libc::c_long,
    )
}

/// `SystemLoad.others`: processors' worth of work other programs did between two samples
/// (1 is one processor kept busy throughout).
pub fn others(from: &Sample, to: &Sample) -> f64 {
    let wall = to.time.duration_since(from.time).as_secs_f64();
    let total = to.total - from.total;
    if wall <= 0.0 || total <= 0.0 {
        return 0.0;
    }
    let busy = (to.busy - from.busy) / total * processors();
    (busy - (to.own - from.own) / wall).max(0.0)
}

/// `SystemLoad.quiet`: other programs' work this machine shrugs off — a processor's half, plus
/// a little on bigger machines.
pub fn quiet() -> f64 {
    0.5 + 0.05 * processors()
}

/// `SystemLoad.waitForQuiet`: until other programs' work over the last three seconds is under
/// `quiet`, at most `limit` seconds. Says how long it waited, whether the machine settled, and
/// the least work other programs did over three seconds meanwhile.
pub fn wait_for_quiet(limit: f64) -> (f64, bool, Option<f64>) {
    let started = Instant::now();
    let Some(mut last) = sample() else {
        return (0.0, true, None);
    };
    let mut recent: Vec<f64> = Vec::new();
    let mut calmest: Option<f64> = None;
    while started.elapsed().as_secs_f64() < limit {
        std::thread::sleep(Duration::from_secs(1));
        let Some(next) = sample() else { break };
        recent.push(others(&last, &next));
        if recent.len() > 3 {
            recent.remove(0);
        }
        last = next;
        if recent.len() != 3 {
            continue;
        }
        let average = recent.iter().sum::<f64>() / 3.0;
        calmest = Some(calmest.map_or(average, |least: f64| least.min(average)));
        if average < quiet() {
            return (started.elapsed().as_secs_f64(), true, calmest);
        }
    }
    (started.elapsed().as_secs_f64(), false, calmest)
}
