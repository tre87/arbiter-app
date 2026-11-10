# Claude usage bars: how they work

> **Status, 2026-11-10.** Describes the code as it stands on this date, read from the
> source. The sections "Why it is built this way" and "Dead ends" also draw on notes
> from when the feature was built (2026-06 to 2026-09); what comes from those notes and
> was not re-checked is marked.

The titlebar meters (5h, 7d, per-model weekly caps) and the same meters in the
overview's footer. They show the usage figures claude.ai shows on its own usage page,
for the account signed in to Arbiter's own hidden claude.ai webview.

This is separate from everything else Arbiter knows about Claude. It does not use
Claude Code's login, its OAuth token, its hooks or its transcripts, and signing out
here signs out of nothing else.

## Where things are

| File | What it holds |
|---|---|
| `src/bin/usage_helper.rs` | The helper process: a webview on claude.ai, the injected script, the stdin and stdout protocol. |
| `src/bin/iced_shell.rs`, "Claude usage" block (`UsageState` down to `usage_worker`) | Parsing, the helper's lifecycle, the poll thread, the watchdog. |
| `src/bin/iced_shell.rs`, `usage_section`, `overview_usage`, `usage_stat`, `refresh_btn` | Drawing. |
| `src/bin/iced_shell.rs`, `Message::UsageUpdated` and its neighbours | State changes. |
| `src/persist.rs` | Settings and the saved org. |
| `Cargo.toml`, feature `usage-helper` | The webview dependencies. |

## The shape of it

```
 Arbiter (iced)                          Arbiter --usage-helper (same binary)
 ──────────────                          ───────────────────────────────────
 usage_worker ── spawns ───────────────▶ tao window (hidden) + wry webview
      ▲                                   on https://claude.ai/
      │  stdout: one JSON line per fetch  │
      └───────────────────────────────────┤  INIT_SCRIPT, in the page:
                                          │   fetch /api/organizations
 poll thread ── stdin: fetch / reload ──▶ │   fetch /api/organizations/{id}/usage
 buttons     ── stdin: show / org:… / ──▶ │   window.ipc.postMessage(JSON)
                signout                   │
```

- **One binary, two processes.** `main` checks for `--usage-helper` before anything
  else and runs `usage_helper::run()` instead of the GUI. `spawn_usage_helper` starts
  `current_exe() --usage-helper` with stdin and stdout piped (stderr discarded).
- **The helper holds the webview; the app only reads JSON lines.** The webview never
  touches the terminal's event loop.
- **The helper exits when its stdin closes**, so it cannot outlive the app.

## Why it is built this way

- **A webview, because the data needs the browser session.** The figures come from
  `https://claude.ai/api/organizations/{uuid}/usage`, which is authorised by
  claude.ai's session cookies, not by an API key or Claude Code's token.
- **Not a plain HTTP client.** From the earlier notes (not re-checked): claude.ai's API
  sits behind a Cloudflare managed challenge that answers a non-browser client with
  403 whatever its headers, because the clearance cookie is bound to the browser's TLS
  fingerprint. Getting round that would mean impersonating a browser's TLS handshake,
  which is an arms race. A real webview passes without any of it.
- **Its own process, because a webview in the app's event loop is what the native
  rewrite got away from.** A stall or crash in the webview costs the bars, never a
  terminal.
- **The same binary, because a sibling helper binary was a footgun.** It was a
  separate `arbiter-usage-helper` once; `cargo run --bin arbiter` did not build it and
  usage silently hung.

## The helper (`usage_helper.rs`)

### Window and session

- A 460 by 680 window titled "Arbiter · Claude sign-in", created hidden. It is shown
  only when the app asks (`show`), and hidden again as soon as a line says login
  worked (`"ok":true` or `needs_org`). Closing it hides it; the helper keeps running.
- The session lives in `webview_data_dir()`: `<data dir>/arbiter` for a release
  build, `<data dir>/arbiter-debug` for a debug build, or `$ARBITER_DATA_DIR/webview`
  under test. So a debug build has its own claude.ai login, and an isolated test run
  never touches the real one.
- **macOS:** the process is an Accessory app (no dock icon, does not take focus at
  launch). On `show` it becomes a Regular app (dock icon, menu bar, frontmost) so the
  window is findable and gets the keyboard, and goes back to Accessory when hidden. A
  minimal Edit menu (`muda`) exists only so Cmd+V and friends work in the sign-in form.
- **Windows:** see "The hidden renderer on Windows" below.

### The injected script (`INIT_SCRIPT`)

Runs in the claude.ai page with the page's own cookies.

1. `GET /api/organizations`. A network failure or a 5xx or 429 is `error`; only 401
   or 403, or a 2xx body that is not JSON (the login page), is `needs_login`. An
   outage therefore never looks like being signed out.
2. Pick the org: the chosen one if it is still in the list, else the only one, else
   post `needs_org` with the list and stop.
3. `GET /api/organizations/{uuid}/usage` and post the compact result.

It refetches 800 ms after load, on a 120 s interval of its own (unreliable while
hidden, which is why the app drives the cadence), and when the page leaves `/login`.

Details that matter:

- **Top frame only** (`window.top !== window.self` returns at once). WebView2 runs an
  init script in every frame, and the script's "wrong page, go to claude.ai" rescue
  then navigated each of claude.ai's own subframes, whose subframes were navigated in
  turn: a page load every two seconds until the renderer ran out of memory.
- **The chosen org is kept in claude.ai's `localStorage`** (`arbiterUsageOrg`), so a
  reload goes straight to data.
- **`cache: 'no-store'` on both fetches.** A cache-busting query parameter is not an
  option: the API rejects the extra parameter.
- **Fable's weekly cap is not a top-level window.** It is an entry in `usage.limits`
  of kind `weekly_scoped` whose `scope.model.display_name` is "fable"; `scoped()`
  finds it. Another per-model cap would be read the same way.
- **`plan` is inferred**, not reported: "Max" if there is an Opus or Sonnet weekly
  window, "Pro" if there is a 7d window, else "Free".

### Protocol

**Helper to app, one JSON object per stdout line:**

```json
{"ok":true,"plan":"Max","org_uuid":"…","org_name":"…","orgs":[{"uuid":"…","name":"…"}],
 "five_hour":{"utilization":42,"resets_at_ms":1794300000000},
 "seven_day":{…},"seven_day_opus":{…}|null,"seven_day_sonnet":{…}|null,"seven_day_fable":{…}|null}
{"ok":false,"error":"needs_login"}
{"ok":false,"error":"needs_org","orgs":[{"uuid":"…","name":"…"}]}
{"ok":false,"error":"error"}
```

`utilization` is 0 to 100; `resets_at_ms` is epoch milliseconds or null. A window the
plan does not have is null, and gets no meter.

**App to helper, one word per stdin line:**

| Line | Effect |
|---|---|
| `show` | Show the sign-in window. |
| `fetch` | Refetch in the page that is already loaded. Sub-second. |
| `reload` | Navigate to claude.ai again; the script re-runs and fetches. Recovers a dead renderer. |
| `org:<uuid>` | Use this org, remember it, refetch. |
| `signout` | Clear this webview's browsing data and reload, which reports `needs_login`. |

### The hidden renderer on Windows

A hidden window's WebView2 renderer is backgrounded by Chromium and stops running
script: `evaluate_script` returns success and nothing happens. Three layers deal with
it, and all three are needed:

1. **Browser flags** at build (`--disable-renderer-backgrounding`,
   `CalculateNativeWinOcclusion` and `IntensiveWakeUpThrottling` disabled, and
   others). They lower the pressure; alone they were not enough.
2. **Suspend and resume** (`win_suspend`, `ICoreWebView2_3`), Microsoft's documented
   pattern for an invisible webview that syncs data. The controller is kept invisible
   and suspended; each `fetch` does `Resume`, then the in-page refetch; the `ok` line
   that comes back sends `Sleep`, which calls `TrySuspend`. `TrySuspend` fails unless
   the controller is invisible, which is why `Hide` sets that first. `needs_org` does
   not sleep, because an `org:` refetch follows.
3. **Reload** when a fetch goes unanswered: Chromium can discard a long-suspended
   renderer, and `Resume` cannot bring a discarded one back, but a navigation spawns a
   new one.

macOS does none of this: WKWebView keeps running script while hidden.

The Windows COM code cannot be run from macOS, but it type-checks there:
`cargo check --bin arbiter --target x86_64-pc-windows-gnu --features usage-helper`.

## The app side

### State

`UsageData` holds a `UsageState` and the figures:

| State | Meaning | Shown |
|---|---|---|
| `Pending` | First fetch in flight, or waiting after an org choice. | Three pulsing dots. |
| `NeedsLogin` | Not signed in, or the helper is not running. | "Claude Usage Sign In" pill. |
| `NeedsOrg` | Signed in, several orgs, none chosen. | "Choose Claude org" pill. |
| `Error` | Signed in, the fetch failed. | Amber "Usage unavailable", click to retry. |
| `Ok` | Have figures. | The meters and the refresh button. |

Rules in `Message::UsageUpdated` worth knowing:

- **A failed refresh keeps the last good figures.** A failure line carries no meters;
  the handler copies the previous ones in when the state was `Ok`. `usage_section`
  then draws the bars whenever there are any, so `Pending` and `Error` only show
  their own indicator when there is nothing to show at all.
- **A saved org is applied without asking.** On `needs_org`, if `SavedState::usage_org`
  is still in the list, the app sends `org:<uuid>` and stays `Pending`; if it is not,
  the saved choice is dropped and the picker shows.
- **Every line clears `USAGE_FETCH_PENDING`**: the helper spoke, so its renderer is
  alive.
- **`set_usage_poll` must follow every change of `usage.state`.** The poll thread
  cannot see `State`; it reads two flags (`USAGE_POLL_OK`, `USAGE_POLL_RETRY`) that
  mirror it.

### The cadence and the watchdog

The refresh cadence is the app's, on a **background thread** (`start_usage_poll`),
not the UI tick: an idle Arbiter draws no frames for it. Every `USAGE_REFRESH_MS`
(120 s):

- State is not `Ok` or `Error`: do nothing. A sign-in in progress is never disturbed.
- Otherwise one rung of a ladder, chosen by `usage_poke`, by how many cycles in a row
  went unanswered:

| Unanswered cycles | `Ok` | `Error` |
|---|---|---|
| 0 | `fetch` | `reload` |
| 1 | `reload` | `reload` |
| 2 or more | restart the helper | restart the helper |

"Unanswered" means `USAGE_FETCH_PENDING` was still set from the cycle before. So a
webview that goes silent is refetched, then reloaded, then replaced, within about six
minutes.

**Restarting** (`restart_usage_helper`) drops the stdin pipe and kills the process.
Its stdout closes, `usage_worker`'s read loop ends, it reports `NeedsLogin` at once,
waits `USAGE_HELPER_RESPAWN_MS` (2 s) and spawns a new helper. A helper that posts
nothing at all for `USAGE_HELPER_MAX_SILENT_RUNS` (3) runs in a row is not respawned
again: that is a build without the feature, or a machine with no working webview.

**Manual refresh** (`Message::RefreshUsage`): `fetch` from `Ok` (in place, so the
bars stay), `reload` from anything else. The arrow spins while
`usage_refresh_started_ms` is set, which is also what keeps the fast tick on for it.
The tick escalates an unanswered manual fetch to a `reload` after
`USAGE_MANUAL_ESCALATE_MS` (4 s) and stops the spin after `USAGE_SPIN_MAX_MS` (15 s)
whatever happened.

**Loading that never ends:** `Pending` for `USAGE_PENDING_TIMEOUT_MS` (30 s) becomes
`NeedsLogin`. This is the last resort for a helper that is alive and silent. It is
long on purpose: at 8 s a slow but successful load flashed "Sign in" first.

### Drawing

- `usage_section` returns the titlebar element and an estimated width, which
  `titlebar_row` uses to budget the workspace tabs.
- `overview_usage` is the same data in the overview's footer.
- Meters, in order: Fable (purple, only with "Show Fable usage", no timer of its own
  since it resets with 7d), 5h (azure), 7d (green), Opus (green), Sonnet (azure,
  hidden unless "Hide Sonnet usage" is off). One per window that exists.
- `fmt_reset` turns `resets_at_ms` into "1d 22h", "1h 41m", "15m" or "now".
- The refresh button is an icon with no countdown: a per-second countdown would need
  a repaint every second, which is the idle clock that was removed.

### Signing in, orgs, signing out

- The titlebar pill opens an explanation first (`ShowUsageLoginPrompt`,
  `usage_login_prompt_view`), and its × hides the usage section for good
  (`hide_usage_bar`). Confirming sends `show`.
- The org picker is `usage_org_menu_view`; a choice is saved in
  `SavedState::usage_org` and sent as `org:<uuid>`.
- Sign out (Settings, Claude Usage, Account) sends `signout`, forgets the saved org
  and goes straight to `NeedsLogin`.

## Settings and saved state

In `persist::Settings` (Settings, Claude Usage):

| Field | Default | Effect |
|---|---|---|
| `hide_usage_bar` | off | Hides the usage section everywhere, the overview footer included. |
| `hide_sonnet_usage` | on | Hides the Sonnet weekly meter. |
| `show_fable_usage` | off | Shows Fable's weekly meter. |
| `overview_usage_footer` | on | Shows the meters in the overview's footer. |

`SavedState::usage_org` is the chosen org's uuid. The claude.ai session itself is not
in `session.json`; it is the webview's own data directory.

## Build

- Feature `usage-helper` pulls `wry` 0.45, `tao` 0.30, `muda` 0.13 and, on Windows,
  `webview2-com`. It is in the default features, so `cargo build --bin arbiter`
  includes it.
- `--no-default-features` builds without a webview. The helper process then exits at
  once, the bars show "Sign in", and after three silent runs the app stops trying.
- The Windows installer also installs the WebView2 runtime the helper needs.

## Dead ends (do not retry)

From the changelog and the earlier notes; none re-tested for this document.

- **Fetching from Rust with the session cookies.** Blocked by the Cloudflare
  challenge; see "Why it is built this way".
- **A cache-busting query parameter** on the usage call. The API rejects it.
- **Browser flags alone** to keep the hidden Windows renderer alive. Not enough.
- **Reloading the page for every refresh.** It worked, and it is what `reload` still
  is, but a cold page has a bootstrap of most of a second in which a call can fail,
  and that failure used to blank the bars. Routine refreshes are in-page now.
- **An 8 second loading timeout.** Flashed "Sign in" on slow loads.
- **A per-second refresh countdown.** Kept the app from ever being idle.
- **A separate helper binary.** Not built by a plain `cargo run`.
- **The helper's own 120 s timer as the cadence.** Webviews throttle timers in a
  hidden page.

## Working on it

- **There is no debug switch any more.** An `ARBITER_USAGE_DEBUG` log existed once
  and is gone from the source. The helper's stderr is discarded by
  `spawn_usage_helper`. To see what the helper says, run it by hand:
  `ARBITER_DATA_DIR=/tmp/arbiter-test ./target/debug/arbiter --usage-helper`
  prints each line it would send; type `show`, `fetch`, `reload` on its stdin. With
  that data dir it uses its own empty session, so it will report `needs_login` until
  signed in there.
- **The ladder is unit-tested** without a helper:
  `usage_poll_escalates_to_a_restart_when_the_helper_stays_silent` (`usage_poke`).
  Parsing (`parse_usage_line`) and the keep-the-last-figures rule have no tests.
- **Changing what a meter shows:** the script's `post({...})` at the end of
  `fetchUsage`, then `HelperLine`, `UsageData`, `parse_usage_line`, the copy-forward
  block in `Message::UsageUpdated`, `have_bars`, and the `entries` list in
  `usage_section` and `overview_usage`. A new field must be added in all of them, or a
  failed refresh will drop it.
- **Changing the API calls:** only the script. Keep the top-frame guard and the
  401/403-only rule for `needs_login`.
- **Measure idle CPU after touching the cadence.** The point of the background thread
  is that an idle, signed-in Arbiter draws nothing.

## Known gaps

- **The figures can be up to two minutes old**, and there is nothing on screen that
  says how old.
- **`plan` is a guess** from which windows exist.
- **Only Fable's per-model cap is read** from `limits`; any other scoped limit is
  ignored.
- **macOS shows a dock icon** while the sign-in window is open (it has to, to be a
  normal foreground app).
- **claude.ai's API is not a public contract.** A change to the two endpoints or to
  the shape of `usage` breaks the bars until the script is updated; the symptom would
  be "Usage unavailable" while signed in.
