//! Provider metadata must never introduce an unrecognized effort value.
#![no_main]
use libfuzzer_sys::fuzz_target;
use snorkle_core::ai;
fuzz_target!(|data: &[u8]| {
    if let Ok(Some(support)) = ai::parse_effort_support(200, data) {
        assert!(support.levels.len() <= 5);
        for level in support.levels {
            assert!(["low", "medium", "high", "xhigh", "max"].contains(&level.as_str()));
        }
    }
});
