//! Prints every single-word link in Scana's texts with a little context, for review:
//! `cargo run -p scana-db --example link_audit`.
use scana_db::library::find_links;
use std::collections::BTreeMap;

/// Every sentence-like string in Scana's data files (ids and short names skipped).
fn texts() -> Vec<String> {
    fn walk(v: &toml::Value, out: &mut Vec<String>) {
        match v {
            toml::Value::String(s) if s.len() > 24 && s.contains(' ') => out.push(s.clone()),
            toml::Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            toml::Value::Table(t) => {
                for (k, x) in t {
                    if k != "sources" && k != "source" && k != "aliases" {
                        walk(x, out);
                    }
                }
            }
            _ => {}
        }
    }
    let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
    let mut out = Vec::new();
    let mut stack = vec![data];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "toml")
                && let Ok(t) = toml::from_str::<toml::Table>(&std::fs::read_to_string(&p).unwrap_or_default())
            {
                walk(&toml::Value::Table(t), &mut out);
            }
        }
    }
    out
}

fn main() {
    let mut by_word: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for text in texts() {
        for (r, _) in find_links(&text, &[]) {
            let word = &text[r.clone()];
            if word.contains(' ') || word.chars().filter(|c| c.is_uppercase()).count() >= 2 {
                continue; // phrases and acronyms are rarely wrong
            }
            let a = text[..r.start].char_indices().rev().nth(30).map_or(0, |(i, _)| i);
            let b = text[r.end..].char_indices().nth(30).map_or(text.len(), |(i, _)| r.end + i);
            by_word.entry(word.to_lowercase()).or_default().push(format!(
                "...{}[{}]{}...",
                &text[a..r.start],
                word,
                &text[r.end..b]
            ));
        }
    }
    let only: Vec<String> = std::env::args().skip(1).map(|a| a.to_lowercase()).collect();
    for (w, ctx) in &by_word {
        if !only.is_empty() && !only.contains(w) {
            continue;
        }
        println!("== {w} ({})", ctx.len());
        let shown = if only.is_empty() { 4 } else { usize::MAX };
        for c in ctx.iter().take(shown) {
            println!("   {}", c.replace('\n', " "));
        }
    }
}
