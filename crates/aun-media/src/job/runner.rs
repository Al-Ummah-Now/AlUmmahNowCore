//! Runs a [`JobPlan`] on a backend with progress, pause/resume and cancellation.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use super::backend::{EncodeBackend, StageArtifact, StageReporter, StageTask};
use super::plan::{JobPlan, JobStage, NodeOp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Lifecycle and progress notifications.
#[derive(Debug, Clone, PartialEq)]
pub enum JobEvent {
    StateChanged {
        state: JobState,
        detail: Option<String>,
    },
    StageStarted {
        stage_id: String,
        index: u32,
        count: u32,
    },
    Progress {
        stage_id: String,
        stage_fraction: f64,
        overall_fraction: f64,
    },
    StageFinished {
        stage_id: String,
    },
}

/// Receives [`JobEvent`]s from a worker thread: keep it cheap, and hop to the UI thread natively.
/// Calling [`JobHandle`] methods from here is allowed.
pub trait JobListener: Send + Sync {
    fn on_event(&self, event: JobEvent);
}

/// A finished job's final files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOutcome {
    pub job_id: String,
    pub outputs: Vec<String>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug)]
struct Control {
    state: JobState,
    pause_requested: bool,
    cancel_requested: bool,
    started: bool,
}

/// Runs a [`JobPlan`] on one backend.
pub struct JobRunner {
    backend: Arc<dyn EncodeBackend>,
}

impl std::fmt::Debug for JobRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobRunner").finish_non_exhaustive()
    }
}

impl JobRunner {
    pub fn new(backend: Arc<dyn EncodeBackend>) -> Self {
        Self { backend }
    }

    /// Creates a job without starting it, so listeners can capture the handle first.
    pub fn create(
        &self,
        plan: JobPlan,
        work_dir: &Path,
        listener: Arc<dyn JobListener>,
    ) -> Result<Arc<JobHandle>> {
        plan.validate()?;
        let stages = plan.stages()?;
        let (done, _) = watch::channel(None);
        Ok(Arc::new(JobHandle {
            job_id: plan.id,
            stages: Mutex::new(Some(stages)),
            backend: Arc::clone(&self.backend),
            work_root: work_dir.to_path_buf(),
            listener,
            control: Mutex::new(Control {
                state: JobState::Queued,
                pause_requested: false,
                cancel_requested: false,
                started: false,
            }),
            wake: Condvar::new(),
            done,
        }))
    }

    /// Creates and starts a job. Must be called inside a Tokio runtime.
    pub fn start(
        &self,
        plan: JobPlan,
        work_dir: &Path,
        listener: Arc<dyn JobListener>,
    ) -> Result<Arc<JobHandle>> {
        let handle = self.create(plan, work_dir, listener)?;
        handle.start()?;
        Ok(handle)
    }
}

type Finished = Option<std::result::Result<JobOutcome, Error>>;

/// A running job. All methods are safe to call from any thread, repeatedly, in any state.
pub struct JobHandle {
    job_id: String,
    stages: Mutex<Option<Vec<JobStage>>>,
    backend: Arc<dyn EncodeBackend>,
    work_root: PathBuf,
    listener: Arc<dyn JobListener>,
    control: Mutex<Control>,
    wake: Condvar,
    done: watch::Sender<Finished>,
}

impl std::fmt::Debug for JobHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobHandle")
            .field("job_id", &self.job_id)
            .finish_non_exhaustive()
    }
}

impl JobHandle {
    pub fn job_id(&self) -> String {
        self.job_id.clone()
    }

    pub fn state(&self) -> JobState {
        lock(&self.control).state
    }

    /// Starts the job on a blocking worker. Errors if already started or outside a Tokio runtime.
    pub fn start(self: &Arc<Self>) -> Result<()> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| Error::Internal("job started outside a Tokio runtime".to_owned()))?;
        {
            let mut control = lock(&self.control);
            if control.started {
                return Err(Error::Conflict("job already started".to_owned()));
            }
            control.started = true;
        }
        let job = Arc::clone(self);
        runtime.spawn_blocking(move || job.run());
        Ok(())
    }

    /// Pauses at the next checkpoint. No effect on a finished job.
    pub fn pause(&self) {
        let notify = {
            let mut control = lock(&self.control);
            if control.state.is_terminal() || control.cancel_requested || control.pause_requested {
                false
            } else {
                control.pause_requested = true;
                let running = control.state == JobState::Running;
                if running {
                    control.state = JobState::Paused;
                }
                running
            }
        };
        if notify {
            self.emit_state(JobState::Paused, None);
        }
    }

    pub fn resume(&self) {
        let notify = {
            let mut control = lock(&self.control);
            let was_paused = control.pause_requested;
            control.pause_requested = false;
            let notify = was_paused && control.state == JobState::Paused;
            if notify {
                control.state = JobState::Running;
            }
            self.wake.notify_all();
            notify
        };
        if notify {
            self.emit_state(JobState::Running, None);
        }
    }

    /// Requests cancellation, waking a paused job. The job ends as `Cancelled`.
    pub fn cancel(&self) {
        let mut control = lock(&self.control);
        control.cancel_requested = true;
        self.wake.notify_all();
    }

    /// Waits for the job to end. May be called concurrently.
    pub async fn wait(&self) -> Result<JobOutcome> {
        let mut rx = self.done.subscribe();
        let result = rx
            .wait_for(Option::is_some)
            .await
            .map_err(|_| Error::Internal("job dropped".to_owned()))?;
        result
            .clone()
            .unwrap_or_else(|| Err(Error::Internal("job ended without a result".to_owned())))
    }

    fn emit_state(&self, state: JobState, detail: Option<String>) {
        self.listener.on_event(JobEvent::StateChanged { state, detail });
    }

    fn checkpoint(&self) -> Result<()> {
        let mut control = lock(&self.control);
        loop {
            if control.cancel_requested {
                return Err(Error::Cancelled);
            }
            if !control.pause_requested {
                return Ok(());
            }
            control = self.wake.wait(control).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn run(self: Arc<Self>) {
        let queued_pause = {
            let mut control = lock(&self.control);
            let paused = control.pause_requested;
            control.state = if paused {
                JobState::Paused
            } else {
                JobState::Running
            };
            paused
        };
        self.emit_state(JobState::Running, None);
        if queued_pause {
            self.emit_state(JobState::Paused, None);
        }
        let job_dir = self.work_root.join(&self.job_id);
        let result = std::fs::create_dir_all(&job_dir)
            .map_err(|e| Error::Storage(format!("{}: {e}", job_dir.display())))
            .and_then(|()| self.run_stages(&job_dir));
        let _ = std::fs::remove_dir_all(&job_dir);
        let (state, detail) = match &result {
            Ok(_) => (JobState::Completed, None),
            Err(Error::Cancelled) => (JobState::Cancelled, None),
            Err(e) => (JobState::Failed, Some(e.to_string())),
        };
        lock(&self.control).state = state;
        self.emit_state(state, detail);
        self.done.send_replace(Some(result));
    }

    fn run_stages(self: &Arc<Self>, job_dir: &Path) -> Result<JobOutcome> {
        let stages = lock(&self.stages)
            .take()
            .ok_or_else(|| Error::Internal("job ran twice".to_owned()))?;
        let total_weight: f64 = stages.iter().map(|s| s.weight).sum();
        let shared = Arc::new(ProgressState {
            job: Arc::clone(self),
            inner: Mutex::new(ProgressInner {
                stage_id: String::new(),
                base: 0.0,
                share: 0.0,
                overall: 0.0,
                last_stage_fraction: 0.0,
            }),
        });
        let mut artifacts: HashMap<String, Vec<String>> = HashMap::new();
        let mut outputs = Vec::new();
        let mut done_weight = 0.0;
        let count = stages.len() as u32;
        for (index, stage) in stages.into_iter().enumerate() {
            self.checkpoint()?;
            let stage_id = stage.id.clone();
            let weight = stage.weight;
            let is_sink = matches!(
                stage.nodes.last().map(|n| &n.op),
                Some(NodeOp::Mux { .. } | NodeOp::Thumbnail { .. })
            );
            let inputs = stage
                .input_stages
                .iter()
                .map(|id| {
                    let path = artifacts
                        .get(id)
                        .and_then(|a| a.first())
                        .cloned()
                        .ok_or_else(|| Error::Internal(format!("stage {id} produced no artifact")))?;
                    Ok(StageArtifact {
                        stage_id: id.clone(),
                        path,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            shared.begin_stage(&stage_id, done_weight / total_weight, weight / total_weight);
            self.listener.on_event(JobEvent::StageStarted {
                stage_id: stage_id.clone(),
                index: index as u32,
                count,
            });
            let task = StageTask {
                job_id: self.job_id.clone(),
                output_path: job_dir
                    .join(format!("{stage_id}.stream"))
                    .to_string_lossy()
                    .into_owned(),
                work_dir: job_dir.to_string_lossy().into_owned(),
                stage,
                inputs,
            };
            let reporter: Arc<dyn StageReporter> = shared.clone();
            let backend = Arc::clone(&self.backend);
            let output = catch_unwind(AssertUnwindSafe(move || backend.execute_stage(task, reporter)))
                .map_err(|_| Error::Internal(format!("backend panicked in stage {stage_id}")))??;
            shared.finish_stage();
            self.listener.on_event(JobEvent::StageFinished {
                stage_id: stage_id.clone(),
            });
            if output.artifacts.is_empty() {
                return Err(Error::Platform(format!("stage {stage_id} returned no artifacts")));
            }
            if is_sink {
                outputs.extend(output.artifacts.iter().cloned());
            }
            artifacts.insert(stage_id, output.artifacts);
            done_weight += weight;
        }
        Ok(JobOutcome {
            job_id: self.job_id.clone(),
            outputs,
        })
    }
}

struct ProgressInner {
    stage_id: String,
    base: f64,
    share: f64,
    overall: f64,
    last_stage_fraction: f64,
}

/// Bridges backend reports to listener events and keeps overall progress monotonic.
struct ProgressState {
    job: Arc<JobHandle>,
    inner: Mutex<ProgressInner>,
}

impl ProgressState {
    fn begin_stage(&self, stage_id: &str, base: f64, share: f64) {
        let mut inner = lock(&self.inner);
        inner.stage_id = stage_id.to_owned();
        inner.base = base;
        inner.share = share;
        inner.last_stage_fraction = 0.0;
    }

    fn finish_stage(&self) {
        self.report(1.0);
    }

    fn report(&self, fraction: f64) {
        if !fraction.is_finite() {
            return;
        }
        let fraction = fraction.clamp(0.0, 1.0);
        let event = {
            let mut inner = lock(&self.inner);
            let stage_fraction = fraction.max(inner.last_stage_fraction);
            let overall = (inner.base + stage_fraction * inner.share).clamp(inner.overall, 1.0);
            inner.last_stage_fraction = stage_fraction;
            inner.overall = overall;
            JobEvent::Progress {
                stage_id: inner.stage_id.clone(),
                stage_fraction,
                overall_fraction: overall,
            }
        };
        self.job.listener.on_event(event);
    }
}

impl StageReporter for ProgressState {
    fn progress(&self, fraction: f64) {
        self.report(fraction);
    }

    fn checkpoint(&self) -> Result<()> {
        self.job.checkpoint()
    }
}
