#![doc = include_str!("../README.md")]

#[cfg(target_family = "wasm")]
compile_error!("bloq_lassynth synthesis is native-only and does not support WebAssembly");

mod clifford_t;
mod executor;
mod lassynth;
mod monitor;

use std::time::{Duration, Instant};

pub use clifford_t::{
    CliffordTError, ComponentOptions, ParsedComponent, QasmError, parse_component, synthesize_qasm,
    synthesize_qasm_async,
};
pub use lassynth::{
    Port, SynthesisError, SynthesisProblem, synthesize, synthesize_with_timeout,
    synthesize_with_timeout_async,
};
pub use monitor::{SynthesisMonitor, SynthesisProgress};

#[derive(Clone)]
pub(crate) struct Deadline {
    start: Instant,
    timeout: Option<Duration>,
}

impl Deadline {
    pub(crate) fn none() -> Self {
        Self {
            start: Instant::now(),
            timeout: None,
        }
    }

    pub(crate) fn after(timeout: Duration) -> Self {
        Self {
            start: Instant::now(),
            timeout: Some(timeout),
        }
    }

    pub(crate) fn expired(&self) -> bool {
        self.timeout
            .is_some_and(|timeout| self.start.elapsed() >= timeout)
    }

    pub(crate) fn check(&self, monitor: &SynthesisMonitor) -> Result<(), SynthesisError> {
        if monitor.is_cancelled() {
            Err(SynthesisError::Cancelled)
        } else if self.expired() {
            Err(SynthesisError::TimedOut)
        } else {
            Ok(())
        }
    }

    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.timeout
            .map(|timeout| timeout.saturating_sub(self.start.elapsed()))
    }
}

/// Construction checks stop requests every 256 items.
pub(crate) struct Construction<'a> {
    deadline: &'a Deadline,
    monitor: &'a SynthesisMonitor,
    steps: usize,
}

impl<'a> Construction<'a> {
    fn new(deadline: &'a Deadline, monitor: &'a SynthesisMonitor) -> Self {
        Self {
            deadline,
            monitor,
            steps: 0,
        }
    }

    #[expect(clippy::unused_async, reason = "retains the async synthesis interface")]
    async fn checkpoint(&mut self) -> Result<(), SynthesisError> {
        if self.steps.is_multiple_of(256) {
            self.deadline.check(self.monitor)?;
        }
        self.steps += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::IVec3;

    #[test]
    fn stopped_runs_skip_preprocessing_and_sat() {
        for cancelled in [false, true] {
            let monitor = SynthesisMonitor::new();
            if cancelled {
                monitor.cancel();
            }
            let timeout = if cancelled {
                Duration::from_secs(30)
            } else {
                Duration::ZERO
            };
            let options = ComponentOptions::new(IVec3::ONE, [], [], timeout);
            let component =
                executor::block_on(synthesize_qasm_async("invalid QASM", &options, &monitor));
            let problem = SynthesisProblem::new(IVec3::splat(15), [], []);
            let direct =
                executor::block_on(synthesize_with_timeout_async(&problem, timeout, &monitor));
            let expected = |error: &SynthesisError| {
                matches!(
                    (cancelled, error),
                    (true, SynthesisError::Cancelled) | (false, SynthesisError::TimedOut)
                )
            };
            assert!(matches!(component, Err(CliffordTError::Synthesis(error)) if expected(&error)));
            assert!(matches!(direct, Err(error) if expected(&error)));
            assert_eq!(monitor.progress().solves, 0);
        }
    }
}
