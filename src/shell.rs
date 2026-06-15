//! Shell-integration: build the login shell command with OSC-7 (cwd) + OSC-133
//! (FinalTerm prompt markers: A/D=idle, B/C=busy) emitters injected, so the
//! terminal can detect cwd changes + shell busy/idle without polling. Ported
//! from `src-tauri/src/shell.rs` (the OSC-133 injection; the claude-shim
//! PATH/statusLine setup comes with the statusLine work later). Tauri-free:
//! the zsh integration dir lives under the per-OS data dir via `dirs`.

use std::sync::OnceLock;

use portable_pty::CommandBuilder;

use crate::claude_shim;

/// Arbiter's per-OS data dir — session.json + the claude-shim, capture, and hook
/// files live under here (the watchers read the same dirs). `ARBITER_DATA_DIR`
/// overrides it, so a test/dev instance can run fully ISOLATED and never read or
/// clobber the real session.json.
pub fn app_data_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("ARBITER_DATA_DIR") {
        return Some(std::path::PathBuf::from(dir));
    }
    // Debug builds (`cargo run`) use a SEPARATE dir from the installed release
    // app, so dev work doesn't share/clobber its workspaces, layout, settings, or
    // Claude shim state. Release (incl. `cargo run --release`) uses the canonical
    // dir. `cfg!(debug_assertions)` is false under `--release`, which is exactly
    // the split we want.
    let name = if cfg!(debug_assertions) { "arbiter-native-debug" } else { "arbiter-native" };
    Some(dirs::data_dir()?.join(name))
}

static SHIM: OnceLock<Option<claude_shim::ShimSetup>> = OnceLock::new();

/// Lazily set up the claude shim (idempotent file writes) on first shell spawn.
fn shim() -> Option<&'static claude_shim::ShimSetup> {
    SHIM.get_or_init(|| {
        let data_dir = app_data_dir()?;
        let path = std::env::var("PATH").unwrap_or_default();
        let exe = std::env::current_exe().ok()?;
        claude_shim::setup(&data_dir, &path, &exe)
    })
    .as_ref()
}

/// Prepend the shim `bin/` to PATH and export the shim env, so `claude` in the
/// spawned shell resolves to our launcher (which runs the real claude with our
/// generated `--settings` → statusLine/hook capture). Best-effort; a missing
/// shim just means no Claude stats, never a broken shell.
fn apply_claude_shim(cmd: &mut CommandBuilder) {
    let Some(s) = shim() else {
        claude_shim::debug_log("apply_claude_shim: shim() returned None (no shim set up)");
        return;
    };
    let sep = if cfg!(windows) { ";" } else { ":" };
    let path = std::env::var("PATH").unwrap_or_default();
    cmd.env("PATH", format!("{}{sep}{path}", s.bin_dir.display()));
    claude_shim::debug_log(&format!(
        "apply_claude_shim: PATH prepend bin={} real_claude={:?} settings={}",
        s.bin_dir.display(),
        s.real_claude,
        s.settings_file.display(),
    ));
    // zsh re-applies this last, after sourcing the user's rc (see ZSH_ZSHRC).
    cmd.env(claude_shim::SHIM_BIN_ENV, s.bin_dir.display().to_string());
    if let Some(rc) = &s.real_claude {
        cmd.env(claude_shim::REAL_CLAUDE_ENV, rc.display().to_string());
    }
    cmd.env(claude_shim::SETTINGS_ENV, s.settings_file.display().to_string());
    cmd.env(claude_shim::CAPTURE_DIR_ENV, s.capture_dir.display().to_string());
    cmd.env(claude_shim::HOOKS_DIR_ENV, s.hooks_dir.display().to_string());
    if let Some(orig) = &s.orig_statusline {
        cmd.env(claude_shim::ORIG_STATUSLINE_ENV, orig);
    }
}

// zsh ignores PROMPT_COMMAND, so we inject precmd/preexec hooks via a ZDOTDIR
// whose .z* files source the user's real startup files then add the emitters.
#[cfg(not(target_os = "windows"))]
const ZSH_ZSHENV: &str = "[[ -f \"${ARBITER_USER_ZDOTDIR:-$HOME}/.zshenv\" ]] && source \"${ARBITER_USER_ZDOTDIR:-$HOME}/.zshenv\"\n";
#[cfg(not(target_os = "windows"))]
const ZSH_ZPROFILE: &str = "[[ -f \"${ARBITER_USER_ZDOTDIR:-$HOME}/.zprofile\" ]] && source \"${ARBITER_USER_ZDOTDIR:-$HOME}/.zprofile\"\n";
#[cfg(not(target_os = "windows"))]
const ZSH_ZLOGIN: &str = "[[ -f \"${ARBITER_USER_ZDOTDIR:-$HOME}/.zlogin\" ]] && source \"${ARBITER_USER_ZDOTDIR:-$HOME}/.zlogin\"\n";
#[cfg(not(target_os = "windows"))]
const ZSH_ZSHRC: &str = r#"_arbiter_user_zdotdir="${ARBITER_USER_ZDOTDIR:-$HOME}"
ZDOTDIR="$_arbiter_user_zdotdir"
[[ -f "$_arbiter_user_zdotdir/.zshrc" ]] && source "$_arbiter_user_zdotdir/.zshrc"
unset _arbiter_user_zdotdir ARBITER_USER_ZDOTDIR

_arbiter_precmd() {
  local pwd_encoded="${PWD// /%20}"
  printf '\e]133;D\a\e]7;file://%s%s\a\e]133;A\a' "$HOST" "$pwd_encoded"
}
_arbiter_preexec() {
  printf '\e]133;C\a'
}
autoload -Uz add-zsh-hook 2>/dev/null
if (( $+functions[add-zsh-hook] )); then
  add-zsh-hook precmd _arbiter_precmd
  add-zsh-hook preexec _arbiter_preexec
fi

# Re-prepend Arbiter's claude-shim dir LAST (after the user's rc + path_helper),
# so `claude` resolves to our launcher. No-op until the shim sets ARBITER_SHIM_BIN.
[[ -n "$ARBITER_SHIM_BIN" ]] && export PATH="$ARBITER_SHIM_BIN:$PATH"

# Private per-pane command history: switch to a fresh history list backed by this
# pane's file (set AFTER the user's rc, so an oh-my-zsh HISTFILE can't win). `fc -p`
# pushes a new, isolated list read from the file — empty for a new terminal, the
# pane's prior commands on restore — so panes don't share each other's up-arrow and
# history survives relaunch. Set HISTFILE too so SAVEHIST writes back to our file.
if [[ -n "$ARBITER_HISTFILE" ]]; then
  export HISTFILE="$ARBITER_HISTFILE"
  fc -p "$ARBITER_HISTFILE" 2>/dev/null
  # Write each command to the file as it's entered, so history survives Arbiter
  # killing the shell on quit (zsh otherwise saves on exit, which a kill skips).
  setopt INC_APPEND_HISTORY 2>/dev/null
fi
"#;

#[cfg(not(target_os = "windows"))]
fn ensure_zsh_integration_dir() -> Option<std::path::PathBuf> {
    let dir = dirs::data_dir()?
        .join("arbiter-native")
        .join("shell-integration")
        .join("zsh");
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::write(dir.join(".zshenv"), ZSH_ZSHENV).ok()?;
    std::fs::write(dir.join(".zprofile"), ZSH_ZPROFILE).ok()?;
    std::fs::write(dir.join(".zshrc"), ZSH_ZSHRC).ok()?;
    std::fs::write(dir.join(".zlogin"), ZSH_ZLOGIN).ok()?;
    Some(dir)
}

/// Locate Git Bash (`bash.exe`) on Windows so a terminal can switch to it.
/// Checks the standard install dirs, then `where bash.exe` filtered to a Git
/// one (not WSL/System32). Returns None off Windows or when not installed.
pub fn detect_git_bash() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        let candidates =
            [r"C:\Program Files\Git\bin\bash.exe", r"C:\Program Files (x86)\Git\bin\bash.exe"];
        for path in candidates {
            if std::path::Path::new(path).exists() {
                return Some(path.to_string());
            }
        }
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("where")
            .arg("bash.exe")
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output()
            .ok()?;
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                let l = line.to_lowercase();
                if l.contains("git") && !l.contains("system32") {
                    return Some(line.trim().to_string());
                }
            }
        }
        None
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// PowerShell startup, run via `-File` (NOT `-Command`). Critical: PowerShell adds
/// the string passed to `-Command` to PSReadLine's interactive history, so our own
/// startup script kept showing up on up-arrow. A `-File` script is executed, not
/// recorded, so it can't pollute history. Functions are declared `global:` so they
/// survive into the interactive session (a script scope wouldn't). Sets OSC-7/133
/// emitters, per-pane private history (see the per-terminal-history feature), and
/// re-prepends the claude-shim dir after $PROFILE.
#[cfg(target_os = "windows")]
const PS_INIT: &str = r#"$global:__arbiter_orig_prompt = $function:prompt
# Point PSReadLine's history at this pane's file at SCRIPT TOP LEVEL — i.e. before
# the REPL's first ReadLine, which is when PSReadLine initialises its history. Set
# here, PSReadLine natively LOADS our file (not the global default, so no leakage)
# and SAVES to it incrementally. Doing this in the prompt function instead was too
# late: the engine had already loaded the default, and calling AddToHistory before
# first ReadLine throws a NullReferenceException. -File keeps the startup out of history.
if ($env:ARBITER_HISTFILE -and (Get-Module PSReadLine -ErrorAction SilentlyContinue)) {
  try {
    Set-PSReadLineOption -HistorySavePath $env:ARBITER_HISTFILE
    Set-PSReadLineOption -HistorySaveStyle SaveIncrementally
  } catch {}
}
function global:prompt {
  $loc = (Get-Location).Path
  $uri = 'file:///' + ($loc -replace '\\','/')
  $e = [char]27; $bel = [char]7
  [Console]::Write("${e}]133;C${bel}${e}]7;${uri}${bel}${e}]133;A${bel}")
  & $global:__arbiter_orig_prompt
}
if (Get-Module PSReadLine -ErrorAction SilentlyContinue) {
  Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
    param($key, $arg)
    [Console]::Write([char]27 + ']133;C' + [char]7)
    [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
  }
}
if ($env:ARBITER_SHIM_BIN -and ($env:PATH -split ';')[0] -ne $env:ARBITER_SHIM_BIN) { $env:PATH = $env:ARBITER_SHIM_BIN + ';' + $env:PATH }
if ($env:ARBITER_HIST_DEBUG) { try { [Console]::Error.WriteLine('[arbiter-hist] save=' + (Get-PSReadLineOption).HistorySavePath + ' style=' + (Get-PSReadLineOption).HistorySaveStyle) } catch {} }
"#;

/// Write the PowerShell init script and return its path (run via `-File`).
#[cfg(target_os = "windows")]
fn ensure_powershell_init() -> Option<std::path::PathBuf> {
    let dir = dirs::data_dir()?.join("arbiter-native").join("shell-integration").join("powershell");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("arbiter-init.ps1");
    std::fs::write(&path, PS_INIT).ok()?;
    Some(path)
}

/// Build the interactive shell command with OSC-7/OSC-133 emitters injected.
/// On Windows: `shell = Some(bash_path)` → Git Bash, else PowerShell.
#[cfg_attr(target_os = "windows", allow(unused_variables))]
pub fn build_shell_command(shell: Option<&str>) -> CommandBuilder {
    #[cfg(target_os = "windows")]
    {
        if let Some(bash_path) = shell {
            let mut cmd = CommandBuilder::new(bash_path);
            cmd.args(["--login", "-i"]);
            cmd.env(
                "PROMPT_COMMAND",
                concat!(
                    // One-time: switch to this pane's private history (Windows path →
                    // POSIX via cygpath for Git Bash). `history -c; history -r` isolates
                    // + loads our file. No-op until ARBITER_HISTFILE is set.
                    // One-time isolate (cygpath → POSIX path for Git Bash), then `history -a`
                    // on every prompt so commands persist immediately — Arbiter kills the
                    // shell on quit, so a save-on-exit would lose everything (the bug you saw:
                    // up-arrow empty after restart). No-op until ARBITER_HISTFILE is set.
                    r#"if [ -z "$_ARB_HIST" ] && [ -n "$ARBITER_HISTFILE" ]; then shopt -s histappend; export HISTFILE="$(cygpath -u "$ARBITER_HISTFILE" 2>/dev/null || echo "$ARBITER_HISTFILE")"; history -c; history -r; _ARB_HIST=1; [ -n "$ARBITER_HIST_DEBUG" ] && echo "[arbiter-hist] HISTFILE=$HISTFILE (from $ARBITER_HISTFILE)" >&2; fi; [ -n "$ARBITER_HISTFILE" ] && history -a; "#,
                    r#"printf '\e]133;D\a\e]7;file:///%s\a\e]133;A\a' "$(pwd -W | sed 's/ /%20/g' | sed 's/\\/\//g')""#,
                    // Re-prepend Arbiter's claude-shim dir LAST (after Git Bash's
                    // profile/rc, which may reorder PATH so the real claude wins), so
                    // `claude` resolves to our launcher and our --settings →
                    // statusLine/hook capture applies. Mirrors the macOS zsh precmd
                    // re-prepend. `cygpath -u` converts the Windows shim path to the
                    // /c/... form Git Bash's PATH uses; only prepends when not already
                    // first. No-op until the shim sets ARBITER_SHIM_BIN.
                    r#"; if [ -n "$ARBITER_SHIM_BIN" ]; then _sb=$(cygpath -u "$ARBITER_SHIM_BIN" 2>/dev/null); if [ -n "$_sb" ]; then case "$PATH" in "$_sb":*) ;; *) PATH="$_sb:$PATH";; esac; fi; unset _sb; fi"#,
                ),
            );
            cmd.env("PS0", "\x1b]133;C\x07");
            apply_claude_shim(&mut cmd);
            cmd
        } else {
            let mut cmd = CommandBuilder::new("powershell.exe");
            // Run the startup from a FILE, not `-Command` — `-Command` strings get added
            // to PSReadLine history (our own startup kept appearing on up-arrow). `-File`
            // is executed, not recorded. `-ExecutionPolicy Bypass` so a Restricted policy
            // can't block our own script. Falls back to a plain shell if the write fails.
            if let Some(init) = ensure_powershell_init() {
                cmd.args(["-NoExit", "-ExecutionPolicy", "Bypass", "-File"]);
                cmd.arg(&init);
            } else {
                cmd.arg("-NoExit");
            }
            apply_claude_shim(&mut cmd);
            cmd
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = shell;
        let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let shell_name = std::path::Path::new(&sh)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let mut cmd = CommandBuilder::new(&sh);
        cmd.arg("-l");
        cmd.env("TERM", "xterm-256color");
        // Advertise 24-bit colour so programs (e.g. Claude Code) emit their true
        // truecolor palette instead of a duller 256-colour approximation. Set it
        // ourselves rather than relying on inheriting it from whatever launched
        // Arbiter — a Finder-launched .app has no COLORTERM, so without this the
        // colours depend on the parent terminal (vivid from iTerm2, dull from the
        // app / a nested launch). The renderer is full truecolor regardless.
        cmd.env("COLORTERM", "truecolor");

        if shell_name.ends_with("zsh") {
            if let Some(zdotdir) = ensure_zsh_integration_dir() {
                if let Ok(orig) = std::env::var("ZDOTDIR") {
                    cmd.env("ARBITER_USER_ZDOTDIR", orig);
                }
                cmd.env("ZDOTDIR", zdotdir);
            }
        } else {
            cmd.env(
                "PROMPT_COMMAND",
                concat!(
                    // One-time: switch to this pane's private history. Runs at the first
                    // prompt (after .bashrc, so a bashrc HISTFILE can't win). `history -c`
                    // drops the shared history loaded at startup, `history -r` loads ONLY
                    // our file → isolated + persistent. No-op until ARBITER_HISTFILE is set.
                    // One-time: switch to this pane's private history (after .bashrc, so a
                    // bashrc HISTFILE can't win). `history -c` drops the shared history,
                    // `history -r` loads ONLY our file → isolation. Then `history -a` on
                    // EVERY prompt appends new commands to the file immediately, so they
                    // persist even though Arbiter kills the shell on quit (bash otherwise
                    // only saves on a clean exit). No-op until ARBITER_HISTFILE is set.
                    r#"if [ -z "$_ARB_HIST" ] && [ -n "$ARBITER_HISTFILE" ]; then shopt -s histappend; export HISTFILE="$ARBITER_HISTFILE"; history -c; history -r; _ARB_HIST=1; [ -n "$ARBITER_HIST_DEBUG" ] && echo "[arbiter-hist] HISTFILE=$HISTFILE" >&2; fi; [ -n "$ARBITER_HISTFILE" ] && history -a; "#,
                    r#"printf '\e]133;D\a\e]7;file://%s%s\a\e]133;A\a' "$(hostname)" "$(pwd)""#,
                ),
            );
            cmd.env("PS0", "\x1b]133;C\x07");
        }
        apply_claude_shim(&mut cmd);
        cmd
    }
}
