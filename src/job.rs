//! 일괄 처리: 폴더 탐색, 메모리 예산 안에서의 동시 실행, 진행률과 남은 시간.

use crate::Result;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// 동시에 처리하는 사진들의 예상 메모리 합을 한도 안으로 묶는다.
///
/// 메모리가 바닥나면 프로그램이 오류 없이 강제 종료되므로, 부족해지기 전에 시작을 막는다.
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

/// 확보한 예산. 버리면 반납된다.
pub struct Permit<'a> {
    budget: &'a Budget,
    cost: u64,
}

impl Budget {
    /// `limit`은 바이트, `max_jobs`는 동시에 처리할 최대 장수.
    pub fn new(limit: u64, max_jobs: usize) -> Self {
        Self {
            limit,
            max_jobs,
            state: Mutex::default(),
            changed: Condvar::new(),
        }
    }

    /// `cost`만큼 확보될 때까지 기다린다. 한도보다 큰 작업은 다른 작업이 없을 때 혼자 실행된다.
    // ponytail: 먼저 온 순서를 지키지 않는다. 큰 작업은 작은 작업들에 밀려 배치 끝으로 갈 수 있다.
    pub fn acquire(&self, cost: u64) -> Permit<'_> {
        let mut state = self.state.lock().unwrap();
        loop {
            let fits = state.running < self.max_jobs
                && !state.low_memory
                && state.used.saturating_add(cost) <= self.limit;
            // 아무것도 돌고 있지 않으면 무조건 하나는 시작한다. 그래야 전체가 멈추지 않는다.
            if fits || state.running == 0 {
                state.used = state.used.saturating_add(cost);
                state.running += 1;
                return Permit { budget: self, cost };
            }
            state = self.changed.wait(state).unwrap();
        }
    }

    /// 시스템의 여유 메모리가 부족한지 알린다. 부족한 동안에는 새 작업을 한 번에 하나만 시작한다.
    pub fn set_low_memory(&self, low: bool) {
        self.state.lock().unwrap().low_memory = low;
        self.changed.notify_all();
    }

    /// 지금 처리 중인 장수.
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

/// 남은 시간을 계산할 때 보는 최근 구간(초).
const ETA_WINDOW: f64 = 60.0;
/// 이보다 짧게 지켜본 속도는 믿지 않는다(초).
const ETA_MIN_SPAN: f64 = 5.0;

/// 남은 시간 계산. 시간은 초 단위 숫자로 받는다.
pub struct Eta {
    /// (시각, 끝난 양). 맨 앞은 최근 구간이 시작되기 직전의 기록이다.
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

    /// 지금까지 끝난 양(장 단위, 처리 중인 사진의 부분 진행 포함)을 기록한다.
    pub fn record(&mut self, now: f64, done: f64) {
        if self.history.back().is_some_and(|&(_, last)| done > last) {
            self.last_change = now;
        }
        self.history.push_back((now, done));
        while self.history.len() > 2 && self.history[1].0 <= now - ETA_WINDOW {
            self.history.pop_front();
        }
    }

    /// 최근 속도로 계산한 남은 시간(초). 아직 알 수 없으면 None.
    pub fn remaining(&self, total: f64) -> Option<f64> {
        let (&(t0, d0), &(t1, d1)) = (self.history.front()?, self.history.back()?);
        if t1 - t0 < ETA_MIN_SPAN || d1 <= d0 {
            return None;
        }
        Some((total - d1) * (t1 - t0) / (d1 - d0))
    }

    /// 마지막으로 진행이 있은 뒤 흐른 시간(초). 느린 것과 멈춘 것을 구분하는 데 쓴다.
    pub fn idle_for(&self, now: f64) -> f64 {
        now - self.last_change
    }
}

/// 진행 막대 옆에 쓰는 문구. 30초 넘게 진행이 없으면 남은 시간 대신 그 사실을 알린다.
pub fn describe_remaining(remaining: Option<f64>, idle: f64) -> String {
    if idle > STALL_AFTER {
        return format!("응답 대기 중 (마지막 진행 {idle:.0}초 전)");
    }
    match remaining {
        None => "남은 시간 계산 중".into(),
        Some(s) if s < 60.0 => format!("약 {:.0}초 남음", (s / 10.0).ceil() * 10.0),
        Some(s) if s < 3600.0 => format!("약 {:.0}분 남음", s / 60.0),
        Some(s) => format!("약 {}시간 {}분 남음", s as u64 / 3600, s as u64 % 3600 / 60),
    }
}

/// 이만큼(초) 진행 신호가 없으면 멈춘 것일 수 있다고 알린다.
const STALL_AFTER: f64 = 30.0;

/// 사진 한 장의 변환 작업.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub input: PathBuf,
    pub output: PathBuf,
}

/// 사진 한 장을 실제로 변환하는 함수. 두 번째 인자로 그 사진의 진행 정도(0~1)를 알린다.
pub type Work<'a> = dyn Fn(&Task, &dyn Fn(f32)) -> Result<()> + Sync + 'a;

/// 배치 하나를 돌리는 데 필요한 것들.
pub struct Batch<'a> {
    pub tasks: &'a [Task],
    /// 동시에 일하는 스레드 수.
    pub workers: usize,
    pub budget: &'a Budget,
    /// 출력 파일이 이미 있어도 다시 만든다. 끄면 건너뛴다 (중단된 배치를 이어 하기 좋다).
    pub overwrite: bool,
    /// 켜지면 새 사진을 시작하지 않는다.
    pub cancel: &'a AtomicBool,
    /// 사진 한 장에 필요한 메모리(바이트)의 추정값.
    pub cost: &'a (dyn Fn(&Task) -> Result<u64> + Sync),
    /// 지금 시스템에 남아 있는 메모리(바이트).
    pub available: &'a (dyn Fn() -> u64 + Sync),
    pub work: &'a Work<'a>,
}

/// 진행 상황. 주기적으로 `tick`에 전달된다.
#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub total: usize,
    /// 끝난 장수 (성공, 건너뜀, 실패 모두).
    pub finished: usize,
    /// 끝난 양. 처리 중인 사진의 부분 진행을 포함한다.
    pub progress: f64,
    /// 지금 처리 중인 장수.
    pub running: usize,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub done: usize,
    pub skipped: usize,
    /// 실패한 입력 파일과 이유.
    pub failed: Vec<(PathBuf, String)>,
    /// 배치가 끝까지 가지 못했으면 그 이유.
    pub stopped: Option<String>,
}

/// 배치를 실행한다. 한 장이 실패해도 나머지는 계속한다. `tick`은 0.2초마다, 그리고 끝날 때 한 번 불린다.
pub fn run_batch(batch: &Batch, mut tick: impl FnMut(&Status)) -> Outcome {
    let total = batch.tasks.len();
    let workers = batch.workers.clamp(1, total.max(1));
    let next = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    // 일꾼마다 지금 잡고 있는 사진의 진행 정도 (천분율)
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
                                    outcome.stopped = Some("디스크가 가득 찼습니다".into());
                                }
                                outcome.failed.push((task.input.clone(), error.to_string()));
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
        outcome.stopped = Some("취소했습니다".into());
    }
    tick(&Status {
        total,
        finished: finished.load(SeqCst),
        progress: finished.load(SeqCst) as f64,
        running: 0,
    });
    outcome
}

/// `tick`을 부르는 간격.
const TICK_INTERVAL: Duration = Duration::from_millis(200);

enum Step {
    Done,
    Skipped,
    /// 기다리는 사이에 배치가 멈춰서 시작하지 않았다.
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
    // 한도보다 커서 혼자 실행되는 사진이다. 그래도 남은 메모리에 안 들어가면 시작하지 않는다.
    let available = (batch.available)();
    if cost > batch.budget.limit && cost > available {
        const GB: f64 = (1u64 << 30) as f64;
        return Err(format!(
            "메모리 부족 (필요 {:.1}GB, 사용 가능 {:.1}GB)",
            cost as f64 / GB,
            available as f64 / GB
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

/// 폴더와 하위 폴더에서 RAW 파일을 찾는다. 결과는 `dir` 기준 상대 경로이고 정렬돼 있다.
pub fn scan(dir: &Path) -> Result<Vec<PathBuf>> {
    fn walk(root: &Path, dir: &Path, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            // 심볼릭 링크로 연결된 폴더는 따라가지 않는다 (순환 방지).
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

/// 입력의 상대 경로에 대응하는 출력 경로. 하위 폴더 구조를 그대로 따른다.
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
            "30짜리 작업이 동시에 3개까지 돌아야 한다"
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
            assert!(!started.load(SeqCst), "세 번째 작업은 기다려야 한다");
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
                "다른 작업이 있는 동안에는 시작하지 않는다"
            );
            drop(small);
            thread::sleep(TICK);
            assert!(
                big_started.load(SeqCst),
                "혼자 남으면 한도를 넘어도 시작한다"
            );

            s.spawn(|| {
                let _late = budget.acquire(10);
                late_started.store(true, SeqCst);
            });
            thread::sleep(TICK);
            assert!(
                !late_started.load(SeqCst),
                "큰 작업이 도는 동안에는 다른 작업이 끼어들지 않는다"
            );
            big_release.store(true, SeqCst);
        });
        assert!(late_started.load(SeqCst));
    }

    #[test]
    fn low_memory_lets_only_one_job_run_at_a_time() {
        let budget = Budget::new(100, 16);
        budget.set_low_memory(true);
        let first = budget.acquire(10); // 아무것도 안 돌고 있으면 하나는 시작한다 (전부 멈추지는 않는다)
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
                "메모리가 회복되면 다시 동시에 돈다"
            );
            drop(first);
        });
    }

    #[test]
    fn steady_speed_gives_the_right_remaining_time() {
        let mut eta = Eta::new(0.0);
        for t in 0..=30 {
            eta.record(t as f64, t as f64 * 0.5); // 초당 0.5장
        }
        let remaining = eta.remaining(100.0).unwrap(); // 85장 남음
        assert!((remaining - 170.0).abs() < 1.0, "{remaining}");
    }

    #[test]
    fn no_estimate_without_enough_history() {
        let mut eta = Eta::new(0.0);
        assert_eq!(eta.remaining(100.0), None);
        eta.record(1.0, 0.5);
        assert_eq!(eta.remaining(100.0), None, "1초 지켜본 것으로는 알 수 없다");

        let mut eta = Eta::new(0.0);
        eta.record(15.0, 0.0);
        eta.record(30.0, 0.0);
        assert_eq!(eta.remaining(100.0), None, "진행이 없으면 남은 시간도 없다");
    }

    #[test]
    fn estimate_follows_the_recent_speed() {
        let mut eta = Eta::new(0.0);
        for t in 0..=100 {
            eta.record(t as f64, t as f64); // 처음에는 초당 1장
        }
        for t in 101..=200 {
            eta.record(t as f64, 100.0 + (t - 100) as f64 * 0.25); // 그 뒤로 초당 0.25장
        }
        let remaining = eta.remaining(225.0).unwrap(); // 100장 남음
        assert!((remaining - 400.0).abs() < 5.0, "{remaining}");
    }

    #[test]
    fn idle_time_counts_from_the_last_real_progress() {
        let mut eta = Eta::new(0.0);
        eta.record(10.0, 5.0);
        eta.record(20.0, 5.0); // 값이 그대로면 진행이 아니다
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

    /// 테스트용 배치 환경: 임시 폴더와 작업 목록.
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

    /// 출력 파일을 실제로 쓰는 가짜 변환.
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
        let mut failed = outcome.failed;
        failed.sort();
        assert_eq!(
            failed,
            [
                (PathBuf::from("bad2.ARW"), "읽을 수 없는 파일".to_string()),
                ("sub/bad1.ARW".into(), "읽을 수 없는 파일".into())
            ]
        );
        assert!(
            fx.dir.join("sub/c.heic").exists(),
            "하위 폴더도 만들어서 쓴다"
        );
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
                fx.cancel.store(true, SeqCst); // 첫 장을 처리하는 도중에 취소
                write_output(task, progress)
            },
            |_| {},
        );
        assert_eq!(outcome.done, 1, "하던 사진은 끝낸다");
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
        assert!(outcome.stopped.unwrap().contains("디스크"));
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
                // 첫 장이 절반까지 온 것을 본 뒤에 놓아준다 (첫 tick은 일꾼이 시작하기 전일 수 있다).
                if status.progress >= 0.5 {
                    release.store(true, SeqCst);
                }
            },
        );
        assert_eq!(outcome.done, 2);
        let mid = seen.iter().find(|status| status.running == 1).unwrap();
        assert_eq!((mid.total, mid.finished), (2, 0));
        assert_eq!(mid.progress, 0.5, "첫 장을 절반 처리한 상태");
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
        assert_eq!(file, Path::new("huge.ARW"));
        assert!(
            reason.contains("메모리") && reason.contains("40.0") && reason.contains("16.0"),
            "{reason}"
        );
    }
}
