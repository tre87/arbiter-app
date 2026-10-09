//! One `notify` watcher over the pane event logs the hook subcommand appends to (see
//! `agent_shim`), applying each new event to its pane's `AgentHandle`. Event-driven, the
//! same model as the git status: nothing runs while no agent is speaking.
//!
//! A log is append-only and read from where the last pass stopped, so two events landing
//! inside one debounce window (a Stop then a Notification, two Copilot tabs at once) are
//! both applied, in order. Only complete lines are taken: a line still being written is
//! left for the pass its final write triggers.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};

use crate::agent::AgentEvent;
use crate::agent_state::{live_handles, AgentHandle};

pub type Watcher = Debouncer<RecommendedWatcher>;

/// The per-session capture and hook dirs of 1.6 and earlier, cleared away at startup.
const LEGACY_DIRS: &[&str] = &["claude-sessions", "claude-hooks"];

/// Start the watcher. Returns the debouncer (keep it alive for the app's lifetime).
pub fn start_watcher() -> Option<Watcher> {
    let data_dir = crate::shell::app_data_dir()?;
    let dir = data_dir.join(crate::agent_shim::EVENTS_SUBDIR);
    let _ = std::fs::create_dir_all(&dir);
    // Drop leftovers from a previous run before anything binds. Logs are keyed by pane
    // id, and pane ids repeat across runs, so a stale log could falsely bind to a new
    // pane. They are transient (the durable resume id lives in session.json), so
    // clearing them loses nothing.
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    for legacy in LEGACY_DIRS {
        let _ = std::fs::remove_dir_all(data_dir.join(legacy));
    }

    let watched = dir.clone();
    let mut deb = new_debouncer(Duration::from_millis(80), move |res: DebounceEventResult| {
        if res.is_ok() {
            process(&watched);
        }
    })
    .ok()?;
    deb.watcher().watch(&dir, RecursiveMode::NonRecursive).ok()?;
    process(&dir);
    Some(deb)
}

fn process(dir: &Path) {
    for h in live_handles() {
        apply_new_lines(dir, &h);
    }
}

/// Apply the lines appended to `h`'s log since the last pass.
fn apply_new_lines(dir: &Path, h: &Arc<AgentHandle>) {
    let path = dir.join(format!("{}.jsonl", h.pane_id));
    let mut offset = h.events_offset.lock().unwrap();
    let Ok(mut file) = std::fs::File::open(&path) else { return };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    // Shorter than what was read: the log was deleted (`end_session`) and started over.
    if len < *offset {
        *offset = 0;
    }
    if len == *offset || file.seek(SeekFrom::Start(*offset)).is_err() {
        return;
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return;
    }
    let Some(end) = buf.iter().rposition(|&b| b == b'\n') else { return };
    let complete = &buf[..=end];
    *offset += complete.len() as u64;
    apply_lines(h, &String::from_utf8_lossy(complete));
}

pub(crate) fn apply_lines(h: &Arc<AgentHandle>, text: &str) {
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Some((kind, session, event)) = crate::agent_shim::decode_event(line) else {
            crate::debug::debug_log(&format!("agent_watch: pane {} skipped {line:?}", h.pane_id));
            continue;
        };
        crate::debug::debug_log(&format!(
            "agent_watch: pane {} {kind:?} {event:?} (running={})",
            h.pane_id,
            h.running.load(Ordering::Relaxed)
        ));
        h.apply(kind, session.as_deref(), &event);
        let bound = matches!(event, AgentEvent::SessionBound { .. })
            && h.kind() == Some(kind)
            && h.running.load(Ordering::Relaxed);
        if bound {
            if let Some(&shell) = h.shell_pid.get() {
                crate::priority::raise_agent_under(shell, kind);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentEvent, AgentKind};
    use crate::agent_shim::encode_event;
    use crate::agent_state::Lifecycle;
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;

    fn handle(pane: u64) -> Arc<AgentHandle> {
        AgentHandle::new(pane, Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(false)))
    }

    fn line(ev: AgentEvent) -> String {
        encode_event(AgentKind::Claude, Some("s1"), &ev, 1) + "\n"
    }

    fn tmpdir() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let d = std::env::temp_dir().join(format!("arbiter-watchtest-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // Two events in one pass are both applied, in order; a partial line waits for the
    // write that completes it; a log that starts over is read from its start.
    #[test]
    fn the_log_is_applied_in_order_a_line_at_a_time() {
        let dir = tmpdir();
        let h = handle(42);
        let path = dir.join("42.jsonl");
        let bound = line(AgentEvent::SessionBound { id: "s1".into(), transcript: None });
        let attention = line(AgentEvent::NeedsAttention);
        std::fs::write(&path, format!("{bound}{attention}")).unwrap();
        apply_new_lines(&dir, &h);
        assert_eq!(h.kind(), Some(AgentKind::Claude));
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Attention);

        // Half a line: nothing yet.
        let ended = line(AgentEvent::SessionEnded);
        let (a, b) = ended.split_at(10);
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(a.as_bytes()).unwrap();
        apply_new_lines(&dir, &h);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Attention);
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b.as_bytes()).unwrap();
        apply_new_lines(&dir, &h);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Ready, "SessionEnded clears the hook's attention");

        // Started over (shorter than what was read): read from the top.
        std::fs::write(&path, attention.clone()).unwrap();
        apply_new_lines(&dir, &h);
        assert_eq!(h.snapshot().lifecycle, Lifecycle::Attention);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A pane only ever reads its own log.
    #[test]
    fn a_log_is_applied_to_its_own_pane_only() {
        let dir = tmpdir();
        let (mine, other) = (handle(7), handle(8));
        std::fs::write(dir.join("7.jsonl"), line(AgentEvent::SessionBound { id: "s1".into(), transcript: None })).unwrap();
        apply_new_lines(&dir, &mine);
        apply_new_lines(&dir, &other);
        assert_eq!(mine.kind(), Some(AgentKind::Claude));
        assert_eq!(other.kind(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    use std::io::Write;
}
