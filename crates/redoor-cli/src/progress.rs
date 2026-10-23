//! A single redrawn stderr line for `cp`, so long transfers show their state without
//! scrolling the terminal.

use std::{
    collections::VecDeque,
    io::Write,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// Shared between a running transfer and the progress line, so drawing never waits on IO.
#[derive(Default)]
pub(crate) struct TransferCounters {
    /// Payload bytes moved so far; for directories only file content, not archive framing.
    pub(crate) bytes: AtomicU64,
    /// Expected payload bytes. Zero means unknown, for example while a directory is still
    /// being measured, and hides the percentage instead of showing a wrong one.
    pub(crate) total: AtomicU64,
}

/// Redraws one line in place. Speed comes from a short window of samples so it follows the
/// current throughput instead of the average since start.
pub(crate) struct ProgressLine {
    /// Off for `--quiet`, `--json` and non-terminal stderr, where a carriage-return line
    /// would only clutter logs.
    enabled: bool,
    started: Instant,
    samples: VecDeque<(Instant, u64)>,
    /// Whether a line is on screen that `clear` has to erase.
    drawn: bool,
}

impl ProgressLine {
    /// Long enough to smooth chunk bursts, short enough to react to a stalled transfer.
    const SPEED_WINDOW: Duration = Duration::from_secs(3);

    /// Starts the clock now so the completion summary covers the whole command.
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            started: Instant::now(),
            samples: VecDeque::new(),
            drawn: false,
        }
    }

    /// Samples the counters and replaces the current line with the new state.
    pub(crate) fn draw(&mut self, counters: &TransferCounters) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        let bytes = counters.bytes.load(Ordering::Relaxed);
        self.samples.push_back((now, bytes));
        while self.samples.len() > 2
            && self
                .samples
                .front()
                .is_some_and(|(at, _)| now.duration_since(*at) > Self::SPEED_WINDOW)
        {
            self.samples.pop_front();
        }
        let speed = self
            .samples
            .front()
            .map(|(at, first)| rate(bytes.saturating_sub(*first), now.duration_since(*at)))
            .unwrap_or(0);
        let total = counters.total.load(Ordering::Relaxed);
        let line = if total > 0 {
            let percent = (u128::from(bytes) * 100 / u128::from(total)).min(100);
            format!(
                "Copying {} / {} ({percent}%) {}/s",
                format_bytes(bytes),
                format_bytes(total),
                format_bytes(speed)
            )
        } else {
            format!("Copying {} {}/s", format_bytes(bytes), format_bytes(speed))
        };
        let mut stderr = std::io::stderr().lock();
        // Progress is best effort; a closed stderr must not fail the transfer.
        let _ = write!(stderr, "\r\x1b[2K{line}");
        let _ = stderr.flush();
        self.drawn = true;
    }

    /// Erases the line so the completion message or an error starts on a clean line.
    pub(crate) fn clear(&mut self) {
        if self.drawn {
            let mut stderr = std::io::stderr().lock();
            let _ = write!(stderr, "\r\x1b[2K");
            let _ = stderr.flush();
            self.drawn = false;
        }
    }

    /// Summarizes the finished copy with its average speed over the whole command.
    pub(crate) fn summary(&self, bytes: u64) -> String {
        let elapsed = self.started.elapsed();
        format!(
            "Copy completed: {} in {:.1}s ({}/s)",
            format_bytes(bytes),
            elapsed.as_secs_f64(),
            format_bytes(rate(bytes, elapsed))
        )
    }
}

/// Bytes per second, treating an empty interval as no measurable speed yet.
fn rate(bytes: u64, elapsed: Duration) -> u64 {
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        0
    } else {
        (bytes as f64 / seconds) as u64
    }
}

/// Binary units with one decimal, the same scale `ls -h` and `du -h` use.
pub(crate) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit boundaries decide which scale a reader sees, so pin them down.
    #[test]
    fn formats_bytes_with_binary_units() {
        assert_eq!(format_bytes(0), "0 B", "small sizes stay exact");
        assert_eq!(format_bytes(1023), "1023 B", "below one KiB stays in bytes");
        assert_eq!(format_bytes(1024), "1.0 KiB", "one KiB switches unit");
        assert_eq!(
            format_bytes(1536 * 1024),
            "1.5 MiB",
            "fractions keep one decimal"
        );
        assert_eq!(
            format_bytes(u64::MAX),
            "16.0 EiB",
            "the largest value must not run past the unit table"
        );
    }

    /// A zero interval happens on the first sample and must not divide by zero.
    #[test]
    fn rate_handles_empty_interval() {
        assert_eq!(rate(100, Duration::ZERO), 0, "no time means no speed yet");
        assert_eq!(
            rate(2048, Duration::from_secs(2)),
            1024,
            "rate is bytes per second"
        );
    }
}
