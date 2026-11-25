//! Batch processing: folder scanning, concurrent runs within a memory budget, progress and time
//! left.

use crate::Result;
use crate::lang::{Text, t, text};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// Keeps the estimated memory of the photos being processed at once within a limit.
///
/// Running out of memory kills the program without an error, so starts are held back before it
/// runs short.
pub struct Budget {
    limit: u64,
    max_jobs: usize,
    state: Mutex<BudgetState>,
    changed: Condvar,
}

#[derive(Default)]
struct BudgetState {
    used: u64,
    running: usize,
    low_memory: bool,
}

/// Reserved budget. Returned when dropped.
pub struct Permit<'a> {
    budget: &'a Budget,
    cost: u64,
}

impl Budget {
    /// `limit` is in bytes; `max_jobs` is the most photos processed at once.
    pub fn new(limit: u64, max_jobs: usize) -> Self {
        Self {
            limit,
            max_jobs,
            state: Mutex::default(),
            changed: Condvar::new(),
        }
    }

    /// Waits until `cost` can be reserved. A job larger than the limit runs alone when nothing else
    /// is running.
    // ponytail: not first-come-first-served. A large job can be pushed to the end of the batch by
    // smaller ones.
    pub fn acquire(&self, cost: u64) -> Permit<'_> {
        let mut state = self.state.lock().unwrap();
        loop {
            let fits = state.running < self.max_jobs
                && !state.low_memory
                && state.used.saturating_add(cost) <= self.limit;
            // If nothing is running, always start one. Otherwise the whole batch could stall.
            if fits || state.running == 0 {
                state.used = state.used.saturating_add(cost);
                state.running += 1;
                return Permit { budget: self, cost };
            }
            state = self.changed.wait(state).unwrap();
        }
    }

    /// Reports whether the system is low on free memory. While it is, new jobs start one at a time.
    pub fn set_low_memory(&self, low: bool) {
        self.state.lock().unwrap().low_memory = low;
        self.changed.notify_all();
    }

    /// Number of photos being processed now.
    pub fn running(&self) -> usize {
        self.state.lock().unwrap().running
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut state = self.budget.state.lock().unwrap();
        state.used = state.used.saturating_sub(self.cost);
        state.running -= 1;
        self.budget.changed.notify_all();
    }
}

/// Recent window used to compute the time left (seconds).
const ETA_WINDOW: f64 = 60.0;
/// A speed observed for less than this is not trusted (seconds).
const ETA_MIN_SPAN: f64 = 5.0;

/// Time-left estimate. Times are given as seconds.
pub struct Eta {
    /// (time, amount done). The front entry is the last record before the recent window starts.
    history: VecDeque<(f64, f64)>,
    last_change: f64,
}

impl Eta {
    pub fn new(now: f64) -> Self {
        Self {
            history: VecDeque::from([(now, 0.0)]),
            last_change: now,
        }
    }

    /// Records the amount done so far (in photos, including partial progress on photos in flight).
    pub fn record(&mut self, now: f64, done: f64) {
        if self.history.back().is_some_and(|&(_, last)| done > last) {
            self.last_change = now;
        }
        self.history.push_back((now, done));
        while self.history.len() > 2 && self.history[1].0 <= now - ETA_WINDOW {
            self.history.pop_front();
        }
    }

    /// Time left at the recent speed (seconds). None if it can't be known yet.
    pub fn remaining(&self, total: f64) -> Option<f64> {
        let (&(t0, d0), &(t1, d1)) = (self.history.front()?, self.history.back()?);
        if t1 - t0 < ETA_MIN_SPAN || d1 <= d0 {
            return None;
        }
        Some((total - d1) * (t1 - t0) / (d1 - d0))
    }

    /// Time since the last progress (seconds). Used to tell slow from stalled.
    pub fn idle_for(&self, now: f64) -> f64 {
        now - self.last_change
    }
}

/// Text shown next to the progress bar. With no progress for over 30 seconds, says so instead of
/// the time left.
pub fn describe_remaining(remaining: Option<f64>, idle: f64) -> String {
    if idle > STALL_AFTER {
        return t(
            format!("응답 대기 중 (마지막 진행 {idle:.0}초 전)"),
            format!("waiting for a response (last progress {idle:.0} s ago)"),
        );
    }
    match remaining {
        None => t("남은 시간 계산 중", "estimating time left").into(),
        Some(s) if s < 60.0 => {
            let seconds = (s / 10.0).ceil() * 10.0;
            t(
                format!("약 {seconds:.0}초 남음"),
                format!("about {seconds:.0} s left"),
            )
        }
        Some(s) if s < 3600.0 => {
            let minutes = s / 60.0;
            t(
                format!("약 {minutes:.0}분 남음"),
                format!("about {minutes:.0} min left"),
            )
        }
        Some(s) => {
            let (hours, minutes) = (s as u64 / 3600, s as u64 % 3600 / 60);
            t(
                format!("약 {hours}시간 {minutes}분 남음"),
                format!("about {hours} h {minutes} min left"),
            )
        }
    }
}

/// One-line resource usage. Memory is given in bytes.
pub fn describe_resources(
    cpu: f32,
    memory: u64,
    memory_limit: u64,
    gpu: f32,
    running: usize,
    low_memory: bool,
) -> String {
    const GB: f64 = (1u64 << 30) as f64;
    let (used, limit) = (memory as f64 / GB, memory_limit as f64 / GB);
    let mut text = t(
        format!(
            "CPU {cpu:.0}% · 메모리 {used:.1}/{limit:.1}GB · GPU {gpu:.0}% · 처리 중 {running}장"
        ),
        format!(
            "CPU {cpu:.0}% · Memory {used:.1}/{limit:.1} GB · GPU {gpu:.0}% · Working on {running}"
        ),
    );
    if low_memory {
        text.push_str(t(
            " · 메모리가 부족해 동시 처리 수를 줄였습니다",
            " · Low on memory, so fewer photos run at once",
        ));
    }
    text
}

/// With no progress signal for this long (seconds), warn that it may be stalled.
const STALL_AFTER: f64 = 30.0;

/// Conversion task for one photo.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub input: PathBuf,
    pub output: PathBuf,
}

/// Function that actually converts one photo. Reports that photo's progress (0..1) through the
/// second argument.
pub type Work<'a> = dyn Fn(&Task, &dyn Fn(f32)) -> Result<()> + Sync + 'a;

/// Everything needed to run one batch.
pub struct Batch<'a> {
    pub tasks: &'a [Task],
    /// Number of worker threads.
    pub workers: usize,
    pub budget: &'a Budget,
    /// Recreate output files even if they already exist. When off, they are skipped (handy for
    /// resuming an interrupted batch).
    pub overwrite: bool,
    /// When set, no new photos are started.
    pub cancel: &'a AtomicBool,
    /// Estimated memory needed for one photo (bytes).
    pub cost: &'a (dyn Fn(&Task) -> Result<u64> + Sync),
    /// Memory currently left on the system (bytes).
    pub available: &'a (dyn Fn() -> u64 + Sync),
    pub work: &'a Work<'a>,
}

/// Progress. Passed to `tick` periodically.
#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub total: usize,
    /// Photos finished (done, skipped and failed).
    pub finished: usize,
    /// Amount done. Includes partial progress on photos in flight.
    pub progress: f64,
    /// Number of photos being processed now.
    pub running: usize,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Outcome {
    pub done: usize,
    pub skipped: usize,
    /// Failed input files and why.
    pub failed: Vec<(PathBuf, Text)>,
    /// Why the batch didn't run to the end, if it didn't.
    pub stopped: Option<Text>,
}

/// Runs the batch. One photo failing doesn't stop the rest. `tick` is called every 0.2 s and once
/// at the end.
pub fn run_batch(batch: &Batch, mut tick: impl FnMut(&Status)) -> Outcome {
    let total = batch.tasks.len();
    let workers = batch.workers.clamp(1, total.max(1));
    let next = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    // Progress of each worker's current photo (per mille)
    let partial: Vec<AtomicU32> = (0..workers).map(|_| AtomicU32::new(0)).collect();
    let outcome = Mutex::new(Outcome::default());
    let stop = AtomicBool::new(false);

    let status = || Status {
        total,
        finished: finished.load(SeqCst),
        progress: finished.load(SeqCst) as f64
            + partial
                .iter()
                .map(|p| p.load(SeqCst) as f64 / 1000.0)
                .sum::<f64>(),
        running: batch.budget.running(),
    };

    std::thread::scope(|scope| {
        let handles: Vec<_> = partial
            .iter()
            .map(|partial| {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, SeqCst);
                        if index >= total || stop.load(SeqCst) || batch.cancel.load(SeqCst) {
                            break;
                        }
                        let task = &batch.tasks[index];
                        let result = run_one(batch, task, &stop, partial);
                        partial.store(0, SeqCst);
                        let mut outcome = outcome.lock().unwrap();
                        match result {
                            Ok(Step::Done) => outcome.done += 1,
                            Ok(Step::Skipped) => outcome.skipped += 1,
                            Ok(Step::NotStarted) => {}
                            Err(error) => {
                                if is_disk_full(error.as_ref()) {
                                    stop.store(true, SeqCst);
                                    outcome.stopped =
                                        Some(text!("디스크가 가득 찼습니다", "The disk is full"));
                                }
                                outcome.failed.push((task.input.clone(), Text::of(&error)));
                            }
                        }
                        finished.fetch_add(1, SeqCst);
                    }
                })
            })
            .collect();

        while !handles.iter().all(|h| h.is_finished()) {
            tick(&status());
            std::thread::sleep(TICK_INTERVAL);
        }
    });

    let mut outcome = outcome.into_inner().unwrap();
    if outcome.stopped.is_none()
        && batch.cancel.load(SeqCst)
        && outcome.done + outcome.skipped + outcome.failed.len() < total
    {
        outcome.stopped = Some(text!("취소했습니다", "Cancelled"));
    }
    tick(&Status {
        total,
        finished: finished.load(SeqCst),
        progress: finished.load(SeqCst) as f64,
        running: 0,
    });
    outcome
}

/// Interval between `tick` calls.
const TICK_INTERVAL: Duration = Duration::from_millis(200);

enum Step {
    Done,
    Skipped,
    /// The batch stopped while waiting, so it never started.
    NotStarted,
}

fn run_one(batch: &Batch, task: &Task, stop: &AtomicBool, partial: &AtomicU32) -> Result<Step> {
    if !batch.overwrite && task.output.exists() {
        return Ok(Step::Skipped);
    }
    let cost = (batch.cost)(task)?;
    let _permit = batch.budget.acquire(cost);
    if stop.load(SeqCst) || batch.cancel.load(SeqCst) {
        return Ok(Step::NotStarted);
    }
    // This photo exceeds the limit and runs alone. Still, if it doesn't fit in free memory, don't
    // start it.
    let available = (batch.available)();
    if cost > batch.budget.limit && cost > available {
        const GB: f64 = (1u64 << 30) as f64;
        let (needed, free) = (cost as f64 / GB, available as f64 / GB);
        return Err(text!(
            format!("메모리 부족 (필요 {needed:.1}GB, 사용 가능 {free:.1}GB)"),
            format!("Not enough memory (needs {needed:.1} GB, {free:.1} GB available)"),
        )
        .into());
    }
    if let Some(parent) = task.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    (batch.work)(task, &|fraction| {
        partial.store((fraction.clamp(0.0, 1.0) * 1000.0) as u32, SeqCst)
    })?;
    Ok(Step::Done)
}

fn is_disk_full(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|io| io.kind() == std::io::ErrorKind::StorageFull)
}

/// Finds RAW files in a folder and its subfolders. Results are paths relative to `dir`, sorted.
pub fn scan(dir: &Path) -> Result<Vec<PathBuf>> {
    fn walk(root: &Path, dir: &Path, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            // Don't follow symlinked folders (avoids cycles).
            if entry.file_type()?.is_dir() {
                walk(root, &path, found)?;
            } else if path.is_file() && is_raw(&path) {
                found.push(path.strip_prefix(root).unwrap_or(&path).to_owned());
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    walk(dir, dir, &mut found)?;
    found.sort();
    Ok(found)
}

fn is_raw(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            rawler::decoders::supported_extensions().contains(&ext.to_uppercase().as_str())
        })
}

/// Output path for an input's relative path. Mirrors the subfolder structure.
pub fn output_path(out_dir: &Path, relative: &Path) -> PathBuf {
    out_dir.join(relative).with_extension("heic")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::thread;

    const TICK: Duration = Duration::from_millis(30);

    #[test]
    fn concurrent_jobs_stay_within_the_budget() {
        let budget = Budget::new(100, 16);
        let (in_use, peak) = (AtomicU64::new(0), AtomicU64::new(0));
        thread::scope(|s| {
            for _ in 0..12 {
                s.spawn(|| {
                    let _permit = budget.acquire(30);
                    let now = in_use.fetch_add(30, SeqCst) + 30;
                    peak.fetch_max(now, SeqCst);
                    thread::sleep(Duration::from_millis(5));
                    in_use.fetch_sub(30, SeqCst);
                });
            }
        });
        assert_eq!(
            peak.load(SeqCst),
            90,
            "up to three jobs of 30 should run at once"
        );
    }

    #[test]
    fn job_count_is_capped() {
        let budget = Budget::new(u64::MAX, 2);
        let (_a, _b) = (budget.acquire(1), budget.acquire(1));
        let started = AtomicBool::new(false);
        thread::scope(|s| {
            s.spawn(|| {
                let _c = budget.acquire(1);
                started.store(true, SeqCst);
            });
            thread::sleep(TICK);
            assert!(!started.load(SeqCst), "the third job should wait");
            drop(_a);
        });
        assert!(started.load(SeqCst));
    }

    #[test]
    fn a_job_larger_than_the_budget_runs_alone() {
        let budget = Budget::new(100, 16);
        let small = budget.acquire(30);
        let (big_started, big_release, late_started) = (
            AtomicBool::new(false),
            AtomicBool::new(false),
            AtomicBool::new(false),
        );
        thread::scope(|s| {
            s.spawn(|| {
                let _big = budget.acquire(500);
                big_started.store(true, SeqCst);
                while !big_release.load(SeqCst) {
                    thread::sleep(Duration::from_millis(1));
                }
            });
            thread::sleep(TICK);
            assert!(
                !big_started.load(SeqCst),
                "does not start while other jobs are running"
            );
            drop(small);
            thread::sleep(TICK);
            assert!(big_started.load(SeqCst), "starts alone even over the limit");

            s.spawn(|| {
                let _late = budget.acquire(10);
                late_started.store(true, SeqCst);
            });
            thread::sleep(TICK);
            assert!(
                !late_started.load(SeqCst),
                "no other job cuts in while the large one runs"
            );
            big_release.store(true, SeqCst);
        });
        assert!(late_started.load(SeqCst));
    }

    #[test]
    fn low_memory_lets_only_one_job_run_at_a_time() {
        let budget = Budget::new(100, 16);
        budget.set_low_memory(true);
        let first = budget.acquire(10); // With nothing running, one still starts (no full stall)
        let second_started = AtomicBool::new(false);
        thread::scope(|s| {
            s.spawn(|| {
                let _second = budget.acquire(10);
                second_started.store(true, SeqCst);
            });
            thread::sleep(TICK);
            assert!(!second_started.load(SeqCst));
            budget.set_low_memory(false);
            thread::sleep(TICK);
            assert!(
                second_started.load(SeqCst),
                "runs concurrently again once memory recovers"
            );
            drop(first);
        });
    }

    #[test]
    fn steady_speed_gives_the_right_remaining_time() {
        let mut eta = Eta::new(0.0);
        for t in 0..=30 {
            eta.record(t as f64, t as f64 * 0.5); // 0.5 photos per second
        }
        let remaining = eta.remaining(100.0).unwrap(); // 85 photos left
        assert!((remaining - 170.0).abs() < 1.0, "{remaining}");
    }

    #[test]
    fn no_estimate_without_enough_history() {
        let mut eta = Eta::new(0.0);
        assert_eq!(eta.remaining(100.0), None);
        eta.record(1.0, 0.5);
        assert_eq!(
            eta.remaining(100.0),
            None,
            "one second of history is not enough"
        );

        let mut eta = Eta::new(0.0);
        eta.record(15.0, 0.0);
        eta.record(30.0, 0.0);
        assert_eq!(eta.remaining(100.0), None, "no progress means no time left");
    }

    #[test]
    fn estimate_follows_the_recent_speed() {
        let mut eta = Eta::new(0.0);
        for t in 0..=100 {
            eta.record(t as f64, t as f64); // 1 photo per second at first
        }
        for t in 101..=200 {
            eta.record(t as f64, 100.0 + (t - 100) as f64 * 0.25); // then 0.25 photos per second
        }
        let remaining = eta.remaining(225.0).unwrap(); // 100 photos left
        assert!((remaining - 400.0).abs() < 5.0, "{remaining}");
    }

    #[test]
    fn idle_time_counts_from_the_last_real_progress() {
        let mut eta = Eta::new(0.0);
        eta.record(10.0, 5.0);
        eta.record(20.0, 5.0); // An unchanged value is not progress
        assert_eq!(eta.idle_for(50.0), 40.0);
        eta.record(51.0, 5.5);
        assert_eq!(eta.idle_for(52.0), 1.0);
    }

    #[test]
    fn remaining_time_reads_naturally() {
        assert_eq!(describe_remaining(Some(40.0), 0.0), "약 40초 남음");
        assert_eq!(describe_remaining(Some(372.0), 0.0), "약 6분 남음");
        assert_eq!(describe_remaining(Some(3900.0), 0.0), "약 1시간 5분 남음");
        assert_eq!(describe_remaining(None, 2.0), "남은 시간 계산 중");
    }

    #[test]
    fn a_stall_is_reported_instead_of_a_remaining_time() {
        assert_eq!(
            describe_remaining(Some(372.0), 45.0),
            "응답 대기 중 (마지막 진행 45초 전)"
        );
        assert_eq!(
            describe_remaining(None, 31.0),
            "응답 대기 중 (마지막 진행 31초 전)"
        );
    }

    #[test]
    fn scan_finds_raw_files_in_subfolders_and_nothing_else() {
        let dir = std::env::temp_dir().join(format!("amsil-scan-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub/deeper")).unwrap();
        for name in [
            "b.ARW",
            "a.arw",
            "a.JPG",
            "a.ARW.xmp",
            "sub/c.NEF",
            "sub/deeper/d.dng",
            "sub/note.txt",
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        let found = scan(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let expected: Vec<PathBuf> = ["a.arw", "b.ARW", "sub/c.NEF", "sub/deeper/d.dng"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(found, expected);
    }

    #[test]
    fn output_keeps_the_folder_structure() {
        let out = output_path(Path::new("/out"), Path::new("10161107/DSC04307.ARW"));
        assert_eq!(out, Path::new("/out/10161107/DSC04307.heic"));
    }

    /// Test batch setup: a temp folder and a task list.
    struct Fixture {
        dir: PathBuf,
        tasks: Vec<Task>,
        budget: Budget,
        cancel: AtomicBool,
    }

    impl Fixture {
        fn new(name: &str, inputs: &[&str]) -> Self {
            let dir =
                std::env::temp_dir().join(format!("amsil-batch-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let tasks = inputs
                .iter()
                .map(|i| Task {
                    input: i.into(),
                    output: output_path(&dir, Path::new(i)),
                })
                .collect();
            Self {
                dir,
                tasks,
                budget: Budget::new(u64::MAX, 64),
                cancel: AtomicBool::new(false),
            }
        }

        fn run(
            &self,
            workers: usize,
            overwrite: bool,
            work: &Work,
            tick: impl FnMut(&Status),
        ) -> Outcome {
            let batch = Batch {
                tasks: &self.tasks,
                workers,
                budget: &self.budget,
                overwrite,
                cancel: &self.cancel,
                cost: &|_| Ok(1),
                available: &|| u64::MAX,
                work,
            };
            run_batch(&batch, tick)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Fake conversion that actually writes the output file.
    fn write_output(task: &Task, _: &dyn Fn(f32)) -> Result<()> {
        Ok(std::fs::write(&task.output, b"heic")?)
    }

    #[test]
    fn a_failing_file_does_not_stop_the_rest() {
        let fx = Fixture::new(
            "fail",
            &["a.ARW", "sub/bad1.ARW", "sub/c.ARW", "bad2.ARW", "e.ARW"],
        );
        let outcome = fx.run(
            3,
            false,
            &|task, progress| {
                if task.input.to_string_lossy().contains("bad") {
                    return Err("읽을 수 없는 파일".into());
                }
                write_output(task, progress)
            },
            |_| {},
        );
        assert_eq!(
            (outcome.done, outcome.skipped, outcome.stopped),
            (3, 0, None)
        );
        let mut failed: Vec<(PathBuf, String)> = outcome
            .failed
            .into_iter()
            .map(|(file, reason)| (file, reason.to_string()))
            .collect();
        failed.sort();
        assert_eq!(
            failed,
            [
                (PathBuf::from("bad2.ARW"), "읽을 수 없는 파일".to_string()),
                ("sub/bad1.ARW".into(), "읽을 수 없는 파일".into())
            ]
        );
        assert!(fx.dir.join("sub/c.heic").exists(), "creates subfolders too");
    }

    #[test]
    fn existing_outputs_are_skipped_unless_overwriting() {
        let fx = Fixture::new("skip", &["a.ARW", "b.ARW", "c.ARW"]);
        std::fs::create_dir_all(&fx.dir).unwrap();
        std::fs::write(fx.dir.join("b.heic"), b"old").unwrap();

        let outcome = fx.run(2, false, &write_output, |_| {});
        assert_eq!((outcome.done, outcome.skipped), (2, 1));
        assert_eq!(std::fs::read(fx.dir.join("b.heic")).unwrap(), b"old");

        let outcome = fx.run(2, true, &write_output, |_| {});
        assert_eq!((outcome.done, outcome.skipped), (3, 0));
        assert_eq!(std::fs::read(fx.dir.join("b.heic")).unwrap(), b"heic");
    }

    #[test]
    fn cancelling_stops_before_the_next_file() {
        let fx = Fixture::new("cancel", &["a.ARW", "b.ARW", "c.ARW", "d.ARW"]);
        let outcome = fx.run(
            1,
            false,
            &|task, progress| {
                fx.cancel.store(true, SeqCst); // Cancel while the first photo is in progress
                write_output(task, progress)
            },
            |_| {},
        );
        assert_eq!(outcome.done, 1, "finishes the photo in progress");
        assert!(outcome.stopped.is_some());
        assert!(!fx.dir.join("b.heic").exists());
    }

    #[test]
    fn a_full_disk_stops_the_batch_instead_of_failing_every_file() {
        let fx = Fixture::new("full", &["a.ARW", "b.ARW", "c.ARW"]);
        let calls = AtomicU64::new(0);
        let outcome = fx.run(
            1,
            false,
            &|_, _| {
                calls.fetch_add(1, SeqCst);
                Err(std::io::Error::from(std::io::ErrorKind::StorageFull).into())
            },
            |_| {},
        );
        assert_eq!(calls.load(SeqCst), 1);
        assert_eq!(outcome.failed.len(), 1);
        assert!(outcome.stopped.unwrap().to_string().contains("디스크"));
    }

    #[test]
    fn progress_includes_the_photo_being_worked_on() {
        let fx = Fixture::new("progress", &["a.ARW", "b.ARW"]);
        let release = AtomicBool::new(false);
        let mut seen = Vec::new();
        let outcome = fx.run(
            1,
            false,
            &|task, progress| {
                progress(0.5);
                while !release.load(SeqCst) {
                    thread::sleep(Duration::from_millis(1));
                }
                write_output(task, progress)
            },
            |status| {
                seen.push(*status);
                // Release once the first photo is seen halfway (the first tick may come before the
                // worker starts).
                if status.progress >= 0.5 {
                    release.store(true, SeqCst);
                }
            },
        );
        assert_eq!(outcome.done, 2);
        let mid = seen.iter().find(|status| status.running == 1).unwrap();
        assert_eq!((mid.total, mid.finished), (2, 0));
        assert_eq!(mid.progress, 0.5, "first photo half done");
        let last = seen.last().unwrap();
        assert_eq!((last.finished, last.progress, last.running), (2, 2.0, 0));
    }

    #[test]
    fn a_photo_too_big_for_the_remaining_memory_fails_with_the_numbers() {
        let fx = Fixture::new("toobig", &["huge.ARW", "ok.ARW"]);
        let budget = Budget::new(8 << 30, 4);
        let batch = Batch {
            tasks: &fx.tasks,
            workers: 1,
            budget: &budget,
            overwrite: false,
            cancel: &fx.cancel,
            cost: &|task| {
                Ok(if task.input.starts_with("huge.ARW") {
                    40 << 30
                } else {
                    1 << 30
                })
            },
            available: &|| 16 << 30,
            work: &write_output,
        };
        let outcome = run_batch(&batch, |_| {});
        assert_eq!(outcome.done, 1);
        assert_eq!(outcome.failed.len(), 1);
        let (file, reason) = &outcome.failed[0];
        let reason = reason.to_string();
        assert_eq!(file, Path::new("huge.ARW"));
        assert!(
            reason.contains("메모리") && reason.contains("40.0") && reason.contains("16.0"),
            "{reason}"
        );
    }

    #[test]
    fn remaining_time_is_described_in_english_too() {
        crate::lang::set(crate::lang::Lang::En);
        assert_eq!(describe_remaining(Some(40.0), 0.0), "about 40 s left");
        assert_eq!(describe_remaining(Some(372.0), 0.0), "about 6 min left");
        assert_eq!(
            describe_remaining(Some(3900.0), 0.0),
            "about 1 h 5 min left"
        );
        assert_eq!(describe_remaining(None, 2.0), "estimating time left");
        assert_eq!(
            describe_remaining(Some(372.0), 45.0),
            "waiting for a response (last progress 45 s ago)"
        );
    }
}
