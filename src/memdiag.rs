//! Diagnostic allocator, off unless `ARBITER_MEM_DIAG` is set in the environment. On, every
//! Rust allocation of `BIG` bytes or more is appended to `<temp>/arbiter-mem-diag.log` with a
//! backtrace, every free of such a block is noted, and a summary of the live big blocks is
//! appended once a minute. Off, an allocation costs one atomic load.
//!
//! Written to attribute the identical 16 MiB heap blocks that accumulated in a two-day run
//! (2026-09-20). A block this allocator never sees came from a non-Rust component (a COM
//! object, the graphics driver), which is as useful to know as a backtrace.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

pub struct DiagAlloc;

// Blocks below this are ordinary churn; the ones under investigation are 16 MiB.
const BIG: usize = 8 * 1024 * 1024;

// 0 not yet read, 1 off, 2 on. Read once, since reading the environment allocates.
static MODE: AtomicU8 = AtomicU8::new(0);

// Live big blocks as (address, size). A Vec, not a map: there are at most dozens.
static LIVE: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

thread_local! {
    // Set while the hook runs on this thread, so the allocations the hook itself makes
    // (backtrace, formatting, the file) are not logged in turn.
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

pub fn enabled() -> bool {
    match MODE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            if IN_HOOK.with(|f| f.replace(true)) {
                return false;
            }
            let on = std::env::var_os("ARBITER_MEM_DIAG").is_some();
            MODE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            IN_HOOK.with(|f| f.set(false));
            on
        }
    }
}

fn log_path() -> std::path::PathBuf {
    std::env::temp_dir().join("arbiter-mem-diag.log")
}

fn append(text: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = f.write_all(text.as_bytes());
    }
}

fn stamp() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("[{ms} pid={}]", std::process::id())
}

fn note(kind: &str, ptr: *mut u8, size: usize) {
    if !enabled() || IN_HOOK.with(|f| f.replace(true)) {
        return;
    }
    let (count, total) = {
        let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
        match kind {
            "free" => live.retain(|&(p, _)| p != ptr as usize),
            _ => live.push((ptr as usize, size)),
        }
        (live.len(), live.iter().map(|&(_, s)| s).sum::<usize>())
    };
    let mut line = format!(
        "{} {kind} {size} bytes @{ptr:p}  live big blocks {count} = {} MiB\n",
        stamp(),
        total / (1024 * 1024)
    );
    if kind != "free" {
        line.push_str(&format!("{}\n", std::backtrace::Backtrace::force_capture()));
    }
    append(&line);
    IN_HOOK.with(|f| f.set(false));
}

// Once a minute, the live big blocks and the process's own memory counters. Diagnostic
// only, hence a sleeping thread, and only when the variable is set.
pub fn start_summary_thread() {
    if !enabled() {
        return;
    }
    append(&format!("{} diag allocator on; summary every 60 s\n", stamp()));
    std::thread::spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
        let snapshot: Vec<(usize, usize)> = LIVE.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let sizes: Vec<String> = snapshot.iter().map(|&(_, s)| format!("{}", s / (1024 * 1024))).collect();
        let mut sys = sysinfo::System::new();
        let pid = sysinfo::Pid::from_u32(std::process::id());
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[pid]),
            true,
            sysinfo::ProcessRefreshKind::new().with_memory(),
        );
        let (ws, virt) = sys
            .process(pid)
            .map(|p| (p.memory() / (1024 * 1024), p.virtual_memory() / (1024 * 1024)))
            .unwrap_or((0, 0));
        append(&format!(
            "{} summary: live big blocks {} (MiB each: {})  working set {ws} MiB  virtual {virt} MiB\n",
            stamp(),
            snapshot.len(),
            sizes.join(" ")
        ));
    });
}

unsafe impl GlobalAlloc for DiagAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if layout.size() >= BIG && !p.is_null() {
            note("alloc", p, layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(layout);
        if layout.size() >= BIG && !p.is_null() {
            note("alloc_zeroed", p, layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if layout.size() >= BIG {
            note("free", ptr, layout.size());
        }
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = System.realloc(ptr, layout, new_size);
        if p.is_null() {
            return p;
        }
        if layout.size() >= BIG {
            note("free", ptr, layout.size());
        }
        if new_size >= BIG {
            note("realloc", p, new_size);
        }
        p
    }
}
