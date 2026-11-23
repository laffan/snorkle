use super::*;

fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

#[test]
fn sites() {
    assert_eq!(site_of(&u("https://chatgpt.com/c/123")), Some(Site::ChatGpt));
    assert_eq!(site_of(&u("https://chat.openai.com/c/1")), Some(Site::ChatGpt));
    assert_eq!(site_of(&u("https://claude.ai/chat/abc")), Some(Site::Claude));
    assert_eq!(site_of(&u("https://gemini.google.com/app/x")), Some(Site::Gemini));
    for no in
        ["https://notchatgpt.com/", "https://claude.ai.example.com/", "https://google.com/", "http://claude.ai/"]
    {
        assert_eq!(site_of(&u(no)), None, "{no}");
    }
}

#[test]
fn rebuilds_only_safe_markup() {
    let answer = serde_json::json!({ "turns": [
        { "r": "user", "c": ["div", {}, "Hi <script>alert(1)</script> & \u{202e}", ["img", { "src": "data:image/png;base64,iVBORw0K", "alt": "a\"b" }]] },
        { "r": "assistant", "c": ["", {}, ["p", { "style": "x", "onclick": "y" }, "Answer"],
            ["script", {}, "alert(1)"],
            ["a", { "href": "javascript:alert(1)" }, "bad"],
            ["a", { "href": "https://example.com/a?b=1&c=\"" }, "good"],
            ["img", { "src": "https://cdn.example.com/i.png" }],
            ["img", { "src": "data:text/html;base64,PHNjcmlwdD4=" }],
            ["pre", { "lang": "rust\"><x" }, "fn main() {}"],
            ["pre", { "lang": "python" }, "print('<b>')"],
            ["td", { "colspan": "999" }, "x"],
            ["math", { "display": "block", "onload": "x" }, ["mi", { "mathvariant": "bold\"" }, "x"]],
            ["iframe", { "src": "https://evil.example" }, "frame text"],
        ]},
        { "r": "system", "c": ["p", {}, "ignored"] },
    ]});
    let c = parse(&answer.to_string()).unwrap();
    assert_eq!(c.turns.len(), 2);
    assert!(c.turns[0].user && !c.turns[1].user);
    let a = &c.turns[0].html;
    assert_eq!(
        a,
        "<div>Hi &lt;script&gt;alert(1)&lt;/script&gt; &amp; \u{202e}<img alt=\"a&quot;b\" src=\"data:image/png;base64,iVBORw0K\"></div>"
    );
    let b = &c.turns[1].html;
    for bad in
        ["<script", "onclick", "style=", "javascript:", "text/html", "onload", "<iframe", "bold&quot;", "colspan"]
    {
        assert!(!b.contains(bad), "{bad} in {b}");
    }
    assert!(b.contains("<p>Answer</p>") && b.contains("alert(1)"), "unknown tags keep their text: {b}");
    assert!(b.contains("<a href=\"https://example.com/a?b=1&amp;c=%22\" rel=\"noreferrer\">good</a>"), "{b}");
    assert!(b.contains("<a rel=\"noreferrer\">bad</a>"));
    assert!(b.contains("<pre><code>fn main() {}</code></pre>"), "bad language dropped: {b}");
    assert!(b.contains("<pre data-lang=\"python\"><code>print(&#39;&lt;b&gt;&#39;)</code></pre>"));
    assert!(b.contains("<math display=\"block\"><mi>x</mi></math>"));
    assert!(b.contains("frame text"));
    assert_eq!(c.images, vec![u("https://cdn.example.com/i.png")]);
}

#[test]
fn bounded_and_fails_closed() {
    assert!(parse("nope").is_none());
    assert!(parse(r#"{"turns":[]}"#).is_none());
    assert!(parse(r#"{"turns":[{"r":"user","c":["p",{}]}]}"#).is_none(), "empty turns dropped");
    // Deep nesting stops at the bound without failing the whole thread
    // (and serde_json refuses anything past 128 levels).
    let mut deep = serde_json::json!("deep");
    for _ in 0..60 {
        deep = serde_json::json!(["div", {}, deep]);
    }
    let t = serde_json::json!({ "turns": [ { "r": "user", "c": ["p", {}, "first"] }, { "r": "assistant", "c": deep } ] });
    let c = parse(&t.to_string()).unwrap();
    assert_eq!(c.turns.len(), 1, "the turn that passed the bound is left out, and what follows it");
    // Data images: bounded each and in all.
    let big = format!("data:image/png;base64,{}", "A".repeat(MAX_DATA_IMAGE));
    let t = serde_json::json!({ "turns": [ { "r": "user", "c": ["p", {}, "x", ["img", { "src": big }]] } ] });
    let c = parse(&t.to_string()).unwrap();
    assert!(!c.turns[0].html.contains("data:"));
    assert!(parse(&"x".repeat(MAX_END_BYTES + 1)).is_none());
    assert_eq!(parse_steps(r#"{"steps":12}"#), Some(12));
    assert_eq!(parse_steps(r#"{"steps":-1}"#), None);
    assert_eq!(parse_step(r#"{"done":false,"at":50,"of":200}"#), Some((false, 25)));
    assert_eq!(parse_step(r#"{"done":true,"at":500,"of":200}"#), Some((true, 100)));
    assert_eq!(parse_step(r#"{"done":true,"at":1,"of":0}"#), None);
}

#[test]
fn page() {
    let c = Conversation {
        turns: vec![
            Turn { user: true, html: "<p>Q</p>".into() },
            Turn { user: false, html: "<p>A1</p>".into() },
            Turn { user: false, html: "<p>A2</p>".into() },
        ],
        images: vec![],
    };
    let html =
        String::from_utf8(to_html(&c, Site::Claude, &u("https://claude.ai/chat/1"), "My <chat>", 0)).unwrap();
    assert!(html.contains("<title>My &lt;chat&gt;</title>"));
    assert!(html.contains("Claude conversation · saved 1970-01-01"));
    assert_eq!(html.matches("<section").count(), 2, "consecutive turns of one side share a section");
    assert!(!html.contains("<script"));
    assert_eq!(day(1_763_164_800_000), "2025-11-15");
}

#[test]
fn capture_mode() {
    let begin = capture_begin_js();
    assert!(begin.contains("st.live") || begin.contains("live: true"));
    assert!(begin.contains("new MutationObserver(soon)") && begin.contains("setTimeout(take, 300)"));
    assert!(CAPTURE_CANCEL_JS.contains("st.stop()") && END_JS.contains("if (st.stop) st.stop();"));
    // The 2026-10 ChatGPT markup first, then the earlier forms.
    let unit = begin.find("[data-content-search-unit-key]").unwrap();
    assert!(
        unit < begin.find("[data-message-author-role]").unwrap() && unit < begin.find("[data-turn-key]").unwrap()
    );
    assert!(begin.contains("[data-testid=\"assistant-message\"], .font-claude-response"));
    assert_eq!(parse_count(r#"{"count":12}"#), Some(12));
    assert_eq!(parse_count(r#"{"count":-1}"#), None);
    assert_eq!(parse_count("nope"), None);
}

#[test]
fn scripts_are_constant_and_bounded() {
    let start = start_js();
    assert!(start.contains("window.__snorkleConv = st") && start.contains("Date.now() + 20000"));
    assert!(STEP_JS.contains("Date.now() - t0 < 6000") && STEP_JS.contains("75000"));
    assert!(END_JS.contains("delete window.__snorkleConv") && END_JS.contains("st.back()"));
    for js in [start.as_str(), STEP_JS, END_JS] {
        assert!(!js.contains("messageHandlers"), "answers go back only as the call's result");
    }
}
