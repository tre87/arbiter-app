use portable_pty::CommandBuilder;
#[cfg(not(target_os = "windows"))]
use std::path::PathBuf;
use tauri::AppHandle;
#[cfg(not(target_os = "windows"))]
use tauri::Manager;

// zsh ignores PROMPT_COMMAND, so we install precmd/preexec hooks via ZDOTDIR
// injection: point zsh at an Arbiter-managed dir whose .z* files source the
// user's real startup files and then add OSC 7 / OSC 133 emitters.
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
"#;

#[cfg(not(target_os = "windows"))]
fn ensure_zsh_integration_dir(app: &AppHandle) -> Option<PathBuf> {
    let data_dir = app.path().app_data_dir().ok()?;
    let dir = data_dir.join("shell-integration").join("zsh");
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::write(dir.join(".zshenv"), ZSH_ZSHENV).ok()?;
    std::fs::write(dir.join(".zprofile"), ZSH_ZPROFILE).ok()?;
    std::fs::write(dir.join(".zshrc"), ZSH_ZSHRC).ok()?;
    std::fs::write(dir.join(".zlogin"), ZSH_ZLOGIN).ok()?;
    Some(dir)
}

#[tauri::command]
pub fn check_git_bash() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        let candidates = [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ];
        for path in &candidates {
            if std::path::Path::new(path).exists() {
                return Some(path.to_string());
            }
        }
        // Fallback: check PATH via `where bash.exe`, filtering out WSL/System32
        if let Ok(output) = crate::util::hidden_command("where").arg("bash.exe").output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    let lower = line.to_lowercase();
                    if lower.contains("git") && !lower.contains("system32") {
                        return Some(line.trim().to_string());
                    }
                }
            }
        }
        None
    }
    #[cfg(not(target_os = "windows"))]
    { None }
}

#[cfg_attr(target_os = "windows", allow(unused_variables))]
pub fn build_shell_command(app: &AppHandle, shell: Option<&str>) -> CommandBuilder {
    // OSC 133 (FinalTerm prompt markers) lets the PTY parser emit
    // `shell-activity-{sid}` events without polling sysinfo:
    //   133;A → prompt start (idle)
    //   133;C → pre-execution (busy)
    //   133;D → command finished (idle)
    // We embed these in PROMPT_COMMAND / PS0 / the PS prompt function so users
    // don't need any shell-init changes.

    #[cfg(target_os = "windows")]
    {
        if let Some(bash_path) = shell {
            // Git Bash on Windows — use PROMPT_COMMAND with pwd -W for Windows paths
            let mut cmd = CommandBuilder::new(bash_path);
            cmd.args(["--login", "-i"]);
            cmd.env(
                "PROMPT_COMMAND",
                concat!(
                    // D (prev command finished), 7 (cwd), A (new prompt starts)
                    r#"printf '\e]133;D\a\e]7;file:///%s\a\e]133;A\a' "$(pwd -W | sed 's/ /%20/g' | sed 's/\\/\//g')""#,
                ),
            );
            // PS0 is emitted by bash just before executing the command — literal
            // ESC/BEL bytes so bash doesn't need to parse `\e`/`\a` escapes.
            cmd.env("PS0", "\x1b]133;C\x07");
            cmd
        } else {
            let mut cmd = CommandBuilder::new("powershell.exe");
            // -NoExit keeps the shell interactive after running the setup command.
            // The prompt override emits OSC 133 C (busy), then OSC 7 (cwd),
            // then OSC 133 A (idle). The C→A pair guarantees a busy→idle
            // transition on every prompt render, which is critical because
            // PSReadLine's Enter key handler does NOT fire for programmatic
            // `\r` input — without the leading C, the backend's transition-
            // based dedup suppresses the idle event when prev_idle is already
            // true, breaking exit detection for programmatically-launched
            // Claude sessions.
            cmd.args([
                "-NoExit",
                "-Command",
                concat!(
                    "$__arbiter_orig_prompt = $function:prompt; ",
                    "function prompt { ",
                        "$loc = (Get-Location).Path; ",
                        "$uri = 'file:///' + ($loc -replace '\\\\','/'); ",
                        "$e = [char]27; $bel = [char]7; ",
                        "[Console]::Write(\"${e}]133;C${bel}${e}]7;${uri}${bel}${e}]133;A${bel}\"); ",
                        "& $__arbiter_orig_prompt ",
                    "}; ",
                    "if (Get-Module PSReadLine -ErrorAction SilentlyContinue) { ",
                        "Set-PSReadLineKeyHandler -Key Enter -ScriptBlock { ",
                            "param($key, $arg) ",
                            "[Console]::Write([char]27 + ']133;C' + [char]7); ",
                            "[Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine() ",
                        "} ",
                    "}"
                ),
            ]);
            cmd
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = shell; // unused on non-Windows
        let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let shell_name = std::path::Path::new(&sh)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let mut cmd = CommandBuilder::new(&sh);
        cmd.arg("-l");

        if shell_name.ends_with("zsh") {
            // zsh ignores PROMPT_COMMAND. Point it at our ZDOTDIR so the
            // wrapper .zshrc sources the user's real rc files and then installs
            // precmd/preexec hooks emitting OSC 7 + OSC 133.
            if let Some(zdotdir) = ensure_zsh_integration_dir(app) {
                if let Ok(orig) = std::env::var("ZDOTDIR") {
                    cmd.env("ARBITER_USER_ZDOTDIR", orig);
                }
                cmd.env("ZDOTDIR", zdotdir);
            }
        } else {
            // bash PROMPT_COMMAND: emit OSC 133 D (command finished) + OSC 7
            // (cwd) + OSC 133 A (prompt start).
            cmd.env(
                "PROMPT_COMMAND",
                r#"printf '\e]133;D\a\e]7;file://%s%s\a\e]133;A\a' "$(hostname)" "$(pwd)""#,
            );
            // PS0 fires just before executing a command → OSC 133 C (busy). Literal
            // bytes so bash doesn't re-interpret the escapes.
            cmd.env("PS0", "\x1b]133;C\x07");
        }
        cmd
    }
}
