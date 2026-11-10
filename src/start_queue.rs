//! The order a restored layout's shells are started in.
//!
//! Started all at once, 42 shells reached their prompts together, five to six seconds
//! after launch, and the seven agents they then ran reported at seven (measured
//! 2026-11-10 on ten cores, one run of each variant; a login zsh takes 0.3 s alone).
//! The panes on screen waited as long as the ones nobody was looking at. Here they
//! start a `Rank` at a time: the next rank begins when every shell of the one before
//! has given up its `StartSlot`. Nothing but the order moves: every pane is still
//! started, at launch, without being viewed. Same layout, in order: the panes on
//! screen at their prompts at 1.2 s, the agents reporting at 3 s, the last shell at
//! 8 s (two seconds later than before, the price of the agents going first).
//!
//! A rank starts all at once, with no cap on how many. A cap was tried (five at a
//! time) and put the last shell at 10.7 s: about half a second of a shell's start is
//! waiting, not work, so fewer at once only starts fewer per second.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// Which panes start first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    /// A pane of the workspace on screen. Its slot is held until its first prompt.
    Focused,
    /// A pane with a local agent to bring back. Its slot is held until the agent's
    /// hook reports a session, so the agents start with the machine to themselves:
    /// with the other shells started at their prompt they took four seconds from
    /// there to report, not one.
    Agent,
    /// Held until its first prompt.
    Rest,
}

/// How long a shell may hold its slot without reaching a prompt. One with no shell
/// integration never reports a prompt, and must not hold the next rank back for good.
const SHELL_MAX: Duration = Duration::from_secs(3);

/// The same for a pane waiting on its agent's hook, which never comes from an agent
/// without a session hook (Codex), with hooks disabled, or that failed to start.
const AGENT_MAX: Duration = Duration::from_secs(5);

type Job = Box<dyn FnOnce(StartSlot) + Send>;

#[derive(Default)]
struct State {
    pending: Vec<(Rank, Job)>,
    /// Slots of the rank that is starting, not yet given up.
    starting: usize,
    /// A worker thread is draining `pending`.
    running: bool,
}

pub struct Queue {
    state: Mutex<State>,
    cvar: Condvar,
    shell_max: Duration,
    agent_max: Duration,
}

/// A started shell's hold on its rank. Released when the shell is up (see `Rank`),
/// when it exits, or when its time runs out; only the first release counts.
#[derive(Clone)]
pub struct StartSlot(Arc<SlotInner>);

struct SlotInner {
    released: AtomicBool,
    queue: Arc<Queue>,
}

impl StartSlot {
    pub fn release(&self) {
        if !self.0.released.swap(true, Ordering::AcqRel) {
            let q = &self.0.queue;
            let mut st = q.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.starting = st.starting.saturating_sub(1);
            q.cvar.notify_one();
        }
    }
}

impl Queue {
    fn new(shell_max: Duration, agent_max: Duration) -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(State::default()), cvar: Condvar::new(), shell_max, agent_max })
    }

    /// Queue a start. Nothing runs until `start`, so a whole layout can be queued
    /// before the first rank is chosen from.
    pub fn push(&self, rank: Rank, job: impl FnOnce(StartSlot) + Send + 'static) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).pending.push((rank, Box::new(job)));
    }

    /// Start what is queued, rank by rank. The worker ends once the queue is empty,
    /// so a later batch needs `start` again.
    pub fn start(self: &Arc<Self>) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.running || st.pending.is_empty() {
            return;
        }
        st.running = true;
        drop(st);
        let q = self.clone();
        std::thread::Builder::new()
            .name("arbiter-start-queue".into())
            .spawn(move || q.run())
            .expect("spawn start queue");
    }

    fn run(self: Arc<Self>) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            while st.starting > 0 {
                st = self.cvar.wait(st).unwrap_or_else(PoisonError::into_inner);
            }
            let Some(rank) = st.pending.iter().map(|(rank, _)| *rank).min() else {
                st.running = false;
                return;
            };
            let (now, later): (Vec<_>, Vec<_>) = std::mem::take(&mut st.pending).into_iter().partition(|(r, _)| *r == rank);
            st.pending = later;
            st.starting = now.len();
            drop(st);
            let max = if rank == Rank::Agent { self.agent_max } else { self.shell_max };
            for (_, job) in now {
                let slot = StartSlot(Arc::new(SlotInner { released: AtomicBool::new(false), queue: self.clone() }));
                let late = slot.clone();
                crate::session::schedule(Instant::now() + max, move || late.release());
                job(slot);
            }
            st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// The app's queue.
pub fn global() -> &'static Arc<Queue> {
    static QUEUE: OnceLock<Arc<Queue>> = OnceLock::new();
    QUEUE.get_or_init(|| Queue::new(SHELL_MAX, AGENT_MAX))
}

#[cfg(test)]
mod tests {
    use super::{Queue, Rank, StartSlot};
    use std::sync::mpsc::{channel, Receiver};
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(5);
    const NEVER: Duration = Duration::from_secs(3600);
    const QUIET: Duration = Duration::from_millis(200);

    // Queues one job per name; each reports its name and hands its slot back.
    fn queued(q: &Queue, jobs: &[(Rank, &'static str)]) -> Receiver<(&'static str, StartSlot)> {
        let (tx, rx) = channel();
        for &(rank, name) in jobs {
            let tx = tx.clone();
            q.push(rank, move |slot| {
                let _ = tx.send((name, slot));
            });
        }
        rx
    }

    // The next `n` starts, and that no more follow while their slots are held.
    fn rank_of(rx: &Receiver<(&'static str, StartSlot)>, n: usize) -> (Vec<&'static str>, Vec<StartSlot>) {
        let (names, slots) = (0..n).map(|_| rx.recv_timeout(WAIT).expect("a start")).unzip();
        assert!(rx.recv_timeout(QUIET).is_err(), "the next rank started before this one was up");
        (names, slots)
    }

    #[test]
    fn a_rank_starts_together_and_the_next_waits_for_all_of_it() {
        let q = Queue::new(NEVER, NEVER);
        let rx = queued(
            &q,
            &[(Rank::Rest, "rest 1"), (Rank::Agent, "agent 1"), (Rank::Focused, "focused 1"),
              (Rank::Rest, "rest 2"), (Rank::Focused, "focused 2"), (Rank::Agent, "agent 2")],
        );
        q.start();
        let (names, slots) = rank_of(&rx, 2);
        assert_eq!(names, ["focused 1", "focused 2"]);
        slots[0].release();
        slots[0].release();
        assert!(rx.recv_timeout(QUIET).is_err(), "one slot given up twice counted as two");
        slots[1].release();
        let (names, slots) = rank_of(&rx, 2);
        assert_eq!(names, ["agent 1", "agent 2"]);
        slots.iter().for_each(StartSlot::release);
        let (names, _) = rank_of(&rx, 2);
        assert_eq!(names, ["rest 1", "rest 2"]);
    }

    #[test]
    fn a_shell_that_never_reports_does_not_hold_the_next_rank_back() {
        let q = Queue::new(Duration::from_millis(100), NEVER);
        let rx = queued(&q, &[(Rank::Focused, "silent"), (Rank::Rest, "next")]);
        q.start();
        let (_, _silent) = rx.recv_timeout(WAIT).expect("first");
        let (name, _) = rx.recv_timeout(WAIT).expect("the next rank, once the first ran out of time");
        assert_eq!(name, "next");
    }

    #[test]
    fn an_agent_pane_has_its_own_longer_limit() {
        let q = Queue::new(NEVER, Duration::from_millis(100));
        let rx = queued(&q, &[(Rank::Agent, "no hook"), (Rank::Rest, "next")]);
        q.start();
        let (_, _held) = rx.recv_timeout(WAIT).expect("first");
        assert_eq!(rx.recv_timeout(WAIT).expect("the rest, once the agent ran out of time").0, "next");
    }

    #[test]
    fn a_batch_queued_after_the_first_drained_starts_too() {
        let q = Queue::new(NEVER, NEVER);
        let rx = queued(&q, &[(Rank::Rest, "a")]);
        q.start();
        rx.recv_timeout(WAIT).expect("first batch").1.release();
        let rx = queued(&q, &[(Rank::Rest, "b")]);
        q.start();
        assert_eq!(rx.recv_timeout(WAIT).expect("second batch").0, "b");
    }
}
