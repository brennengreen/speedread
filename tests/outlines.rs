//! Golden outlines for every fixture language. Regenerate with
//! `UPDATE_SNAPSHOTS=1 cargo test --test outlines` and review the diff.

use std::fmt::Write;
use std::path::Path;

use speedread::outline::parse_outline;
use speedread::source::Source;

fn render(path: &Path) -> String {
    let src = Source::load(path).unwrap();
    let lang = src.lang.expect("fixture language detected");
    let o = parse_outline(lang, &src.data, &src.lines).expect("outline");
    let mut out = String::new();
    for (i, s) in o.symbols.iter().enumerate() {
        let collapse = s
            .collapse
            .map(|(a, b)| format!(" ⋯{}-{}", a + 1, b + 1))
            .unwrap_or_default();
        writeln!(
            out,
            "{}{:?} {} {}-{}{} | {}",
            "  ".repeat(s.depth as usize),
            s.kind,
            o.qualified_name(i),
            s.start + 1,
            s.end + 1,
            collapse,
            s.label
        )
        .unwrap();
    }
    out
}

#[test]
fn outlines_match_snapshots() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let update = std::env::var_os("UPDATE_SNAPSHOTS").is_some();
    let mut failures = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e != "outline"))
        .collect();
    entries.sort();
    assert!(entries.len() >= 20, "fixtures missing");
    for path in entries {
        let got = render(&path);
        let snap = path.with_extension(format!(
            "{}.outline",
            path.extension().unwrap().to_string_lossy()
        ));
        if update || !snap.exists() {
            std::fs::write(&snap, &got).unwrap();
            continue;
        }
        let want = std::fs::read_to_string(&snap).unwrap();
        if want != got {
            failures.push(format!(
                "{}:\n--- want\n{want}--- got\n{got}",
                path.display()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "outline snapshots differ:\n{}",
        failures.join("\n")
    );
}
