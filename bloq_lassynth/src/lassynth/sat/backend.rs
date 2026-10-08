//! Native Kissat backend with cancellation and deadline interruption.

use rustsat::instances::Cnf;
use rustsat::solvers::{Interrupt, Solve, SolverResult};
use rustsat::types::{TernaryVal, Var};
use rustsat_kissat::{Config, Kissat};

use crate::monitor::SynthesisMonitor;
use crate::{Deadline, SynthesisError};

pub(crate) struct Backend {
    solver: Kissat<'static>,
    deadline: Deadline,
    monitor: SynthesisMonitor,
}

impl Backend {
    pub(crate) async fn new(
        cnf: Cnf,
        deadline: Deadline,
        monitor: SynthesisMonitor,
    ) -> Result<Self, SynthesisError> {
        deadline.check(&monitor)?;
        let mut solver = Kissat::default();
        // Fixed-box component searches usually seek a model. This profile is
        // 2-6x faster on the synthesis suite than Kissat's UNSAT profile.
        solver
            .set_configuration(Config::Sat)
            .map_err(solver_error)?;
        load_cnf(&mut solver, cnf, &deadline, &monitor).await?;
        Ok(Self {
            solver,
            deadline,
            monitor,
        })
    }

    /// Runs Kissat once, publishing its interrupt handle while active.
    pub(crate) fn solve(&mut self) -> Result<SolverResult, SynthesisError> {
        self.deadline.check(&self.monitor)?;
        self.monitor
            .attach_solver(Box::new(self.solver.interrupter()));
        let watchdog = self.deadline.remaining().map(|remaining| {
            let (stop, wait) = std::sync::mpsc::channel();
            let monitor = self.monitor.clone();
            let thread = std::thread::spawn(move || {
                if wait.recv_timeout(remaining).is_err() {
                    monitor.interrupt_solver();
                }
            });
            (stop, thread)
        });
        let result = self.solver.solve();
        if let Some((stop, watchdog)) = watchdog {
            let _ = stop.send(());
            let _ = watchdog.join();
        }
        self.monitor.detach_solver();
        let result = result.map_err(solver_error)?;
        self.deadline.check(&self.monitor)?;
        Ok(result)
    }

    pub(crate) fn value(&self, var: Var) -> Result<bool, SynthesisError> {
        self.solver
            .var_val(var)
            .map(|value| value == TernaryVal::True)
            .map_err(solver_error)
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // The published handle wraps this solver's raw pointer; a panic
        // unwinding out of `solve` must not leave it behind.
        self.monitor.detach_solver();
    }
}

async fn load_cnf(
    solver: &mut impl rustsat::solvers::Solve,
    cnf: rustsat::instances::Cnf,
    deadline: &crate::Deadline,
    monitor: &crate::SynthesisMonitor,
) -> Result<(), SynthesisError> {
    let mut work = crate::Construction::new(deadline, monitor);
    for clause in cnf {
        work.checkpoint().await?;
        solver.add_clause(clause).map_err(solver_error)?;
    }
    deadline.check(monitor)
}

fn solver_error(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> SynthesisError {
    SynthesisError::Solver(error.into())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use rustsat::instances::Cnf;
    use rustsat::types::{Clause, Var};

    use super::Backend;
    use crate::SynthesisError;
    use crate::executor::block_on;
    use crate::monitor::SynthesisMonitor;

    #[test]
    fn cancel_interrupts_a_running_search() {
        let (cnf, _) = pigeonhole(14, 13);
        let monitor = SynthesisMonitor::new();
        let (finished, wait) = mpsc::channel();

        let canceller = {
            let monitor = monitor.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(200));
                monitor.cancel();
            })
        };
        thread::spawn(move || {
            let result = block_on(async {
                Backend::new(cnf, crate::Deadline::none(), monitor)
                    .await?
                    .solve()
            });
            let _ = finished.send(matches!(result, Err(SynthesisError::Cancelled)));
        });

        let cancelled = wait
            .recv_timeout(Duration::from_secs(20))
            .expect("the cancelled search returns instead of running the instance out");
        assert!(cancelled, "an interrupted search reports cancellation");
        canceller.join().expect("the cancelling thread finishes");
    }

    #[test]
    fn deadline_interrupts_a_running_search() {
        let (cnf, _) = pigeonhole(14, 13);
        let (finished, wait) = mpsc::channel();
        thread::spawn(move || {
            let result = block_on(async {
                Backend::new(
                    cnf,
                    crate::Deadline::after(Duration::from_millis(20)),
                    SynthesisMonitor::new(),
                )
                .await?
                .solve()
            });
            let _ = finished.send(matches!(result, Err(SynthesisError::TimedOut)));
        });

        assert!(
            wait.recv_timeout(Duration::from_secs(20))
                .expect("the timed search returns")
        );
    }
    /// Pigeonhole: `pigeons` pigeons into `holes` holes, one variable per pair.
    /// Unsatisfiable whenever there are more pigeons than holes, and expensive
    /// enough for a CDCL solver that the interruption tests can rely on a search
    /// that is still running a moment after it started.
    fn pigeonhole(pigeons: u32, holes: u32) -> (Cnf, impl Fn(u32, u32) -> Var) {
        let var = move |pigeon: u32, hole: u32| Var::new(pigeon * holes + hole);
        let mut cnf = Cnf::new();
        for pigeon in 0..pigeons {
            cnf.add_clause((0..holes).map(|hole| var(pigeon, hole).pos_lit()).collect());
        }
        for hole in 0..holes {
            for left in 0..pigeons {
                for right in left + 1..pigeons {
                    cnf.add_clause(Clause::from_iter([
                        var(left, hole).neg_lit(),
                        var(right, hole).neg_lit(),
                    ]));
                }
            }
        }
        (cnf, var)
    }
}
