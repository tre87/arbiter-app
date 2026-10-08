//! What Arbiter knows about a pane's remote connection: whether an ssh/mosh client is in
//! the foreground, the command that built it, whether it asks for a credential, the far
//! shell's directory, and whether it deserves a reconnect. Nothing here is about the
//! agent that may run on the far side; that half lives on `AgentHandle`, and the few
//! transitions touching both are the free functions in `session.rs`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::persist::SAVE_DIRTY;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub struct ConnHandle {
    /// An SSH/mosh client is running in this pane, so its foreground program lives on
    /// another machine. Set from the same busy-edge scan as the agent's; gates the
    /// on-screen probe, which only remote panes need.
    remote: AtomicBool,
    /// Sticky version of `remote`: true once this pane has been remote, never cleared.
    was_remote: AtomicBool,
    /// The last command the user submitted in this pane (memory only), and the one
    /// accepted as its startup command (persisted). See `latch_startup_cmd`.
    last_command: Mutex<Option<String>>,
    startup_cmd: Mutex<Option<String>>,
    /// This pane's private command-history file, consulted when the typed line was
    /// abandoned (an up-arrow recall). See `histfile_last_remote_cmd`.
    histfile: Option<std::path::PathBuf>,
    /// Whether this pane's connection asks for a credential. `None` until observed,
    /// `Some(true)` once ssh has prompted, `Some(false)` once a login completed with no
    /// prompt. Persisted, so the startup dialog only asks for connections that will ask.
    prompts_for_credential: Mutex<Option<bool>>,
    /// Which secret it asked for, and for what (a key's file name, or `user@host`), read
    /// from ssh's prompt. Persisted, so the dialog can say what it wants.
    credential_prompt: Mutex<Option<(crate::persist::CredentialKind, String)>>,
    /// A credential prompt appeared on the CURRENT connection. Reset per connection, so
    /// the login evidence below can tell "asked" from "never asked" this time.
    prompted_this_connection: AtomicBool,
    /// The current connection has shown signs of a completed login: the far shell
    /// reported a directory, the agent's chrome appeared, or enough time passed. Ends the
    /// window in which a credential may be typed and settles `prompts_for_credential`.
    login_seen: AtomicBool,
    /// Arbiter typed a credential on the current connection (auto or from the re-ask
    /// dialog), so a further prompt or a denial means it was rejected. Reset per prompt
    /// handled and per connection.
    credential_typed: AtomicBool,
    /// When the current connection was detected, ms since epoch; 0 when not remote.
    remote_since_ms: AtomicU64,
    /// The one automatic reconnect a connection gets has been used. Reset when the next
    /// connection is detected, so every established session earns one more.
    retry_spent: AtomicBool,
    /// Requests from the reader to the UI, taken on the next redraw: rerun this pane's
    /// connection (its ssh exited abnormally), or ask again for its credential (the one
    /// typed was rejected).
    retry_wanted: AtomicBool,
    reask_wanted: AtomicBool,
    /// ssh said it never reached the host on the current attempt (refused, timed out,
    /// unresolvable), so however long the attempt took, nothing was up to drop.
    connect_failed: AtomicBool,
    /// The far host's working directory: from an OSC-7 the REMOTE shell emitted (the
    /// opt-in snippet) where there is one, else followed from the `cd` commands typed
    /// there. Kept when the connection ends so Reconnect and restore can go back to it.
    remote_cwd: Mutex<Option<String>>,
    /// The value before the last change, so a `cd` the far shell then rejected can be
    /// undone.
    remote_cwd_prev: Mutex<Option<String>>,
    /// A remote OSC-7 arrived on the current connection: the snippet is active there.
    remote_cwd_reported: AtomicBool,
    /// The user declined to bring this pane's connection back (Skip in the sign-in
    /// dialog). The command is kept so Reconnect still can, and the amber button stays,
    /// until a command is run here; a save meanwhile writes the pane as a plain local one.
    dismissed: AtomicBool,
}

impl ConnHandle {
    pub fn new(histfile: Option<std::path::PathBuf>) -> Self {
        Self {
            remote: AtomicBool::new(false),
            was_remote: AtomicBool::new(false),
            last_command: Mutex::new(None),
            startup_cmd: Mutex::new(None),
            histfile,
            prompts_for_credential: Mutex::new(None),
            credential_prompt: Mutex::new(None),
            prompted_this_connection: AtomicBool::new(false),
            login_seen: AtomicBool::new(false),
            credential_typed: AtomicBool::new(false),
            remote_since_ms: AtomicU64::new(0),
            retry_spent: AtomicBool::new(false),
            retry_wanted: AtomicBool::new(false),
            reask_wanted: AtomicBool::new(false),
            connect_failed: AtomicBool::new(false),
            remote_cwd: Mutex::new(None),
            remote_cwd_prev: Mutex::new(None),
            remote_cwd_reported: AtomicBool::new(false),
            dismissed: AtomicBool::new(false),
        }
    }

    /// Busy-edge scan: an SSH/mosh client is (or is no longer) running in this pane. The
    /// far agent's half of the edge is `AgentHandle::connection_edge`; both run through
    /// `session::connection_edge`.
    pub fn set_remote(&self, on: bool) {
        self.remote.store(on, Ordering::Relaxed);
        if on {
            self.was_remote.store(true, Ordering::Relaxed);
            self.remote_since_ms.store(now_ms(), Ordering::Relaxed);
            self.retry_spent.store(false, Ordering::Relaxed);
            self.login_seen.store(false, Ordering::Relaxed);
            self.prompted_this_connection.store(false, Ordering::Relaxed);
            self.credential_typed.store(false, Ordering::Relaxed);
            self.connect_failed.store(false, Ordering::Relaxed);
            self.latch_startup_cmd();
        } else {
            self.remote_since_ms.store(0, Ordering::Relaxed);
            self.remote_cwd_reported.store(false, Ordering::Relaxed);
        }
    }

    /// Reader: the user ended the remote session themselves (the far shell exited, so ssh
    /// returned that shell's status rather than its own 255). The pane is a plain local
    /// shell again: nothing to reconnect, nothing to replay or sign in to on the next
    /// launch, and a new `ssh` typed here latches afresh.
    pub fn forget(&self) {
        *self.startup_cmd.lock().unwrap() = None;
        *self.last_command.lock().unwrap() = None;
        *self.prompts_for_credential.lock().unwrap() = None;
        *self.credential_prompt.lock().unwrap() = None;
        *self.remote_cwd.lock().unwrap() = None;
        *self.remote_cwd_prev.lock().unwrap() = None;
        self.was_remote.store(false, Ordering::Relaxed);
        self.dismissed.store(false, Ordering::Relaxed);
        SAVE_DIRTY.store(true, Ordering::Relaxed);
    }

    /// UI: the user declined to bring this connection back for now (see `dismissed`).
    pub fn dismiss_connection(&self) {
        self.dismissed.store(true, Ordering::Relaxed);
        SAVE_DIRTY.store(true, Ordering::Relaxed);
    }

    /// UI: the connection is being brought back after all.
    pub fn undismiss(&self) {
        self.dismissed.store(false, Ordering::Relaxed);
    }

    pub fn dismissed(&self) -> bool {
        self.dismissed.load(Ordering::Relaxed)
    }

    /// Reader: ssh printed a credential prompt on this pane while its connection was
    /// being made, asking for `prompt` (see `session::describe_credential_prompt`).
    /// Ignored once the login is known to have completed, so a prompt from a nested
    /// `ssh` or `git push` on the far host does not describe this connection.
    pub fn note_credential_prompt(&self, prompt: Option<(crate::persist::CredentialKind, String)>) {
        if self.login_seen.load(Ordering::Relaxed) {
            return;
        }
        self.prompted_this_connection.store(true, Ordering::Relaxed);
        let mut prompts = self.prompts_for_credential.lock().unwrap();
        if *prompts != Some(true) {
            *prompts = Some(true);
            SAVE_DIRTY.store(true, Ordering::Relaxed);
        }
        if prompt.is_some() {
            let mut cur = self.credential_prompt.lock().unwrap();
            if *cur != prompt {
                *cur = prompt;
                SAVE_DIRTY.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Which secret the connection asked for, and for what, if it has been seen.
    pub fn credential_prompt(&self) -> Option<(crate::persist::CredentialKind, String)> {
        self.credential_prompt.lock().unwrap().clone()
    }

    /// Seed from a saved layout.
    pub fn seed_credential_prompt(&self, kind: Option<crate::persist::CredentialKind>, detail: Option<&str>) {
        *self.credential_prompt.lock().unwrap() = kind.map(|k| (k, detail.unwrap_or_default().to_string()));
    }

    /// Evidence that the current connection's login has completed. The first call per
    /// connection settles whether this connection prompts (it did not, if no prompt was
    /// seen) and returns true, so the caller can stop holding a credential.
    pub fn note_login_evidence(&self) -> bool {
        if !self.is_remote() || self.login_seen.swap(true, Ordering::Relaxed) {
            return false;
        }
        // Whatever was typed got the pane in, so nothing after this can reject it.
        self.credential_typed.store(false, Ordering::Relaxed);
        if !self.prompted_this_connection.load(Ordering::Relaxed) {
            let mut prompts = self.prompts_for_credential.lock().unwrap();
            if *prompts != Some(false) {
                *prompts = Some(false);
                SAVE_DIRTY.store(true, Ordering::Relaxed);
            }
        }
        true
    }

    /// Whether the current connection's login is known to have completed.
    pub fn login_seen(&self) -> bool {
        self.login_seen.load(Ordering::Relaxed)
    }

    /// Whether a credential prompt has appeared on the current connection.
    pub fn prompted_this_connection(&self) -> bool {
        self.prompted_this_connection.load(Ordering::Relaxed)
    }

    /// Reader: ssh reported that it could not reach the host on this attempt.
    pub fn note_connect_failure(&self) {
        self.connect_failed.store(true, Ordering::Relaxed);
    }

    /// What is known about whether this connection asks for a credential.
    pub fn prompts_for_credential(&self) -> Option<bool> {
        *self.prompts_for_credential.lock().unwrap()
    }

    /// Seed from a saved layout.
    pub fn set_prompts_for_credential(&self, prompts: Option<bool>) {
        *self.prompts_for_credential.lock().unwrap() = prompts;
    }

    /// A credential was just typed into this pane's connection (by Arbiter).
    pub fn note_credential_typed(&self) {
        self.credential_typed.store(true, Ordering::Relaxed);
    }

    /// Reader: ssh asked again, or denied access, after a credential was typed. True
    /// once per typed credential, so the UI is asked to re-ask exactly once for it.
    pub fn note_credential_rejected(&self) -> bool {
        if !self.credential_typed.swap(false, Ordering::Relaxed) {
            return false;
        }
        self.reask_wanted.store(true, Ordering::Relaxed);
        true
    }

    /// Reader: the connection ended abnormally and this pane should be reconnected.
    /// Honoured once per ESTABLISHED connection (see `retry_spent`): one whose login
    /// was seen, or that prompted, or that ran at least `min_connection` without ssh
    /// saying the host was unreachable. That last clause matters: a connect that times
    /// out runs for as long as ssh waits, and must not count as a session that dropped,
    /// or an unreachable host would be retried for as long as it stays down.
    pub fn request_retry(&self, min_connection: Duration) -> bool {
        let since = self.remote_since_ms.load(Ordering::Relaxed);
        let lasted = since != 0 && now_ms().saturating_sub(since) >= min_connection.as_millis() as u64;
        let established = self.login_seen.load(Ordering::Relaxed)
            || self.prompted_this_connection.load(Ordering::Relaxed)
            || (lasted && !self.connect_failed.load(Ordering::Relaxed));
        if !established || self.retry_spent.swap(true, Ordering::Relaxed) {
            return false;
        }
        self.retry_wanted.store(true, Ordering::Relaxed);
        true
    }

    /// When the current connection was detected (ms since epoch), 0 if not remote.
    /// Identifies a connection, so a timer armed for one cannot act on the next.
    pub fn remote_since_ms(&self) -> u64 {
        self.remote_since_ms.load(Ordering::Relaxed)
    }

    /// UI: take a pending reconnect request.
    pub fn take_retry_wanted(&self) -> bool {
        self.retry_wanted.swap(false, Ordering::Relaxed)
    }

    /// UI: take a pending re-ask request.
    pub fn take_reask_wanted(&self) -> bool {
        self.reask_wanted.swap(false, Ordering::Relaxed)
    }

    /// Reader: the REMOTE shell reported its working directory.
    pub fn note_remote_cwd(&self, path: String) {
        self.set_remote_cwd(Some(path));
        self.remote_cwd_reported.store(true, Ordering::Relaxed);
    }

    /// Where the far shell now is, as inferred (a `cd` typed there, a fresh login's home,
    /// a follow-up's `cd`). `None` when a `cd` made it unknowable.
    pub fn set_remote_cwd(&self, path: Option<String>) {
        let mut cur = self.remote_cwd.lock().unwrap();
        *self.remote_cwd_prev.lock().unwrap() = cur.clone();
        *cur = path;
    }

    /// The far shell rejected the `cd` that produced the current value: go back.
    pub fn revert_remote_cwd(&self) {
        if let Some(prev) = self.remote_cwd_prev.lock().unwrap().take() {
            *self.remote_cwd.lock().unwrap() = Some(prev);
        }
    }

    /// The far host's last reported working directory, if its shell ever reported one.
    pub fn remote_cwd(&self) -> Option<String> {
        self.remote_cwd.lock().unwrap().clone()
    }

    /// Seed from a saved layout.
    pub fn seed_remote_cwd(&self, path: Option<&str>) {
        *self.remote_cwd.lock().unwrap() = path.map(str::to_string);
    }

    /// Whether the far host has reported a directory on the current connection, which
    /// is how the user can tell the snippet took.
    pub fn remote_cwd_reported(&self) -> bool {
        self.remote_cwd_reported.load(Ordering::Relaxed)
    }

    /// Record a command the user submitted in this pane, if it invokes a remote client.
    ///
    /// Filtered HERE, at the point of capture, rather than only when the value is used.
    /// Keystrokes reach this from the input path, which includes what is typed at ssh's
    /// own prompts, so a key passphrase or a login password would otherwise sit in
    /// memory until the next command displaced it. Nothing but a recognised client
    /// invocation is worth keeping, so nothing else is kept.
    ///
    /// It also makes the capture more robust: a passphrase entered after `ssh mini` no
    /// longer overwrites it, so the connection command is still there to be latched.
    pub fn note_command(&self, cmd: String) {
        if crate::procscan::looks_like_remote_cmd(&cmd) {
            *self.last_command.lock().unwrap() = Some(cmd);
        }
    }

    /// Promote the last submitted command to this pane's startup command, if it invokes
    /// a remote client.
    ///
    /// Called the moment an ssh client is detected, NOT lazily when something reads the
    /// value. The distinction is the whole point: a save only happens on a layout
    /// change, so a lazy latch would run at quit time and capture whatever had been
    /// typed since: an agent launched at the remote prompt, or a key passphrase. The
    /// whitelist in `looks_like_remote_cmd` then makes the guarantee independent of
    /// timing altogether, which is what keeps a passphrase out of `session.json`.
    fn latch_startup_cmd(&self) {
        let mut startup = self.startup_cmd.lock().unwrap();
        if startup.is_some() {
            return;
        }
        // Prefer the line we watched being typed; otherwise ask the shell's own history.
        let typed = self.last_command.lock().unwrap().clone();
        *startup = typed
            .filter(|c| crate::procscan::looks_like_remote_cmd(c))
            .or_else(|| self.histfile_last_remote_cmd());
    }

    /// The most recent line in this pane's private history file, if it invokes a remote
    /// client.
    ///
    /// This is what makes the feature usable in practice. Keystroke tracking refuses to
    /// guess after an up-arrow recall or a tab completion, and reconnecting by pressing
    /// up is the common case, so without this a retried `ssh` would never be remembered.
    /// PowerShell's PSReadLine appends each command as it is accepted, so the line is
    /// there immediately. bash only appends at its *next* prompt, which for a
    /// long-running ssh has not happened yet, so there the typed line is what counts.
    fn histfile_last_remote_cmd(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.histfile.as_ref()?).ok()?;
        text.lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .filter(|l| crate::procscan::looks_like_remote_cmd(l))
            .map(str::to_string)
    }

    /// Seed the startup command from a saved layout on restore, where the command was
    /// replayed rather than typed. Returns whether it was accepted.
    ///
    /// Validated by the same whitelist as the live latch, which matters because a save
    /// written by an earlier build could hold anything that had been typed last. Without
    /// this check such a value would be replayed AND written straight back out on the
    /// next save, so it would outlive the fix instead of healing itself.
    pub fn set_startup_cmd(&self, cmd: &str) -> bool {
        if !crate::procscan::looks_like_remote_cmd(cmd) {
            return false;
        }
        *self.startup_cmd.lock().unwrap() = Some(cmd.to_string());
        // A pane with a remote startup command was remote, whether or not its ssh has
        // been seen running yet. This is what lets a connection that fails before the
        // process scan catches it still offer Reconnect.
        self.was_remote.store(true, Ordering::Relaxed);
        true
    }

    /// The command that rebuilds this pane, if one was accepted.
    pub fn startup_cmd(&self) -> Option<String> {
        self.startup_cmd.lock().unwrap().clone()
    }

    pub fn is_remote(&self) -> bool {
        self.remote.load(Ordering::Relaxed)
    }

    /// Whether this pane has EVER been remote. Never cleared, so a pane whose ssh
    /// session has ended can still be told apart from one that was always local. That
    /// is the difference between "your connection dropped, reconnect?" and a perfectly
    /// ordinary local shell sitting at its prompt.
    pub fn was_remote(&self) -> bool {
        self.was_remote.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::ConnHandle;
    use std::time::Duration;

    fn handle() -> ConnHandle {
        ConnHandle::new(None)
    }

    // The startup command is latched the moment ssh is detected, not when something
    // reads it. A save only happens on a layout change, so a lazy latch ran at quit time
    // and captured whatever had been typed since: `claude` at the remote prompt.
    #[test]
    fn latches_the_ssh_command_not_what_was_typed_afterwards() {
        let h = handle();
        h.note_command("ssh mini".into());
        h.set_remote(true); // busy-edge scan finds the ssh client
        h.note_command("claude".into()); // typed at the remote prompt
        assert_eq!(h.startup_cmd().as_deref(), Some("ssh mini"));
    }

    // Everything typed while connecting goes through the same tracking, passphrases
    // included, so only a recognised client invocation may ever be latched.
    #[test]
    fn refuses_to_latch_anything_that_is_not_a_remote_client() {
        for typed in ["correct horse battery staple", "claude", "cd ~/src"] {
            let h = handle();
            h.note_command(typed.into());
            h.set_remote(true);
            assert!(h.startup_cmd().is_none(), "{typed:?} must not be latched");
        }
    }

    // A key passphrase or login password is typed at ssh's own prompt and submitted
    // with Enter, so it arrives here exactly like a command would. It must not be
    // retained at all, and it must not displace the connection command that preceded
    // it, which is still needed for the latch.
    #[test]
    fn a_passphrase_entered_after_the_ssh_command_is_not_retained() {
        let h = handle();
        h.note_command("ssh tre@10.0.0.16".into());
        h.note_command("correct horse battery staple".into()); // key passphrase
        h.note_command("hunter2".into()); // login password
        h.set_remote(true);
        assert_eq!(h.startup_cmd().as_deref(), Some("ssh tre@10.0.0.16"));
    }

    // A save written before the whitelist existed can hold anything. Seeding it on
    // restore has to reject it, or it would be replayed and written straight back out,
    // outliving the fix.
    #[test]
    fn a_poisoned_saved_command_is_rejected_on_restore() {
        let h = handle();
        assert!(!h.set_startup_cmd("claude"), "must not be accepted");
        assert!(h.startup_cmd().is_none());
        // A legitimate one still seeds, and then wins over any later typing.
        assert!(h.set_startup_cmd("ssh mini"));
        assert_eq!(h.startup_cmd().as_deref(), Some("ssh mini"));
        h.note_command("ssh other-host".into());
        h.set_remote(true);
        assert_eq!(h.startup_cmd().as_deref(), Some("ssh mini"), "first command wins");
    }

    // A restored pane whose ssh fails before the process scan sees it still has to
    // offer Reconnect, so the saved command alone marks the pane as having been remote.
    #[test]
    fn a_seeded_startup_command_marks_the_pane_remote() {
        let h = handle();
        assert!(!h.was_remote());
        assert!(h.set_startup_cmd("ssh mini"));
        assert!(h.was_remote());
        assert!(!h.is_remote(), "seeding does not pretend the connection is up");
    }

    // One automatic retry per established connection, none for a refusal that never got
    // established, and a fresh one once the next connection is up.
    #[test]
    fn a_dropped_connection_earns_one_retry() {
        let h = handle();
        assert!(!h.request_retry(Duration::ZERO), "never connected: nothing to retry");
        h.set_remote(true);
        assert!(!h.request_retry(Duration::from_secs(3600)), "too short to count as established");
        assert!(h.request_retry(Duration::ZERO));
        assert!(h.take_retry_wanted());
        assert!(!h.take_retry_wanted(), "taken once");
        assert!(!h.request_retry(Duration::ZERO), "the one retry is spent");
        h.set_remote(true);
        assert!(h.request_retry(Duration::ZERO), "a new connection earns another");
    }

    // A connect that times out keeps ssh running for as long as it waits, which is not
    // a session that dropped. Without this rule an unreachable host was retried for as
    // long as it stayed down.
    #[test]
    fn an_attempt_that_never_reached_the_host_earns_no_retry() {
        let h = handle();
        h.set_remote(true);
        h.note_connect_failure();
        assert!(!h.request_retry(Duration::ZERO), "long, but never up");
        // Evidence of a real session outranks the duration rule either way.
        let h = handle();
        h.set_remote(true);
        h.note_login_evidence();
        assert!(h.request_retry(Duration::from_secs(3600)), "logged in: a drop, however quick");
        let h = handle();
        h.set_remote(true);
        h.note_credential_prompt(None);
        assert!(h.request_retry(Duration::from_secs(3600)), "reached the prompt: the host was up");
    }

    // Inferred directories can be undone when the far shell rejects the cd, but a
    // reported one (the snippet) is never second-guessed by the caller.
    #[test]
    fn a_rejected_cd_restores_the_previous_directory() {
        let h = handle();
        h.set_remote_cwd(Some("~".into()));
        h.set_remote_cwd(Some("~/Do".into()));
        h.revert_remote_cwd();
        assert_eq!(h.remote_cwd().as_deref(), Some("~"));
        h.revert_remote_cwd();
        assert_eq!(h.remote_cwd().as_deref(), Some("~"), "nothing further to undo");
    }

    // The directory outlives the connection (Reconnect and restore need it); the
    // "reported on this connection" fact does not.
    #[test]
    fn the_remote_directory_survives_the_connection_ending() {
        let h = handle();
        h.set_remote(true);
        h.note_remote_cwd("/home/tre/src".into());
        assert!(h.remote_cwd_reported());
        h.set_remote(false);
        assert_eq!(h.remote_cwd().as_deref(), Some("/home/tre/src"));
        assert!(!h.remote_cwd_reported());
    }

    // What is known about a connection's prompting: unknown until seen; a prompt during
    // login says yes; a login completing with no prompt says no; a prompt after the login
    // (a nested ssh on the far host) says nothing about this connection.
    #[test]
    fn learns_whether_a_connection_prompts() {
        let h = handle();
        assert_eq!(h.prompts_for_credential(), None);
        h.set_remote(true);
        h.note_credential_prompt(None);
        assert_eq!(h.prompts_for_credential(), Some(true));
        assert!(h.note_login_evidence(), "first evidence on this connection");
        assert!(!h.note_login_evidence(), "only the first counts");
        assert_eq!(h.prompts_for_credential(), Some(true), "it did prompt");

        let h = handle();
        h.set_remote(true);
        assert!(h.note_login_evidence());
        assert_eq!(h.prompts_for_credential(), Some(false));
        h.note_credential_prompt(None); // a nested ssh, after login
        assert_eq!(h.prompts_for_credential(), Some(false));

        let h = handle();
        assert!(!h.note_login_evidence(), "not remote: no connection to speak of");
        assert_eq!(h.prompts_for_credential(), None);
    }

    // A typed credential followed by another prompt (or a denial) is a rejection, and
    // is reported once per typed credential.
    #[test]
    fn a_repeated_prompt_after_typing_is_a_rejection() {
        let h = handle();
        assert!(!h.note_credential_rejected(), "nothing was typed");
        h.note_credential_typed();
        assert!(h.note_credential_rejected());
        assert!(h.take_reask_wanted());
        assert!(!h.note_credential_rejected(), "already reported for that credential");
        assert!(!h.take_reask_wanted());
    }
}
