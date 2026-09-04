//! Where does extraction time actually go?
//! `cargo run --release --example profile -- <corpus-dir>`
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).expect("usage: profile <corpus-dir>");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "html"))
        .collect();
    paths.sort();
    let docs: Vec<String> = paths.iter().map(|p| std::fs::read_to_string(p).unwrap()).collect();
    let bytes: usize = docs.iter().map(String::len).sum();

    let time = |label: &str, f: &dyn Fn()| {
        // Three runs, keep the best: this is a ceiling measurement.
        let mut best = f64::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            f();
            best = best.min(t.elapsed().as_secs_f64());
        }
        println!(
            "  {label:28} {best:6.3} s   {:6.0} docs/s   {:5.1} MB/s",
            docs.len() as f64 / best,
            bytes as f64 / 1e6 / best
        );
        best
    };

    println!("{} documents, {:.2} MB", docs.len(), bytes as f64 / 1e6);
    let t_tl = time("tl::parse only", &|| {
        for d in &docs {
            let dom = tl::parse(d, tl::ParserOptions::default()).unwrap();
            std::hint::black_box(dom.nodes().len());
        }
    });
    let t_full = time("full extract", &|| {
        for d in &docs {
            std::hint::black_box(rustai_core::parse::extract(d, Some("https://x.dev/")).unwrap());
        }
    });
    use rustai_core::denoise::DenoiseConfig;
    use rustai_core::parse::{ExtractOptions, IndexMode, extract_with};

    let no_class = ExtractOptions {
        denoise: DenoiseConfig { drop_by_class: false, ..Default::default() },
        ..ExtractOptions::new()
    };
    let t_noclass = time("  without class regexes", &|| {
        for d in &docs {
            std::hint::black_box(extract_with(d, Some("https://x.dev/"), &no_class).unwrap());
        }
    });
    let no_index = ExtractOptions { index_mode: IndexMode::Never, ..ExtractOptions::new() };
    let t_noindex = time("  without index detection", &|| {
        for d in &docs {
            std::hint::black_box(extract_with(d, Some("https://x.dev/"), &no_index).unwrap());
        }
    });

    println!("\n  tl::parse            {:5.1}% of extraction", 100.0 * t_tl / t_full);
    println!("  class/id regexes     {:5.1}%", 100.0 * (t_full - t_noclass) / t_full);
    println!("  index detection      {:5.1}%", 100.0 * (t_full - t_noindex) / t_full);
    println!("  everything else      {:5.1}%", 100.0 * (t_noclass.min(t_noindex) - t_tl) / t_full);
    Ok(())
}
