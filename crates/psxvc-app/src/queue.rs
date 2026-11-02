//! A queue of exports, run one at a time on a background thread.
//!
//! One at a time because FFmpeg's encoders already use every core, and it keeps
//! the order predictable. A job that fails doesn't stop the ones after it.

use anyhow::{Context, Result};
use psx_disc::{BinCueDisc, RawSector, SectorSource};
use psx_export::{default_output_name, export_stream, ExportOptions};
use psx_index::AudioStream;
use serde::Serialize;
use std::cell::Cell;
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::disc::Disc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Running {
        percent: u32,
    },
    Done {
        output: String,
        frames: usize,
        warnings: Vec<String>,
    },
    Failed {
        error: String,
    },
    Cancelled,
}

impl JobState {
    fn finished(&self) -> bool {
        !matches!(self, JobState::Queued | JobState::Running { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobView {
    pub id: u64,
    pub label: String,
    #[serde(flatten)]
    pub state: JobState,
}

/// What a finished job reports.
#[derive(Debug, Clone, Default)]
pub struct Finished {
    pub output: PathBuf,
    pub frames: usize,
    pub warnings: Vec<String>,
}

/// A job's view of the queue while it runs.
pub struct Control<'a> {
    cancel: &'a AtomicBool,
    progress: &'a dyn Fn(usize, usize),
}

impl Control<'_> {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn progress(&self, done: usize, total: usize) {
        (self.progress)(done, total);
    }
}

pub type Work = Box<dyn FnOnce(&Control) -> Result<Finished> + Send>;
type Notify = Box<dyn Fn(&[JobView]) + Send + Sync>;

#[derive(Default)]
struct Inner {
    jobs: Vec<JobView>,
    pending: VecDeque<(u64, Work)>,
    running: Option<u64>,
    worker: Option<JoinHandle<()>>,
    worker_active: bool,
    next_id: u64,
}

struct Shared {
    inner: Mutex<Inner>,
    /// Stops the running job. Only changed with `inner` locked, so that a job
    /// can't start between a cancel being decided and it being signalled.
    cancel: AtomicBool,
    /// Called with every change, with `inner` locked so that the changes arrive
    /// in order. It mustn't call back into the queue.
    notify: Notify,
}

impl Shared {
    fn set_state(&self, id: u64, state: JobState) {
        let inner = &mut *self.inner.lock().unwrap();
        if let Some(job) = inner.jobs.iter_mut().find(|j| j.id == id) {
            job.state = state;
        }
        (self.notify)(&inner.jobs);
    }
}

pub struct Queue {
    shared: Arc<Shared>,
}

impl Queue {
    pub fn new(notify: impl Fn(&[JobView]) + Send + Sync + 'static) -> Self {
        Queue {
            shared: Arc::new(Shared {
                inner: Mutex::default(),
                cancel: AtomicBool::new(false),
                notify: Box::new(notify),
            }),
        }
    }

    pub fn jobs(&self) -> Vec<JobView> {
        self.shared.inner.lock().unwrap().jobs.clone()
    }

    /// Add a job to the end of the queue, starting the worker if it's idle.
    pub fn push(&self, label: String, work: Work) -> u64 {
        let inner = &mut *self.shared.inner.lock().unwrap();
        inner.next_id += 1;
        let id = inner.next_id;
        inner.jobs.push(JobView {
            id,
            label,
            state: JobState::Queued,
        });
        inner.pending.push_back((id, work));
        (self.shared.notify)(&inner.jobs);
        if !inner.worker_active {
            inner.worker_active = true;
            let shared = self.shared.clone();
            let previous = inner.worker.take();
            inner.worker = Some(std::thread::spawn(move || {
                // It has already decided to exit, so this doesn't wait long.
                if let Some(previous) = previous {
                    let _ = previous.join();
                }
                work_through(&shared);
            }));
        }
        id
    }

    /// Cancel a job. A queued one never starts. A running one stops soon after and
    /// leaves no file behind, or finishes if it was nearly done anyway.
    pub fn cancel(&self, id: u64) {
        let inner = &mut *self.shared.inner.lock().unwrap();
        if inner.running == Some(id) {
            self.shared.cancel.store(true, Ordering::Relaxed);
        } else {
            let before = inner.pending.len();
            inner.pending.retain(|(pending, _)| *pending != id);
            if inner.pending.len() != before {
                if let Some(job) = inner.jobs.iter_mut().find(|j| j.id == id) {
                    job.state = JobState::Cancelled;
                }
                (self.shared.notify)(&inner.jobs);
            }
        }
    }

    /// Forget the jobs that have finished.
    pub fn clear_finished(&self) {
        let inner = &mut *self.shared.inner.lock().unwrap();
        inner.jobs.retain(|j| !j.state.finished());
        (self.shared.notify)(&inner.jobs);
    }

    /// Cancel everything and wait for the running job to clean up after itself.
    pub fn shutdown(&self) {
        let worker = {
            let inner = &mut *self.shared.inner.lock().unwrap();
            for (id, _) in std::mem::take(&mut inner.pending) {
                if let Some(job) = inner.jobs.iter_mut().find(|j| j.id == id) {
                    job.state = JobState::Cancelled;
                }
            }
            if inner.running.is_some() {
                self.shared.cancel.store(true, Ordering::Relaxed);
            }
            inner.worker.take()
        };
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

fn work_through(shared: &Shared) {
    loop {
        let (id, work) = {
            let inner = &mut *shared.inner.lock().unwrap();
            let Some(next) = inner.pending.pop_front() else {
                inner.running = None;
                inner.worker_active = false;
                return;
            };
            inner.running = Some(next.0);
            shared.cancel.store(false, Ordering::Relaxed);
            next
        };
        shared.set_state(id, JobState::Running { percent: 0 });

        let last_percent = Cell::new(0);
        let progress = |done: usize, total: usize| {
            let percent = (done as u64 * 100 / total.max(1) as u64) as u32;
            if last_percent.replace(percent) != percent {
                shared.set_state(id, JobState::Running { percent });
            }
        };
        let control = Control {
            cancel: &shared.cancel,
            progress: &progress,
        };
        // A panic on a badly damaged disc shouldn't take the rest of the queue with it.
        let outcome = catch_unwind(AssertUnwindSafe(|| work(&control)));
        let state = match outcome {
            Ok(Ok(f)) => JobState::Done {
                output: f.output.display().to_string(),
                frames: f.frames,
                warnings: f.warnings,
            },
            Ok(Err(_)) if control.cancelled() => JobState::Cancelled,
            Ok(Err(e)) => JobState::Failed {
                error: format!("{e:#}"),
            },
            Err(_) => JobState::Failed {
                error: "the export crashed; see the app's log for why".into(),
            },
        };
        let inner = &mut *shared.inner.lock().unwrap();
        inner.running = None;
        if let Some(job) = inner.jobs.iter_mut().find(|j| j.id == id) {
            job.state = state;
        }
        (shared.notify)(&inner.jobs);
    }
}

/// A disc that starts failing once the job is cancelled. `export_stream` already
/// handles a failed read by closing FFmpeg's pipe and deleting the partial file.
struct Cancellable<'a> {
    disc: BinCueDisc,
    control: &'a Control<'a>,
}

impl Cancellable<'_> {
    fn check(&self) -> psx_disc::Result<()> {
        if self.control.cancelled() {
            return Err(psx_disc::Error::Unsupported("cancelled".into()));
        }
        Ok(())
    }
}

impl SectorSource for Cancellable<'_> {
    fn sector_count(&self) -> u32 {
        self.disc.sector_count()
    }

    fn read_sector(&mut self, lba: u32) -> psx_disc::Result<RawSector> {
        self.check()?;
        self.disc.read_sector(lba)
    }

    fn read_range(&mut self, start: u32, count: u32) -> psx_disc::Result<Vec<RawSector>> {
        self.check()?;
        self.disc.read_range(start, count)
    }
}

/// `folder/name.ext`, or `folder/name-2.ext` and so on if that's taken, so that a
/// queue can't silently replace an earlier export.
pub fn unused_path(folder: &Path, name: &Path) -> PathBuf {
    let first = folder.join(name);
    if !first.exists() {
        return first;
    }
    let stem = name.file_stem().unwrap_or_default().to_string_lossy();
    let ext = name.extension().unwrap_or_default().to_string_lossy();
    (2u32..)
        .map(|n| folder.join(format!("{stem}-{n}.{ext}")))
        .find(|p| !p.exists())
        .expect("some number is free")
}

/// The work of exporting one video stream and its audio into a folder.
pub fn export_work(
    disc: Arc<Disc>,
    stream: usize,
    options: ExportOptions,
    folder: PathBuf,
) -> Work {
    Box::new(move |control| {
        let video = disc.video(stream)?;
        let audio: Vec<&AudioStream> = disc
            .index
            .audio
            .iter()
            .filter(|a| video.audio.contains(&a.id))
            .collect();
        std::fs::create_dir_all(&folder)
            .with_context(|| format!("creating {}", folder.display()))?;
        // Chosen now rather than when queued, so it sees what earlier jobs wrote.
        let name = default_output_name(&disc.path, video, options.preset);
        let output = unused_path(&folder, &name);
        let source = BinCueDisc::open(&disc.path)
            .with_context(|| format!("opening {}", disc.path.display()))?;
        let mut source = Cancellable {
            disc: source,
            control,
        };
        let report = export_stream(
            &mut source,
            video,
            &audio,
            &options,
            &output,
            |done, total| control.progress(done, total),
        )?;
        Ok(Finished {
            output,
            frames: report.frames_written,
            warnings: report.warnings,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;
    use std::sync::mpsc;
    use std::time::Duration;

    fn finished() -> Result<Finished> {
        Ok(Finished::default())
    }

    /// A queue, and a channel that gets the jobs after each change.
    fn watched() -> (Queue, mpsc::Receiver<Vec<JobView>>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let queue = Queue::new(move |jobs| {
            let _ = tx.lock().unwrap().send(jobs.to_vec());
        });
        (queue, rx)
    }

    /// Read changes until the jobs satisfy `done`.
    fn wait_for(
        rx: &mpsc::Receiver<Vec<JobView>>,
        done: impl Fn(&[JobView]) -> bool,
    ) -> Vec<JobView> {
        loop {
            let jobs = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("the queue went quiet too soon");
            if done(&jobs) {
                return jobs;
            }
        }
    }

    fn all_finished(jobs: &[JobView]) -> bool {
        jobs.iter().all(|j| j.state.finished())
    }

    #[test]
    fn jobs_run_in_order_and_a_failure_doesnt_stop_the_rest() {
        let (queue, rx) = watched();
        let order = Arc::new(Mutex::new(Vec::new()));
        let job = |n: u32, fail: bool| -> Work {
            let order = order.clone();
            Box::new(move |_| {
                order.lock().unwrap().push(n);
                if fail {
                    bail!("job {n} went wrong");
                }
                finished()
            })
        };
        queue.push("one".into(), job(1, false));
        queue.push("two".into(), job(2, true));
        queue.push("three".into(), job(3, false));
        let jobs = wait_for(&rx, |j| j.len() == 3 && all_finished(j));
        assert_eq!(*order.lock().unwrap(), [1, 2, 3]);
        assert!(matches!(jobs[0].state, JobState::Done { .. }));
        assert_eq!(
            jobs[1].state,
            JobState::Failed {
                error: "job 2 went wrong".into()
            }
        );
        assert!(matches!(jobs[2].state, JobState::Done { .. }));
    }

    #[test]
    fn a_panic_fails_its_job_and_the_queue_carries_on() {
        let (queue, rx) = watched();
        queue.push("bad".into(), Box::new(|_| panic!("damaged disc")));
        queue.push("good".into(), Box::new(|_| finished()));
        let jobs = wait_for(&rx, |j| j.len() == 2 && all_finished(j));
        assert!(matches!(jobs[0].state, JobState::Failed { .. }));
        assert!(matches!(jobs[1].state, JobState::Done { .. }));
    }

    #[test]
    fn progress_is_reported_as_it_changes() {
        let (queue, rx) = watched();
        queue.push(
            "job".into(),
            Box::new(|c| {
                c.progress(1, 4);
                c.progress(1, 4);
                c.progress(2, 4);
                finished()
            }),
        );
        let mut percents = Vec::new();
        loop {
            let jobs = rx.recv_timeout(Duration::from_secs(10)).unwrap();
            if let JobState::Running { percent } = jobs[0].state {
                percents.push(percent);
            }
            if all_finished(&jobs) {
                break;
            }
        }
        assert_eq!(percents, [0, 25, 50]);
    }

    #[test]
    fn cancelling_a_queued_job_means_it_never_runs() {
        let (queue, rx) = watched();
        let (release, gate) = mpsc::channel::<()>();
        let ran = Arc::new(AtomicBool::new(false));
        let first = queue.push(
            "first".into(),
            Box::new(move |_| {
                let _ = gate.recv();
                finished()
            }),
        );
        let ran2 = ran.clone();
        let second = queue.push(
            "second".into(),
            Box::new(move |_| {
                ran2.store(true, Ordering::Relaxed);
                finished()
            }),
        );
        queue.cancel(second);
        release.send(()).unwrap();
        let jobs = wait_for(&rx, |j| j.len() == 2 && all_finished(j));
        assert_eq!(jobs[0].id, first);
        assert!(matches!(jobs[0].state, JobState::Done { .. }));
        assert_eq!(jobs[1].state, JobState::Cancelled);
        assert!(!ran.load(Ordering::Relaxed));
    }

    #[test]
    fn cancelling_a_running_job_signals_it_and_the_next_job_still_runs() {
        let (queue, rx) = watched();
        let first = queue.push(
            "first".into(),
            Box::new(|c| {
                while !c.cancelled() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                bail!("stopped");
            }),
        );
        queue.push(
            "second".into(),
            Box::new(|c| {
                assert!(!c.cancelled(), "a cancel only applies to its own job");
                finished()
            }),
        );
        wait_for(&rx, |j| matches!(j[0].state, JobState::Running { .. }));
        queue.cancel(first);
        let jobs = wait_for(&rx, |j| j.len() == 2 && all_finished(j));
        assert_eq!(jobs[0].state, JobState::Cancelled);
        assert!(matches!(jobs[1].state, JobState::Done { .. }));
    }

    #[test]
    fn the_queue_starts_again_after_going_idle() {
        let (queue, rx) = watched();
        queue.push("first".into(), Box::new(|_| finished()));
        wait_for(&rx, all_finished);
        // Wait for the worker to exit, or this would just join its loop.
        while queue.shared.inner.lock().unwrap().worker_active {
            std::thread::sleep(Duration::from_millis(1));
        }
        queue.push("second".into(), Box::new(|_| finished()));
        let jobs = wait_for(&rx, |j| j.len() == 2 && all_finished(j));
        assert!(matches!(jobs[1].state, JobState::Done { .. }));
    }

    #[test]
    fn clearing_keeps_unfinished_jobs() {
        let (queue, rx) = watched();
        let (release, gate) = mpsc::channel::<()>();
        queue.push("done".into(), Box::new(|_| finished()));
        queue.push(
            "running".into(),
            Box::new(move |_| {
                let _ = gate.recv();
                finished()
            }),
        );
        wait_for(&rx, |j| {
            j.len() == 2 && j[0].state.finished() && matches!(j[1].state, JobState::Running { .. })
        });
        queue.clear_finished();
        let left = queue.jobs();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].label, "running");
        release.send(()).unwrap();
    }

    #[test]
    fn shutdown_cancels_everything_and_waits() {
        let (queue, rx) = watched();
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = stopped.clone();
        queue.push(
            "running".into(),
            Box::new(move |c| {
                while !c.cancelled() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                flag.store(true, Ordering::Relaxed);
                bail!("stopped");
            }),
        );
        queue.push("queued".into(), Box::new(|_| finished()));
        wait_for(&rx, |j| {
            j.len() == 2 && matches!(j[0].state, JobState::Running { .. })
        });
        queue.shutdown();
        assert!(stopped.load(Ordering::Relaxed), "it waited for the job");
        let jobs = queue.jobs();
        assert_eq!(jobs[0].state, JobState::Cancelled);
        assert_eq!(jobs[1].state, JobState::Cancelled);
    }

    #[test]
    fn unused_path_never_picks_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let name = Path::new("PE-3.mkv");
        assert_eq!(unused_path(dir.path(), name), dir.path().join("PE-3.mkv"));
        std::fs::write(dir.path().join("PE-3.mkv"), b"").unwrap();
        assert_eq!(unused_path(dir.path(), name), dir.path().join("PE-3-2.mkv"));
        std::fs::write(dir.path().join("PE-3-2.mkv"), b"").unwrap();
        assert_eq!(unused_path(dir.path(), name), dir.path().join("PE-3-3.mkv"));
    }
}
