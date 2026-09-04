//! Extraction benchmark for the Rust pipeline alone.
//!
//! Usage:
//!
//! ```text
//! python benches/corpus.py /tmp/rustai-corpus
//! cargo run --release --example bench -- /tmp/rustai-corpus
//! ```
//!
//! Documents are streamed one at a time and their results dropped, which is how
//! a crawler actually runs. Holding a whole corpus in memory measures the corpus.

use std::time::Instant;

/// Peak resident set size for this process, in MiB.
///
/// `getrusage` is POSIX. There is a Windows equivalent in
/// `GetProcessMemoryInfo`, but this example exists to measure the footprint
/// claim on the platforms that claim is made about, and pulling in a second
/// FFI surface to print one number is not worth it. CI builds every target on
/// Windows too, so the function has to compile there regardless.
#[cfg(unix)]
fn peak_rss_mib() -> f64 {
    // SAFETY: `getrusage` only writes into the struct we hand it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return f64::NAN;
    }
    // macOS reports bytes; Linux reports kibibytes.
    let raw = usage.ru_maxrss as f64;
    if cfg!(target_os = "macos") { raw / (1024.0 * 1024.0) } else { raw / 1024.0 }
}

#[cfg(not(unix))]
fn peak_rss_mib() -> f64 {
    f64::NAN
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: bench <corpus-dir>  (create it with `python benches/corpus.py <dir>`)");
        std::process::exit(2);
    });

    let mut paths: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "html"))
        .collect();
    paths.sort();
    if paths.is_empty() {
        eprintln!("no .html files in {dir}");
        std::process::exit(2);
    }

    let rss_before = peak_rss_mib();
    let mut in_bytes = 0usize;
    let mut out_bytes = 0usize;
    let mut units = 0usize;
    let start = Instant::now();

    for path in &paths {
        let html = std::fs::read_to_string(path)?;
        in_bytes += html.len();
        let url = format!("https://example.com/{}", path.file_stem().unwrap().to_string_lossy());
        let article = rustai_core::parse::extract(&html, Some(&url))?;
        out_bytes += article.markdown.len();
        units += article.units.len();
        // `article` and `html` are dropped here, on purpose.
    }

    let elapsed = start.elapsed().as_secs_f64();
    let in_mb = in_bytes as f64 / 1e6;
    println!("documents      {}", paths.len());
    println!("input          {in_mb:.2} MB");
    println!("output         {:.2} MB in {units} units", out_bytes as f64 / 1e6);
    println!("compression    {:.1}%", 100.0 * (1.0 - out_bytes as f64 / in_bytes as f64));
    println!("elapsed        {elapsed:.3} s");
    println!(
        "throughput     {:.0} docs/s, {:.1} MB/s",
        paths.len() as f64 / elapsed,
        in_mb / elapsed
    );
    println!("peak RSS       {:.1} MiB (was {rss_before:.1} MiB before the run)", peak_rss_mib());
    Ok(())
}
