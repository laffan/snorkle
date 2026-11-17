//! Provider metadata, page blocks and page summaries stay within their validated bounds.
#![no_main]
use libfuzzer_sys::fuzz_target;
use snorkle_core::{ai, summary};
fuzz_target!(|data: &[u8]| {
    if let Ok(points) = ai::parse_summary(ai::Provider::Anthropic, 200, data, 50) {
        assert!(!points.is_empty() && points.len() <= summary::MAX_POINTS);
        for p in points {
            assert!(!p.text.is_empty() && p.text.chars().count() <= summary::MAX_POINT_CHARS + 1);
            assert!(p.blocks.len() <= summary::MAX_POINT_BLOCKS && p.blocks.iter().all(|b| *b < 50));
        }
    }
    if let Some(page) = std::str::from_utf8(data).ok().and_then(summary::parse_blocks) {
        assert!(page.blocks.len() <= summary::MAX_BLOCKS);
        assert!(page.blocks.iter().all(|b| b.chars().count() <= summary::MAX_BLOCK_CHARS + 1));
    }
    if let Ok(Some(support)) = ai::parse_effort_support(200, data) {
        assert!(support.levels.len() <= 5);
        for level in support.levels {
            assert!(["low", "medium", "high", "xhigh", "max"].contains(&level.as_str()));
        }
    }
});
