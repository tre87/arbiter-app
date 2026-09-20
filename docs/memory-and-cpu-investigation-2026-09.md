# Memory and idle CPU in a long-running Arbiter (investigation, September 2026)

Arbiter runs for days and weeks without a restart. This records what a two-day, 18-pane
run (v1.5.0, Windows 11, RTX 5080) looked like from the inside, how it was measured, what
was fixed as a result, what turned out not to be ours, and how to measure again without
repeating the mistakes. Companion to the `[Unreleased]` entry in `CHANGELOG.md` and the
"Memory" section of `CLAUDE.md`.

## Symptom

Task Manager showed the `arbiter-native` group at 822 MB and 0.1 to 0.5 % CPU with the app
idle-ish. Over the investigation the main process went from 767 MB to 1.17 GB of private
bytes. Task Manager's Memory column includes compressed pages, so it exceeds the working
set; private bytes is the number to compare over time.

## Where the memory was (census of the live process)

| Component | 2-day instance | Fresh, 1 pane | Behaviour |
|---|---|---|---|
| Fixed baseline: fonts, iced, wgpu/DX12 driver, DirectWrite | about 240 MB | 240 to 258 MB | constant |
| Per-pane GPU renderers (1 MiB mono + 4 MiB colour atlas copy, cloned font bytes) | 29 to 31 for 18 panes, about 200 MB | 1 | grows per pane ever drawn, never freed (fixed) |
| NT heap segments, Rust allocations | 297 to 313 MB | 31 to 45 MB | mostly blank alacritty rows, see below |
| GPU upload heaps (write-combined 32 MB blocks, wgpu staging) | 252 to 316 MB | about 100 MB | high-water mark of staging traffic (reduced) |
| Fourteen identical 16 MiB heap blocks, all zero, never written | 220 MB | 0 | pool that appears under sustained rendering, occasionally recycled; not ours (see below) |

Working set jumped from 375 to 658 MB during the investigation because the census scripts
read the process's pages; that is an artefact of measuring, not of the app.

Heap segment contents sampled as repeating 24-byte units with a space character and
default colours: alacritty `Cell`s of blank rows. alacritty allocates history rows 1000 at a
time (`MAX_CACHE_SIZE`) as lines scroll off, so every pane whose content ever scrolls
carries multi-megabyte blocks of pre-allocated rows. Claude Code (2.1.276) does not use the
alternate screen; its redraws scroll, so a Claude pane fills its 5000-line history even
when it looks empty.

## CPU

Total CPU was 36 minutes over 55 hours, about 0.5 % of one core on average and 12 % of one
core while a Claude pane worked (0.75 % of a 16-thread machine, which is what Task Manager
shows). Half of all CPU time was on the main thread. Causes, all addressed:

- A thread was spawned per output burst (`WakeHold::extend`), per cursor-hide grace and
  per frozen frame: at least 7.7 spawns a second sampled, undercounted.
- Any pane's output, including panes on hidden workspaces, rebuilt the whole iced view and
  re-walked every visible grid, re-uploading its instance buffer, with no check for change.
- A 16 ms tick runs while any Claude pane is working, by design; with 18 Claude panes one
  nearly always is, so the per-frame cost above was paid at 60 fps most of the day.

## What was fixed (commit dfe2c45)

- Renderers are freed: a dropped `Session` pushes its id to `session::RETIRED`, and
  `TermPrimitive::prepare` removes those entries from the store. The Claude monitor thread
  of a closed pane exits (`closed` flag, condvar notified in `Session`'s Drop).
- Frames rebuild only on change: `VtTerm::generation` bumps on every visible mutation, and
  `gpu::FrameKey` (generation, cursor, background, canvas) gates the grid walk and upload.
  `term::tests::generation_moves_with_every_visible_change` pins the list of mutators.
- Atlas uploads are per dirty rectangle; a full atlas flushes and re-rasterises instead of
  indexing out of bounds; font bytes are shared through `Arc<FontSpec>`.
- One deadline thread (`session::schedule`) replaces spawn-per-event, parked with no
  timeout while idle.
- Scrollback is `term::CLAUDE_SCROLLBACK` (1000) while `claude_running || on_screen`, the
  setting otherwise; switched in the reader loop, applied with alacritty's `set_options`.
- `WGPU_BACKEND=dx12` is set at startup on Windows unless already set.
- `exit` (code 0) closes the pane via a per-session exit watcher thread; ConPTY gives the
  reader no EOF when the child ends.
- `ARBITER_MEM_DIAG=1` enables a logging allocator (`src/memdiag.rs`).

Not changed: wake routing per pane (measure after the above), a wgpu upgrade, the
scrollback default itself.

## The 16 MiB blocks: what is known

- Exactly 16 MiB payload each plus a heap header page, allocated through `HeapAlloc` on
  the process heap (their entries link into that heap's large-block list), zero throughout
  and never written (demand-zero, so absent from the working set).
- Zero after two idle-ish days; 13 appeared within one hour of continuous rendering with a
  Claude pane working; then 13 to 14 with one freed and one allocated per hour or so.
- Never reproduced in isolated instances: idle, streaming 12,000 lines, printing glyphs
  from two dozen scripts and symbol blocks, opening and closing the overview window ten
  times, splitting and closing six panes, playing the notification sound twelve times.
  The logging allocator saw no Rust allocation of 8 MiB or more in any of them, and a
  three-minute ETW heap trace of the live process contained no such allocation either
  (only one free).
- No allocation site in this tree or its dependencies produces a retained zero 16 MiB
  buffer (checked: alacritty rows and history, vte's 2 MiB sync buffer, iced atlases and
  pixmaps, wgpu staging, cosmic-text, sysinfo, notify, portable-pty).
- `nvspcap64.dll`, the NVIDIA in-game overlay's capture hook, was loaded in the process,
  and its container services run on this machine. A pre-allocated capture ring that
  engages under sustained presentation fits every observation. Untested: disable the
  overlay (NVIDIA app, Settings, In-Game Overlay), restart Arbiter, work an hour, compare.

Other results worth keeping: iced window open and close returns its memory within a few
seconds (the overview test), so notification cards do not leak; PlaySound showed no
meaningful per-play growth; cosmic-text's shape-run cache is behind a feature iced does not
enable, so it does not exist in this build.

## Measuring again, safely

Read the warning first. On 2026-09-20 Windows Defender's behaviour classifier flagged
`arbiter.exe` as Trojan:Win32/Bearfoos.A!ml and killed the app. The trigger was the
diagnostics themselves: PowerShell scripts compiled with P/Invoke that opened Arbiter's
process, read its memory, sent it keystrokes and traced its heap, all run from a shell
whose ancestor was `arbiter.exe` (Claude Code in a pane). Seen as one process tree, that is
what malware does. Rules:

- Passive counters only from inside a pane: `Get-Process`, `Get-Counter`. No
  `OpenProcess` on Arbiter, no `ReadProcessMemory`, no `VirtualQueryEx`, no SendKeys, no
  ETW sessions targeting it.
- Anything deeper runs from a console the user opens themselves, outside Arbiter, ideally
  elevated. The census below is safe from there.
- Prefer the built-in switch: `ARBITER_MEM_DIAG=1` before launch, then read
  `%TEMP%\arbiter-mem-diag.log` (allocations of 8 MiB or more with backtraces, plus a
  summary line a minute). Build with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` for
  readable frames.

Passive trend, from any shell:

```powershell
$p = Get-Process arbiter | Sort-Object StartTime | Select-Object -First 1
while ($true) { $p.Refresh(); '{0:HH:mm}  priv {1,7:N1} MB  ws {2,7:N1} MB  threads {3}' -f (Get-Date), ($p.PrivateMemorySize64/1MB), ($p.WorkingSet64/1MB), $p.Threads.Count; Start-Sleep 300 }
```

Region census (metadata only, no memory reads), run from a console outside Arbiter. It
groups committed private memory by allocation base and reports size buckets, the largest
allocations, write-combined totals (GPU upload heaps) and the count of exactly-16 MiB
blocks:

```powershell
param([int]$ProcId)
$src = @'
using System; using System.Collections.Generic; using System.Runtime.InteropServices;
public static class VQ {
  [StructLayout(LayoutKind.Sequential)] public struct MBI { public IntPtr BaseAddress, AllocationBase; public uint AllocationProtect, _a; public IntPtr RegionSize; public uint State, Protect, Type, _b; }
  [DllImport("kernel32.dll", SetLastError=true)] public static extern IntPtr OpenProcess(uint a, bool i, int pid);
  [DllImport("kernel32.dll", SetLastError=true)] public static extern int VirtualQueryEx(IntPtr h, IntPtr addr, out MBI m, IntPtr len);
  public static string Census(int pid) {
    var h = OpenProcess(0x0400, false, pid); var commit = new Dictionary<long,long>(); long wc = 0, addr = 0; MBI m;
    while (addr < 0x7FFFFFFF0000L) {
      if (VirtualQueryEx(h, (IntPtr)addr, out m, (IntPtr)Marshal.SizeOf(typeof(MBI))) == 0) break;
      long rs = (long)m.RegionSize;
      if (m.State == 0x1000 && m.Type == 0x20000) { long ab = (long)m.AllocationBase; long c; commit.TryGetValue(ab, out c); commit[ab] = c + rs; if ((m.Protect & 0x400) != 0) wc += rs; }
      addr = (long)m.BaseAddress + rs;
    }
    int n16 = 0, n4 = 0, n1 = 0; long segs = 0, total = 0;
    foreach (var kv in commit) { total += kv.Value; if (kv.Value == 16781312) n16++; else if (kv.Value == 4198400) n4++; else if (kv.Value == 1052672) n1++; else if (kv.Value >= 14L*1048576 && kv.Value <= (long)(16.5*1048576)) segs += kv.Value; }
    return string.Format("private {0:N0} MB  16MiB-blocks {1}  colour-atlases(4MiB) {2}  mono-atlases(1MiB) {3}  heap-segments {4:N0} MB  write-combined {5:N0} MB", total/1048576.0, n16, n4, n1, segs/1048576.0, wc/1048576.0);
  }
}
'@
Add-Type -TypeDefinition $src
[VQ]::Census($ProcId)
```

The 4 MiB and 1 MiB counts equal the number of live terminal renderers (one pair per pane
ever drawn); with the retire list they should match the panes currently on screen or
recently shown, not every pane ever opened.

Isolated experiments: launch `arbiter.exe` with `ARBITER_DATA_DIR` pointing at a scratch
folder holding a hand-written `session.json` (copy the real one's shape; a `Leaf` with a
`startup_cmd` runs that command in the pane at start, which is how the streaming and glyph
tests were driven without keystrokes). Set `notifications` to false there unless cards are
the subject.

## Expectations after the fixes

Arbiter's own footprint is the fixed baseline plus about 2 MB per pane plus whatever
scrollback holds (24 MB per pane at 5000 lines and 200 columns when full; 1000 lines while
Claude owns it). Nothing in the app's code grows with uptime. Two external pools saturate
rather than leak: the GPU upload heaps, which the change-gated rebuild starves, and the
overlay's ring if the overlay stays on. A GPU-rendered terminal on Windows with a dozen
panes sits at 200 to 400 MB in comparable apps; that is the target here.
