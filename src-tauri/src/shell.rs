use portable_pty::CommandBuilder;

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
        if let Ok(output) = std::process::Command::new("where").arg("bash.exe").output() {
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

pub fn build_shell_command(shell: Option<&str>) -> CommandBuilder {
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
        let mut cmd = CommandBuilder::new(&sh);
        cmd.arg("-l");
        // Bash PROMPT_COMMAND: emit OSC 133 D (command finished) + OSC 7 (cwd)
        // + OSC 133 A (prompt start). Works for bash; zsh users typically have
        // precmd hooks from their rc files instead.
        cmd.env(
            "PROMPT_COMMAND",
            r#"printf '\e]133;D\a\e]7;file://%s%s\a\e]133;A\a' "$(hostname)" "$(pwd)""#,
        );
        // PS0 fires just before executing a command → OSC 133 C (busy). Literal
        // bytes so bash doesn't re-interpret the escapes.
        cmd.env("PS0", "\x1b]133;C\x07");
        cmd
    }
}
