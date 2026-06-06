//! Shell-integration: build the login shell command with OSC-7 (cwd) + OSC-133
//! (FinalTerm prompt markers: A/D=idle, B/C=busy) emitters injected, so the
//! terminal can detect cwd changes + shell busy/idle without polling. Ported
//! from `src-tauri/src/shell.rs` (the OSC-133 injection; the claude-shim
//! PATH/statusLine setup comes with the statusLine work later). Tauri-free:
//! the zsh integration dir lives under the per-OS data dir via `dirs`.

use portable_pty::CommandBuilder;

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
                    r#"printf '\e]133;D\a\e]7;file:///%s\a\e]133;A\a' "$(pwd -W | sed 's/ /%20/g' | sed 's/\\/\//g')""#,
                ),
            );
            cmd.env("PS0", "\x1b]133;C\x07");
            cmd
        } else {
            let mut cmd = CommandBuilder::new("powershell.exe");
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
        let _ = shell;
        let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let shell_name = std::path::Path::new(&sh)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let mut cmd = CommandBuilder::new(&sh);
        cmd.arg("-l");
        cmd.env("TERM", "xterm-256color");

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
                r#"printf '\e]133;D\a\e]7;file://%s%s\a\e]133;A\a' "$(hostname)" "$(pwd)""#,
            );
            cmd.env("PS0", "\x1b]133;C\x07");
        }
        cmd
    }
}
