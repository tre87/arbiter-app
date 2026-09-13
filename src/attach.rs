//! Attaching a local file to a REMOTE pane: the file is copied to the far host first, and
//! what gets pasted into the terminal is the copy's path there, so Claude on that host
//! reads it as it would a file dropped on a local pane.
//!
//! The copy travels over a second connection, made by the client's own `sftp` from the
//! same options as the pane's `ssh` line (`remote::sftp_invocation`). sftp rather than
//! scp because its batch commands (`pwd`, `mkdir`, `put`, `rm`) are protocol operations
//! the server carries out itself, so the far host's shell never enters into it: the same
//! batch works against a Mac, a Linux box and a Windows OpenSSH server, whose default
//! shell is cmd.exe and would neither expand `~` nor take `mkdir -p`. Batch paths are
//! relative to the login's home directory, which is where every sftp session starts, and
//! the `pwd` in the batch is how that directory's absolute spelling becomes known.
//!
//! Authentication: sftp runs inside a PTY, as the pane's ssh does, so a password or key
//! passphrase prompt shows up in its output and is answered from the vault the same way.
//! `-b` makes sftp hand ssh `BatchMode=yes`, which would refuse to prompt at all; an
//! earlier `-o BatchMode=no` wins because ssh keeps the first value it is given for an
//! option (the idiom sshpass users rely on). Windows OpenSSH has no connection sharing,
//! so this is a full handshake each time: a few hundred milliseconds on a LAN with a key
//! in the agent.
//!
//! Cleanup: nothing on the far host removes the copies, so Arbiter does. Names carry a
//! UTC date stamp, and once a day per connection, after a copy has succeeded and its
//! paths have been delivered, a background sweep removes by glob every stamp older than
//! yesterday. A copy therefore lives at least a full day, and nothing has to be tracked.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::persist::CredentialKind;
use crate::remote::SftpInvocation;
use crate::session::{describe_credential_prompt, is_credential_prompt, permission_denied, Secret};

/// Where copies land on the far host, relative to the login's home directory.
pub const REMOTE_DIR: &str = ".arbiter/attach";

/// How long one sftp run may take before it is killed: a big file on a slow link, or a
/// prompt nobody will answer (a 2FA challenge, a confirmation).
const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// Names the batch files apart when two attaches overlap.
static BATCH_SEQ: AtomicU32 = AtomicU32::new(0);

/// The copies made for one attach, as the far host's absolute paths, ready to paste.
#[derive(Debug, Clone)]
pub struct Attached {
    pub paths: Vec<String>,
    /// The daily sweep is running behind this copy (see `sweep`).
    pub swept: bool,
}

/// Why a copy did not happen. The credential variants carry what ssh asked for, so the
/// dialog can name it.
#[derive(Debug, Clone)]
pub enum Failure {
    /// ssh asked for a secret and none was available.
    NeedsCredential(Option<(CredentialKind, String)>),
    /// ssh asked again, or denied access, after the vault's secret was typed.
    Rejected(Option<(CredentialKind, String)>),
    /// ssh wanted the host key confirmed, which only an interactive session can do.
    UnknownHostKey,
    /// Anything else, with sftp's or ssh's own last word on it.
    Failed(String),
}

/// One file to copy: where it is here, and what to call it there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub local: String,
    pub name: String,
}

/// Copy `paths` to the far host and return their paths there. `swept` is left false; the
/// caller decides whether a sweep follows.
pub fn upload(inv: &SftpInvocation, paths: &[String], secret: Option<&Secret>) -> Result<Attached, Failure> {
    let uploads = uploads(paths, SystemTime::now());
    let output = run(inv, &batch(&uploads), secret)?;
    let home = remote_home(&output)
        .ok_or_else(|| Failure::Failed("the far host did not report its home directory".into()))?;
    let paths = uploads.iter().map(|u| format!("{home}/{REMOTE_DIR}/{}", u.name)).collect();
    Ok(Attached { paths, swept: false })
}

/// Remove every copy older than yesterday (UTC). Failure is of no consequence and is
/// not reported: the next day's sweep gets another chance.
pub fn sweep(inv: &SftpInvocation, secret: Option<&Secret>) {
    if let Err(e) = run(inv, &sweep_batch(SystemTime::now()), secret) {
        crate::claude_shim::debug_log(&format!("attach sweep on {}: {e:?}", inv.host()));
    }
}

/// The uploads for `paths`, named apart when two local files share a name.
fn uploads(paths: &[String], now: SystemTime) -> Vec<Upload> {
    let mut out: Vec<Upload> = Vec::with_capacity(paths.len());
    for local in paths {
        let base = remote_name(Path::new(local), now);
        let mut name = base.clone();
        let mut n = 1;
        while out.iter().any(|u| u.name == name) {
            n += 1;
            name = with_suffix(&base, &format!("-{n}"));
        }
        out.push(Upload { local: local.clone(), name });
    }
    out
}

/// The name a copy of `local` gets on the far host: a UTC stamp, then the local name
/// reduced to letters, digits, `.`, `-` and `_`, so it needs quoting nowhere, the far
/// host's shell included. The stamp is what the sweep matches on.
pub fn remote_name(local: &Path, now: SystemTime) -> String {
    let base = local
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let mut name = String::with_capacity(base.len());
    let mut last_was_gap = false;
    for c in base.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
            name.push(c);
            last_was_gap = false;
        } else if !last_was_gap {
            name.push('_');
            last_was_gap = true;
        }
    }
    if name.starts_with('.') || name.is_empty() {
        name.insert(0, '_');
    }
    format!("{}-{}", stamp(now), truncate_name(&name, 100))
}

/// `name` cut to `max` characters, keeping a short extension. ASCII only by the time
/// this runs, so characters are bytes.
fn truncate_name(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if name.len() - i <= 10 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    format!("{}{ext}", &stem[..max.saturating_sub(ext.len()).min(stem.len())])
}

/// `name` with `suffix` before its extension.
fn with_suffix(name: &str, suffix: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{}{suffix}{}", &name[..i], &name[i..]),
        _ => format!("{name}{suffix}"),
    }
}

/// `YYYYMMDD-HHMMSS` in UTC.
fn stamp(now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// The number of the UTC day `now` falls in, for once-a-day bookkeeping.
pub fn day_number(now: SystemTime) -> i64 {
    now.duration_since(UNIX_EPOCH).map(|d| (d.as_secs() / 86_400) as i64).unwrap_or(0)
}

/// Days since 1970-01-01 to (year, month, day). Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// The batch for `uploads`: learn the home directory (its absolute spelling is what gets
/// pasted), make sure the directory is there, copy each file. The `-` prefix lets a
/// `mkdir` fail on a directory that already exists; `put` has none, so a copy that fails
/// ends the batch with a non-zero exit.
pub fn batch(uploads: &[Upload]) -> String {
    let parent = REMOTE_DIR.split('/').next().unwrap_or(REMOTE_DIR);
    let mut out = format!("pwd\n-mkdir {parent}\n-mkdir {REMOTE_DIR}\n");
    for u in uploads {
        out.push_str(&format!(
            "put {} {}\n",
            quote(&local_for_sftp(&u.local)),
            quote(&format!("{REMOTE_DIR}/{}", u.name))
        ));
    }
    out
}

/// The batch that removes every copy stamped before yesterday (UTC): one `rm` glob per
/// day for the month before that, one per earlier month of that month's year, one per
/// year for three years before. Every line is prefixed `-`, so a glob that matches
/// nothing does not end the batch. Yesterday is kept whole: a UTC day boundary can be
/// minutes away from a copy that was just made and not yet read. Days of the month
/// before the day window that are not yet covered by their month's glob wait for the
/// month after, so a stray copy lives at most about two months.
pub fn sweep_batch(now: SystemTime) -> String {
    let today = day_number(now);
    let mut out = String::new();
    for back in 2..=32 {
        let (y, m, d) = civil_from_days(today - back);
        out.push_str(&format!("-rm {REMOTE_DIR}/{y:04}{m:02}{d:02}-*\n"));
    }
    let (y, m, _) = civil_from_days(today - 32);
    for month in (1..m).rev() {
        out.push_str(&format!("-rm {REMOTE_DIR}/{y:04}{month:02}*\n"));
    }
    for back in 1..=3 {
        out.push_str(&format!("-rm {REMOTE_DIR}/{:04}*\n", y - back));
    }
    out
}

/// A local path as sftp's parser wants it. Windows separators become `/`, which every
/// sftp build on Windows (native and MSYS) accepts for a local file, and which keeps `\`
/// free to be the escape character it is inside sftp's quotes.
fn local_for_sftp(path: &str) -> String {
    if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_string()
    }
}

/// One sftp argument in double quotes, `\` and `"` escaped (sftp's own quoting rules).
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// What the `pwd` in the batch reported: the login's home directory as the far host
/// spells it. Windows OpenSSH reports `/C:/Users/name`, which nothing on that host would
/// open, so a `/` in front of a drive letter goes.
pub fn remote_home(output: &str) -> Option<String> {
    let home = output
        .lines()
        .find_map(|l| l.trim().strip_prefix("Remote working directory: "))?
        .trim();
    let b = home.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        return Some(home[1..].to_string());
    }
    Some(home.to_string())
}

/// Run sftp on `batch` in a PTY, answering one credential prompt from `secret`, and
/// return everything it printed, terminal escape sequences removed. The batch goes
/// through a file rather than stdin because stdin is the PTY, which is where a password
/// prompt reads its answer.
fn run(inv: &SftpInvocation, batch: &str, secret: Option<&Secret>) -> Result<String, Failure> {
    let path = std::env::temp_dir().join(format!(
        "arbiter-attach-{}-{}.sftp",
        std::process::id(),
        BATCH_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, batch).map_err(|e| Failure::Failed(format!("could not write the batch file: {e}")))?;
    let result = run_batch_file(inv, &path, secret);
    let _ = std::fs::remove_file(&path);
    if let Err(e) = &result {
        crate::claude_shim::debug_log(&format!("attach: sftp to {} failed: {e:?}", inv.host()));
    }
    result
}

/// What the two helper threads of a run report: sftp's output as it comes, and its exit.
enum Event {
    Output(Vec<u8>),
    Exited(std::io::Result<portable_pty::ExitStatus>),
}

/// How long after a kill the process gets to actually go away before the run is given
/// up on with whatever has been seen.
const KILL_GRACE: Duration = Duration::from_secs(10);

/// How long the output has to stay quiet after the exit before the console is closed.
const OUTPUT_SETTLE: Duration = Duration::from_millis(250);

fn run_batch_file(inv: &SftpInvocation, batch_path: &Path, secret: Option<&Secret>) -> Result<String, Failure> {
    let failed = |e: &dyn std::fmt::Display| Failure::Failed(e.to_string());
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize { rows: 50, cols: 400, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| failed(&e))?;
    let mut cmd = CommandBuilder::new(&inv.program);
    // Before `-b`, which appends `BatchMode=yes`: ssh keeps the first value it sees.
    cmd.args(["-o", "BatchMode=no", "-o", "NumberOfPasswordPrompts=1", "-o", "ConnectTimeout=20"]);
    cmd.args(&inv.options);
    cmd.arg("-b");
    cmd.arg(batch_path);
    cmd.arg(&inv.destination);
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| Failure::Failed(format!("could not start {}: {e}", inv.program)))?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(|e| failed(&e))?;
    let mut writer = pair.master.take_writer().map_err(|e| failed(&e))?;
    let mut killer = child.clone_killer();

    // Output and exit arrive on one channel from two threads. The exit is watched on its
    // own because a ConPTY reader sees no end-of-file when the child exits: its pipe
    // closes only with the pseudoconsole, which is what dropping the master does below.
    // The reader then drains what is left and ends, closing the channel.
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(Event::Output(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }
    std::thread::spawn(move || {
        let _ = tx.send(Event::Exited(child.wait()));
    });

    let mut master = Some(pair.master);
    let mut deadline = Instant::now() + RUN_TIMEOUT;
    let mut killed = false;
    let mut raw = Vec::new();
    let mut scanned = 0;
    let mut typed = false;
    let mut failure: Option<Failure> = None;
    let mut status = None;
    loop {
        // After the exit, let the output go quiet before closing the console: ConPTY
        // renders on its own clock, and closing it discards what it has not yet written
        // to the pipe, which for a quick failure is the error message itself.
        let closing = status.is_some() && master.is_some();
        let wait = if closing { OUTPUT_SETTLE } else { deadline.saturating_duration_since(Instant::now()) };
        let event = match rx.recv_timeout(wait) {
            Ok(event) => event,
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) if closing => {
                master = None;
                killed = true;
                deadline = Instant::now() + KILL_GRACE;
                continue;
            }
            Err(RecvTimeoutError::Timeout) if killed => break,
            Err(RecvTimeoutError::Timeout) => {
                failure.get_or_insert_with(|| Failure::Failed("gave up after two minutes".into()));
                let _ = killer.kill();
                killed = true;
                deadline = Instant::now() + KILL_GRACE;
                continue;
            }
        };
        let bytes = match event {
            Event::Exited(s) => {
                status = Some(s);
                continue;
            }
            Event::Output(bytes) => bytes,
        };
        raw.extend_from_slice(&bytes);
        if failure.is_some() {
            continue;
        }
        let text = strip_escapes(&String::from_utf8_lossy(&raw));
        while !text.is_char_boundary(scanned) {
            scanned -= 1;
        }
        let fresh = &text[scanned..];
        if is_credential_prompt(fresh) {
            let prompt = describe_credential_prompt(fresh);
            match secret {
                Some(s) if !typed => {
                    let _ = writer.write_all(&s.line());
                    let _ = writer.flush();
                    typed = true;
                }
                Some(_) => failure = Some(Failure::Rejected(prompt)),
                None => failure = Some(Failure::NeedsCredential(prompt)),
            }
            scanned = text.len();
        } else if fresh.contains("(yes/no") {
            failure = Some(Failure::UnknownHostKey);
        } else if permission_denied(fresh) {
            failure = Some(if typed {
                Failure::Rejected(None)
            } else {
                Failure::Failed("Permission denied".into())
            });
        }
        if failure.is_some() && !killed {
            let _ = killer.kill();
            killed = true;
            deadline = Instant::now() + KILL_GRACE;
        }
    }
    drop(master);
    let text = strip_escapes(&String::from_utf8_lossy(&raw));
    crate::claude_shim::debug_log(&format!(
        "attach: sftp exit {:?}, {} bytes of output: {:?} (raw {:?})",
        status.as_ref().map(|s| s.as_ref().map(|s| s.exit_code()).map_err(|e| e.to_string())),
        raw.len(),
        text.chars().take(2000).collect::<String>(),
        String::from_utf8_lossy(&raw).chars().take(600).collect::<String>()
    ));
    if let Some(f) = failure {
        return Err(f);
    }
    match status {
        Some(Ok(s)) if s.success() => Ok(text),
        Some(Ok(_)) => Err(Failure::Failed(last_word(&text))),
        Some(Err(e)) => Err(failed(&e)),
        None => Err(Failure::Failed("sftp did not exit".into())),
    }
}

/// The last line of `text` that is sftp's or ssh's own, not a batch echo, cut short.
fn last_word(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("sftp>") && !l.starts_with("Remote working directory:"))
        .last()
        .unwrap_or("sftp failed without saying why");
    line.chars().take(200).collect()
}

/// `text` without terminal escape sequences (CSI, OSC, two-byte ESC forms) or bells,
/// which a PTY, ConPTY in particular, threads through a program's output.
fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    let mut prev = '\0';
                    for c in chars.by_ref() {
                        if c == '\x07' || (prev == '\x1b' && c == '\\') {
                            break;
                        }
                        prev = c;
                    }
                }
                _ => {}
            },
            '\x07' => {}
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    // 2026-09-13 10:15:30 UTC.
    const NOW: u64 = 1_789_294_530;

    #[test]
    fn civil_dates_are_right() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(20_709), (2026, 9, 13));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(day_number(at(NOW)), 20_709);
        assert_eq!(stamp(at(NOW)), "20260913-101530");
    }

    // The stamp leads, the local name follows reduced to shell-safe characters.
    #[test]
    fn remote_names_are_stamped_and_plain() {
        let name = |p: &str| remote_name(Path::new(p), at(NOW));
        assert_eq!(
            name("C:\\Users\\TRE\\Pictures\\Screenshot 2026-09-13 101010.png"),
            "20260913-101530-Screenshot_2026-09-13_101010.png"
        );
        assert_eq!(name("/Users/tre/Desktop/Sk\u{e6}rmbillede (1).png"), "20260913-101530-Sk_rmbillede_1_.png");
        assert_eq!(name("/tmp/.env"), "20260913-101530-_.env");
        assert_eq!(name("/tmp/a  b\tc"), "20260913-101530-a_b_c");
        let long = format!("/tmp/{}.jpeg", "x".repeat(150));
        let got = name(&long);
        assert_eq!(got.len(), "20260913-101530-".len() + 100);
        assert!(got.ends_with(".jpeg"));
        let three = uploads(&["/a/shot.png".into(), "/b/shot.png".into(), "/c/shot.png".into()], at(NOW));
        assert_eq!(three[0].name, "20260913-101530-shot.png");
        assert_eq!(three[1].name, "20260913-101530-shot-2.png");
        assert_eq!(three[2].name, "20260913-101530-shot-3.png");
    }

    #[test]
    fn the_batch_learns_home_makes_the_directory_and_copies() {
        let ups = vec![
            Upload { local: "C:\\Users\\TRE\\my shot.png".into(), name: "20260913-101530-my_shot.png".into() },
            Upload { local: "/Users/tre/it's \"quoted\".png".into(), name: "20260913-101530-it_s_quoted_.png".into() },
        ];
        let got = batch(&ups);
        let local0 = if cfg!(windows) { "C:/Users/TRE/my shot.png" } else { "C:\\\\Users\\\\TRE\\\\my shot.png" };
        assert_eq!(
            got,
            format!(
                "pwd\n-mkdir .arbiter\n-mkdir .arbiter/attach\n\
                 put \"{local0}\" \".arbiter/attach/20260913-101530-my_shot.png\"\n\
                 put \"/Users/tre/it's \\\"quoted\\\".png\" \".arbiter/attach/20260913-101530-it_s_quoted_.png\"\n"
            )
        );
    }

    // Everything older than yesterday goes; today and yesterday stay whole.
    #[test]
    fn the_sweep_spares_yesterday_and_reaches_back_years() {
        let got = sweep_batch(at(NOW));
        let lines: Vec<&str> = got.lines().collect();
        assert!(lines.iter().all(|l| l.starts_with("-rm .arbiter/attach/")), "every line tolerates a miss");
        assert!(!got.contains("20260913"), "today");
        assert!(!got.contains("20260912"), "yesterday");
        assert!(got.contains("-rm .arbiter/attach/20260911-*\n"), "the day before yesterday");
        assert!(got.contains("-rm .arbiter/attach/20260812-*\n"), "32 days back");
        assert!(!got.contains("202608*"), "August is still inside the day window");
        assert!(got.contains("-rm .arbiter/attach/202607*\n"));
        assert!(got.contains("-rm .arbiter/attach/202601*\n"));
        assert!(!got.contains("-rm .arbiter/attach/2026*\n"), "this year is never swept whole");
        assert!(got.contains("-rm .arbiter/attach/2025*\n"));
        assert!(got.contains("-rm .arbiter/attach/2023*\n"));
        assert_eq!(lines.len(), 31 + 7 + 3);
        // Early in a year the month globs belong to the previous year.
        let jan = sweep_batch(at(NOW + 110 * 86_400)); // 2027-01-01
        assert!(jan.contains("-rm .arbiter/attach/20261130-*\n"));
        assert!(jan.contains("-rm .arbiter/attach/202610*\n"));
        assert!(!jan.contains("-rm .arbiter/attach/2026*\n"));
        assert!(jan.contains("-rm .arbiter/attach/2025*\n"));
    }

    #[test]
    fn the_home_directory_is_read_from_the_echoed_batch() {
        let out = "sftp> pwd\r\nRemote working directory: /Users/tre\r\nsftp> -mkdir .arbiter\r\n";
        assert_eq!(remote_home(out).as_deref(), Some("/Users/tre"));
        assert_eq!(remote_home("Remote working directory: /C:/Users/tre\n").as_deref(), Some("C:/Users/tre"));
        assert_eq!(remote_home("Remote working directory: /home/tre\n").as_deref(), Some("/home/tre"));
        assert_eq!(remote_home("Connection closed\n"), None);
        assert_eq!(
            last_word("sftp> put a b\r\nstat a: No such file or directory\r\n"),
            "stat a: No such file or directory"
        );
        assert_eq!(last_word("sftp> pwd\nRemote working directory: /x\n"), "sftp failed without saying why");
    }

    #[test]
    fn escapes_are_stripped_around_prompts() {
        let raw = "\x1b[?25l\x1b[2J\x1b[Htre@mini's \x1b[0mpassword: \x1b]0;title\x07\x1b[?25h";
        assert_eq!(strip_escapes(raw), "tre@mini's password: ");
        assert!(is_credential_prompt(&strip_escapes(raw)));
        assert_eq!(strip_escapes("plain\r\nlines\x1b]0;t\x1b\\!"), "plain\r\nlines!");
    }

    // Run by hand: `cargo test --lib attach::tests::live -- --ignored --nocapture`. The
    // run's plumbing under ConPTY (spawn, capture, exit status, closing the console) with
    // cmd.exe standing in for sftp: the runner's own arguments land behind a final `echo`
    // and are printed harmlessly. The far host itself needs a real sshd, so it is not here.
    #[test]
    #[ignore = "spawns a ConPTY; run by hand"]
    #[cfg(windows)]
    fn live_run_captures_output_and_exit_under_conpty() {
        let probe = |cmdline: &str| SftpInvocation {
            program: "cmd.exe".into(),
            options: vec!["/c".into(), format!("{cmdline} & echo")],
            destination: "dummy".into(),
        };
        let got = upload(&probe("echo Remote working directory: /c/Users/tre"), &["C:\\x\\shot.png".into()], None)
            .unwrap();
        assert_eq!(got.paths.len(), 1);
        assert!(got.paths[0].starts_with("/c/Users/tre/.arbiter/attach/"), "{}", got.paths[0]);
        assert!(got.paths[0].ends_with("-shot.png"), "{}", got.paths[0]);
        match upload(&probe("echo boom 1>&2 & exit 7"), &["C:\\x\\shot.png".into()], None) {
            Err(Failure::Failed(why)) => assert_eq!(why, "boom"),
            other => panic!("{other:?}"),
        }
        match upload(&probe("echo tre@mini's password: & exit 0"), &["C:\\x\\shot.png".into()], None) {
            Err(Failure::NeedsCredential(Some((CredentialKind::Password, who)))) => assert_eq!(who, "tre@mini"),
            other => panic!("{other:?}"),
        }
    }
}
