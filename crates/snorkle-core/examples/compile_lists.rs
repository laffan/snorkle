//! Converts filter lists to WebKit content-blocking JSON and prints stats.
//! `cargo run --release -p snorkle-core --example compile_lists -- <list.txt>... [--out DIR]`
use std::collections::BTreeSet;
use std::time::Instant;

use snorkle_core::blocking::{cache_key, compile, FilterList};

fn main() {
    let mut paths = Vec::new();
    let mut out = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--out" {
            out = args.next();
        } else {
            paths.push(a);
        }
    }
    let texts: Vec<(String, String)> =
        paths.iter().map(|p| (p.clone(), std::fs::read_to_string(p).expect("read list"))).collect();
    let lists: Vec<FilterList<'_>> = texts.iter().map(|(n, t)| FilterList { name: n, text: t }).collect();
    let t = Instant::now();
    let c = compile(&lists, &BTreeSet::new()).expect("compile");
    println!(
        "key={} lists={} rules={} exceptions={} filters_used={} bytes={} time={:?}",
        cache_key(&lists, &BTreeSet::new()),
        c.lists.len(),
        c.rules,
        c.exceptions,
        c.filters_used,
        c.lists.iter().map(|l| l.len()).sum::<usize>(),
        t.elapsed()
    );
    if let Some(dir) = out {
        for (i, l) in c.lists.iter().enumerate() {
            std::fs::write(format!("{dir}/rules-{i}.json"), l).expect("write");
        }
    }
}
