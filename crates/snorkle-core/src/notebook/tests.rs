use super::*;

#[test]
fn reports_are_strict() {
    let r = SelectionRect { x: 10.0, y: 20.0, w: 300.0, h: 18.0, vw: 1024.0 };
    assert_eq!(parse_report(r#"{"nb":"text","r":[10,20,300,18],"vw":1024}"#), Some(Report::Text(r)));
    assert_eq!(parse_report(r#"{"nb":"img","r":[10,20,300,18],"vw":1024}"#), Some(Report::Image(r)));
    assert_eq!(parse_report(r#"{"nb":"clip","r":[10,20,300,18],"vw":1024}"#), Some(Report::Clip(r)));
    assert_eq!(parse_report(r#"{"nb":"end"}"#), Some(Report::End));
    assert_eq!(parse_report(r#"{"nb":"hide"}"#), Some(Report::Hide));
    assert_eq!(parse_report(r#"{"nb":"dend"}"#), Some(Report::DragEnd));
    assert_eq!(
        parse_report(r#"{"nb":"link","url":"https://e.com/a b","r":[10,20,300,18],"vw":1024}"#),
        Some(Report::Link("https://e.com/a%20b".into(), r))
    );
    for bad in [
        r#"{"nb":"link","url":"javascript:alert(1)","r":[10,20,300,18],"vw":1024}"#,
        r#"{"nb":"link","url":"https://u:p@e.com/","r":[10,20,300,18],"vw":1024}"#,
        r#"{"nb":"link","r":[10,20,300,18],"vw":1024}"#,
    ] {
        assert!(parse_report(bad).is_none(), "{bad}");
    }
    assert_eq!(parse_report(r#"{"nb":"dtext","r":[10,20,300,18],"vw":1024}"#), Some(Report::DragText(r)));
    assert_eq!(parse_report(r#"{"nb":"dimg","r":[10,20,300,18],"vw":1024}"#), Some(Report::DragImage(r)));
    for bad in [
        r#"{"nb":"other","r":[1,2,3,4],"vw":9}"#,
        r#"{"nb":"clip","r":[1,2,0,4],"vw":9}"#,
        r#"{"nb":"clip","r":[1,2,3],"vw":9}"#,
        r#"{"nb":"clip","r":[1e9,2,3,4],"vw":9}"#,
        r#"{"nb":"clip","r":[1,2,3,4],"vw":0}"#,
        r#"{"nb":"clip","r":["1",2,3,4],"vw":9}"#,
        r#"{"img":"x"}"#,
    ] {
        assert!(parse_report(bad).is_none(), "{bad}");
    }
    assert!(parse_report(&format!(r#"{{"nb":"clip","r":[1,2,3,4],"vw":9,"x":"{}"}}"#, "a".repeat(MAX_REPORT)))
        .is_none());
    assert!(is_report(r#"{"nb":"end"}"#) && !is_report(r#"{"img":null}"#) && !is_report(r#"{"edit":1}"#));
}

#[test]
fn page_code_is_constant() {
    let w = watch_js();
    assert!(w.contains(&format!("messageHandlers.{HANDLER_NAME}")) && w.contains("window.__snorkleNb"));
    assert_eq!(clip_mode_js(true), "window.__snorkleNb&&window.__snorkleNb.setClip(true);");
    assert_eq!(
        hover_js(0.5, f64::NAN, 2.0),
        "window.__snorkleNb&&window.__snorkleNb.hover(0.50000,0.00000,1.00000);"
    );
    assert!(drop_js(-3.0, 0.25).contains("n.drop(0.00000,0.25000)"));
    assert_eq!(parse_drop("{\"t\":\"  A sentence.\\u202e \"}").as_deref(), Some("A sentence."));
    assert_eq!(parse_drop(r#"{"t":""}"#), None);
    assert_eq!(parse_drop("[]"), None);
    // The quote button and ⌥ over the page's selection use the selection.
    assert!(w.contains("function quoteAt(x,y){var s=inSelection(x,y);return s?s.cloneRange():sentenceAt(x,y);}"));
    assert!(w.contains("return quoteAt(p[0],p[1]);") && !w.contains("return sentenceAt(p[0],p[1]);"));
    assert!(w.contains("selPress") && w.contains("noSelect(!!stage||!!drag)"));
    // ⌥ with ⇧ selects by word as usual; holding ⌥ alone does not dim
    // the page (only clip mode's cover is grey; a drag's stage shades).
    assert!(w.contains("function optKey(e){return !!(e.altKey&&!e.shiftKey&&!e.metaKey&&!e.ctrlKey);}"));
    assert!(!w.contains("e.altKey&&!e.metaKey") && !w.contains("||e.altKey||"));
    assert!(w.contains("if(e.key==='Shift'&&alt){setAlt(false);return;}"));
    assert!(w.contains("background:'transparent',cursor:'crosshair'"));
    assert!(w.contains("at(ov,'background',nb.clip?'rgba(128,128,128,.14)':'transparent')"));
    assert_eq!(
        parse_drop(&format!(r#"{{"t":"{}"}}"#, "a".repeat(MAX_QUOTE_CHARS))).map(|t| t.len()),
        Some(MAX_QUOTE_CHARS)
    );
}

#[test]
fn titles_and_region_text() {
    assert_eq!(
        page_title(b"<html><head><TITLE data-x=1>Caf&eacute; &amp; Tea &#8212; Home\n</title>").as_deref(),
        Some("Caf&eacute; & Tea \u{2014} Home")
    );
    assert_eq!(
        page_title(b"<head><meta property='og:title' content='Only &quot;OG&quot;'></head>").as_deref(),
        Some("Only \"OG\"")
    );
    assert_eq!(page_title(b"<title>  </title>"), None);
    assert_eq!(page_title(b"no title here"), None);
    assert_eq!(page_title("<title>\x07bell &#7; \u{202e}rtl</title>".as_bytes()).as_deref(), Some("bell rtl"));
    assert_eq!(decode_entities("a &bogus; b &#x41; &"), "a &bogus; b A &");
    assert!(region_text_js(1.0, 2.0, f64::NAN, -5.0).contains("regionText(1.0,2.0,0.0,0.0)"));
    let text = |s: &str| parse_region(s).map(|r| r.text);
    assert_eq!(text(r#"{"t":" Line one\n\n\n\nLine two "}"#).as_deref(), Some("Line one\n\nLine two"));
    assert_eq!(text(r#"{"t":""}"#).as_deref(), Some(""));
    assert_eq!(parse_region(r#"{"t":"x"}"#), Some(Region { text: "x".into(), links: vec![], at: None }));
    assert!(parse_region(&format!(r#"{{"t":"{}"}}"#, "a".repeat(MAX_REGION_BYTES))).is_none());
    assert!(parse_region(r#"{"l":[]}"#).is_none() && parse_region("[]").is_none());
}

#[test]
fn clip_links_and_box() {
    // 2026-11-25: the links laid out in a clip's region, checked; its box.
    let r = parse_region(
        r#"{"t":"Hi","l":[["https://a.example/x","Read \u202emore\n now"],["javascript:alert(1)","x"],
        ["https://user:pw@b.example/","y"],["https://a.example/x","again"],["http://c.example","" ],[1,2],"https://d.example/"],
        "at":[10.4,-3,500,1e9]}"#,
    )
    .unwrap();
    assert_eq!(
        r.links,
        vec![
            ClipLink { url: "https://a.example/x".into(), title: "Read more now".into() },
            ClipLink { url: "http://c.example/".into(), title: String::new() },
        ]
    );
    assert_eq!(r.at, Some([10.0, 0.0, 500.0, 100_000.0]));
    let many: Vec<Value> = (0..40).map(|i| serde_json::json!([format!("https://e.example/{i}"), "t"])).collect();
    let r = parse_region(&serde_json::json!({ "t": "", "l": many, "at": [1, 2, 3] }).to_string()).unwrap();
    assert_eq!(r.links.len(), MAX_CLIP_LINKS);
    assert_eq!(r.at, None, "four numbers or none");
    assert_eq!(parse_region(r#"{"t":"","at":[1,2,"3",4]}"#).unwrap().at, None);
    // Going back: constant code with four clamped numbers.
    assert_eq!(back_js(12.6, f64::NAN, -4.0, 2e6), "window.__snorkleNb&&window.__snorkleNb.back(13,0,0,100000);");
    let w = watch_js();
    assert!(w.contains("nb.back=function(x,y,wd,ht){"));
    assert!(w.contains("u.split('#')[0]===here"), "links within the page itself are left out");
}

#[test]
fn notebook_is_rebuilt() {
    let jpeg = "data:image/jpeg;base64,/9j/4AAQ";
    let input = serde_json::json!({
        "strokes": [
            { "id": "s-1", "points": [1, 2, 3.5, 4], "width": 2, "color": "", "note": "note-1" },
            { "id": "s-2", "points": [1, 2, 3], "width": 2, "color": "", "note": "note-1" },
            { "id": "s 3", "points": [1, 2], "width": 2, "color": "", "note": "note-1" },
            { "id": "s-4", "points": [1, 2], "width": 2, "color": "red;x", "note": "note-1" },
            { "id": "s-5", "points": [1, 2], "width": 2, "color": "#FF5F57", "note": "note-1", "evil": 1 },
        ],
        "notes": [
            { "id": "note-1", "createdMs": 5, "url": "javascript:alert(1)", "title": "T\u{202e}itle" },
            { "id": "note-2", "createdMs": 6, "url": "https://user:pw@example.com/", "title": "" },
            { "id": "note-3", "createdMs": 7, "url": "https://example.com/a#:~:text=x" },
        ],
        "links": [
            { "id": "l-1", "x": 1, "y": 2, "target": { "kind": "quote", "url": "https://e.com/", "text": "Quoted", "title": "E" } },
            { "id": "l-2", "x": 1, "y": 2, "image": jpeg, "imageDark": "data:image/svg+xml;base64,PHN2Zz4=", "target": { "kind": "clip", "url": "https://e.com/" } },
            { "id": "l-3", "x": 1, "y": 2, "image": "data:text/html;base64,PGI+", "target": { "kind": "clip" } },
            { "id": "l-4", "x": 1, "y": 2, "text": "# Note\u{0}\nline", "fixed": true, "target": { "kind": "page" } },
            { "id": "l-5", "x": 1, "y": 2, "target": { "kind": "script" } },
            { "id": "l-6", "x": f64::MAX, "y": 2, "target": { "kind": "page" } },
            { "id": "l-7", "x": 1, "y": 2, "target": { "kind": "quote", "text": "  " } },
            { "id": "l-8", "x": 1, "y": 2, "image": jpeg, "target": { "kind": "clip", "url": "https://e.com/",
              "links": [{ "url": "https://f.example/a", "title": "F\u{202e}" }, { "url": "data:text/html,x", "title": "no" }, ["https://g.example/", "G"]],
              "at": [1, 2, 3, 4] } },
            { "id": "l-9", "x": 1, "y": 2, "target": { "kind": "quote", "url": "https://e.com/", "text": "Q",
              "links": [{ "url": "https://f.example/a" }], "at": [1, 2, 3, 4] } },
        ],
        "pan": { "x": 10, "y": "no" },
        "extra": "dropped",
    });
    let out: Value = serde_json::from_str(&clean(&input.to_string()).unwrap()).unwrap();
    let strokes = out["strokes"].as_array().unwrap();
    assert_eq!(strokes.len(), 2, "odd points, bad ids and colours are left out");
    assert_eq!(strokes[1]["color"], "#ff5f57");
    assert!(strokes[1].get("evil").is_none());
    let notes = out["notes"].as_array().unwrap();
    assert_eq!(notes[0]["title"], "Title");
    assert!(notes[0].get("url").is_none() && notes[1].get("url").is_none());
    assert_eq!(notes[2]["url"], "https://example.com/a#:~:text=x");
    let links = out["links"].as_array().unwrap();
    let ids: Vec<&str> = links.iter().map(|l| l["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["l-1", "l-2", "l-4", "l-8", "l-9"]);
    assert_eq!(links[1]["image"], jpeg);
    assert!(links[1].get("imageDark").is_none(), "only jpeg and png");
    assert_eq!(links[2]["text"], "# Note\nline");
    assert_eq!(links[2]["fixed"], true);
    // A clip's links (http(s) only, either form) and box are kept; nothing
    // else has them (2026-11-25).
    assert_eq!(
        links[3]["target"]["links"],
        serde_json::json!([{ "url": "https://f.example/a", "title": "F" }, { "url": "https://g.example/", "title": "G" }])
    );
    assert_eq!(links[3]["target"]["at"], serde_json::json!([1.0, 2.0, 3.0, 4.0]));
    assert!(links[1]["target"].get("links").is_none() && links[1]["target"].get("at").is_none());
    assert!(links[4]["target"].get("links").is_none() && links[4]["target"].get("at").is_none());
    assert_eq!(out["pan"], serde_json::json!({ "x": 10.0, "y": 0.0 }));
    assert!(out.get("extra").is_none());
    assert!(clean("[]").is_none() && clean("nope").is_none());
    assert_eq!(
        clean("{}").as_deref(),
        Some(r#"{"links":[],"notes":[],"pan":{"x":0.0,"y":0.0},"strokes":[],"version":1}"#)
    );
}
