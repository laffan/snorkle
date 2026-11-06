//! Provider metadata and outline summaries stay within their validated bounds.
#![no_main]
use libfuzzer_sys::fuzz_target;
use snorkle_core::ai;
fuzz_target!(|data: &[u8]| {
    if let Ok(snippets) = ai::parse_heading_summaries(ai::Provider::Anthropic, 200, data) {
        assert!(snippets.len() <= snorkle_core::viewmodes::MAX_SAMPLE);
        for snippet in snippets {
            assert!(snippet.summary.split_whitespace().count() <= 20);
            assert!(snippet.heading.chars().count() <= 201);
        }
    }
    if let Ok(Some(support)) = ai::parse_effort_support(200, data) {
        assert!(support.levels.len() <= 5);
        for level in support.levels {
            assert!(["low", "medium", "high", "xhigh", "max"].contains(&level.as_str()));
        }
    }
});
