//! Debug helper: replay a captured PTY byte stream (with `\n@@<ts>@@\n` chunk
//! markers) into a VtTerm up to an optional cutoff timestamp, then dump the
//! rendered visible grid as text. Used to see what's actually on Claude's screen
//! before/after dismissing a menu.
//!
//!   cargo run --example dumpgrid -- /tmp/capask.bin [cutoff_seconds]

use arbiter_native::term::VtTerm;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = &args[1];
    let cutoff: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(f32::MAX);
    let raw = std::fs::read(path).unwrap();
    let s = String::from_utf8_lossy(&raw).into_owned();

    let mut term = VtTerm::new(120, 44);
    // Segments are "\n@@<ts>@@\n<data>". Walk them, feeding data while ts <= cutoff.
    let mut rest = s.as_str();
    while let Some(start) = rest.find("\n@@") {
        let after = &rest[start + 3..];
        let Some(end) = after.find("@@\n") else { break };
        let ts: f32 = after[..end].parse().unwrap_or(0.0);
        let data_start = start + 3 + end + 3;
        let next = rest[data_start..].find("\n@@").map(|i| data_start + i).unwrap_or(rest.len());
        if ts <= cutoff {
            term.feed(rest[data_start..next].as_bytes());
        }
        rest = &rest[next..];
    }

    let (cols, rows) = term.size();
    let mut grid = vec![vec![' '; cols]; rows];
    term.for_each_cell(|r, c, ch, _, _, _, _| {
        if r < rows && c < cols {
            grid[r][c] = if ch == '\0' { ' ' } else { ch };
        }
    });
    for (i, row) in grid.into_iter().enumerate() {
        let line: String = row.into_iter().collect();
        let t = line.trim_end();
        if !t.is_empty() {
            println!("{i:2}| {t}");
        }
    }
}
