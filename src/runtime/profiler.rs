//! Mimi runtime profiler — tracks function call counts and durations.
//!
//! Enabled via `mimi run --profile` or `MIMI_PROFILE=1` environment variable.
//! Outputs a sorted table of function calls with counts, total time, and avg time.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// A single function's profiling data.
#[derive(Debug, Clone)]
pub struct ProfileEntry {
    pub call_count: u64,
    pub total_ns: u64,
    pub min_ns: u64,
    pub max_ns: u64,
}

impl ProfileEntry {
    fn new() -> Self {
        Self {
            call_count: 0,
            total_ns: 0,
            min_ns: u64::MAX,
            max_ns: 0,
        }
    }

    fn record(&mut self, duration_ns: u64) {
        self.call_count += 1;
        self.total_ns += duration_ns;
        if duration_ns < self.min_ns {
            self.min_ns = duration_ns;
        }
        if duration_ns > self.max_ns {
            self.max_ns = duration_ns;
        }
    }

    pub fn avg_ns(&self) -> u64 {
        if self.call_count == 0 {
            0
        } else {
            self.total_ns / self.call_count
        }
    }
}

/// Global profiler state.
static PROFILER: Mutex<Option<Profiler>> = Mutex::new(None);

struct Profiler {
    entries: HashMap<String, ProfileEntry>,
    enabled: bool,
}

impl Profiler {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            enabled: true,
        }
    }
}

/// Initialize the profiler. Call once at program start.
pub fn profiler_init() {
    if let Ok(mut guard) = PROFILER.lock() {
        *guard = Some(Profiler::new());
    }
}

/// Check if profiling is enabled.
pub fn profiler_is_enabled() -> bool {
    PROFILER
        .lock()
        .ok()
        .and_then(|p| p.as_ref().map(|pr| pr.enabled))
        .unwrap_or(false)
}

/// Record a function call with its duration.
pub fn profiler_record(name: &str, duration_ns: u64) {
    let Ok(mut guard) = PROFILER.lock() else { return };
    if let Some(profiler) = guard.as_mut() {
        if profiler.enabled {
            let entry = profiler.entries.entry(name.to_string()).or_insert_with(ProfileEntry::new);
            entry.record(duration_ns);
        }
    }
}

/// RAII guard for timing a function call.
pub struct ProfileTimer {
    name: String,
    start: Instant,
}

impl ProfileTimer {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            start: Instant::now(),
        }
    }
}

impl Drop for ProfileTimer {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed().as_nanos() as u64;
        profiler_record(&self.name, elapsed);
    }
}

/// Print the profiling report to stderr.
pub fn profiler_report() {
    let Ok(guard) = PROFILER.lock() else { return };
    let profiler = match guard.as_ref() {
        Some(p) => p,
        None => return,
    };

    if profiler.entries.is_empty() {
        eprintln!("\n=== Mimi Profile Report ===");
        eprintln!("No function calls recorded.");
        return;
    }

    let mut entries: Vec<(&String, &ProfileEntry)> = profiler.entries.iter().collect();
    entries.sort_by(|a, b| b.1.total_ns.cmp(&a.1.total_ns));

    eprintln!("\n=== Mimi Profile Report ===");
    eprintln!("{:<40} {:>10} {:>14} {:>14} {:>14}",
        "Function", "Calls", "Total (ms)", "Avg (ms)", "Max (ms)");
    eprintln!("{}", "-".repeat(96));

    let mut total_calls = 0u64;
    let mut total_time = 0u64;

    for (name, entry) in entries.iter().take(50) {
        total_calls += entry.call_count;
        total_time += entry.total_ns;
        eprintln!("{:<40} {:>10} {:>14.3} {:>14.3} {:>14.3}",
            name,
            entry.call_count,
            entry.total_ns as f64 / 1_000_000.0,
            entry.avg_ns() as f64 / 1_000_000.0,
            entry.max_ns as f64 / 1_000_000.0,
        );
    }

    eprintln!("{}", "-".repeat(96));
    eprintln!("{:<40} {:>10} {:>14.3}",
        "TOTAL",
        total_calls,
        total_time as f64 / 1_000_000.0,
    );
}
