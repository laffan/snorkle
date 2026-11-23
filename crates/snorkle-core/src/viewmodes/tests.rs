use super::*;

#[test]
fn defaults_and_exemptions() {
    let m = ViewModes::default();
    assert!(m.desaturate && m.hide_media && !m.dark);
    let exempt: BTreeSet<Kind> = [Kind::Desaturate].into();
    let t = m.for_tab(&exempt);
    assert!(!t.desaturate && t.hide_media);
    assert_eq!(injections(t).len(), 1);
}

#[test]
fn injections_per_mode() {
    let all = ViewModes { desaturate: true, hide_media: true, dark: false, night: false };
    let inj = injections(all);
    assert_eq!(inj.len(), 2);
    assert!(inj[0].source.contains("grayscale(1)"));
    assert!(inj[1].source.contains("video,"));
    assert!(injections(ViewModes { desaturate: false, hide_media: false, dark: false, night: false }).is_empty());
    let off = apply_js(ViewModes { desaturate: false, hide_media: false, dark: false, night: false });
    assert!(off.contains("window[\"__snorkle_grayscale_state\"];if(!s)return;s.obs.disconnect();s.st.remove()"));
    assert!(inj[1].all_frames, "hide media also covers iframes");
    assert!(!inj[0].all_frames);
    let with_dark = injections(ViewModes { dark: true, ..all });
    assert!(!inj.iter().chain(&with_dark).any(|i| i.source.contains("messageHandlers")), "nothing posts back");
    assert!(inj[1].source.contains("addEventListener('play'"));
    assert!(off.contains("obs.disconnect()"));
}

#[test]
fn dark_mode() {
    let m = ViewModes { desaturate: true, hide_media: true, dark: true, night: false };
    let inj = injections(m);
    assert_eq!(inj.len(), 4, "grayscale, media, dark style, dark detector");
    assert!(inj[2].source.contains("snorkle-dark") && inj[2].at == InjectAt::DocumentStart);
    assert!(inj[3].source.contains("classList.toggle('snorkle-dark'") && inj[3].at == InjectAt::DocumentEnd);
    assert!(DARK_CSS.contains(":has(#__snorkle_grayscale)") && DARK_CSS.contains(":not(:has(#__snorkle_no_images))"));
    assert!(DARK_CSS.contains("html:not(.snorkle-dark-checked){filter:brightness(.2)"), "dim while loading");
    assert!(DARK_DETECT_JS.contains("classList.add('snorkle-dark-checked')"), "the check ends the dim");
    assert!(!DARK_CSS.contains("picture"), "img inside picture would be filtered twice");
    let off = apply_js(ViewModes { dark: false, ..m });
    assert!(off.contains(DARK_STOP_JS));
    assert!(!ViewModes::default().dark);
    assert!(DARK_DETECT_JS.contains("prefers-color-scheme: dark"));
}

#[test]
fn night_vision() {
    assert!(!ViewModes::default().night && !LaunchModes::default().night, "off unless chosen");
    let m = ViewModes { desaturate: true, hide_media: true, dark: true, night: true };
    let inj = injections(m);
    assert_eq!(inj.len(), 5, "grayscale, media, dark style, dark detector, night");
    let night = &inj[4];
    assert!(night.source.contains("__snorkle_night") && night.at == InjectAt::DocumentStart && !night.all_frames);
    assert!(!night.source.contains("messageHandlers"), "nothing posts back");
    // Luminance to red: grey under a pure red multiply, in that order.
    assert!(NIGHT_CSS.contains("html::before{background:none!important;mix-blend-mode:normal!important;-webkit-backdrop-filter:grayscale(1)!important;backdrop-filter:grayscale(1)!important}"));
    assert!(
        NIGHT_CSS.contains("html::after{background:#f00!important;")
            && NIGHT_CSS.contains("mix-blend-mode:multiply!important}")
    );
    assert!(
        !NIGHT_CSS.contains("url("),
        "no SVG filters: WebKit's accelerated path and backdrop-filter refuse them"
    );
    // The root filters would apply over the layers: cleared, and dark
    // pages' inversion moves into the grey layer. Two `:has` outrank
    // dark mode's `.snorkle-dark:has(#__snorkle_grayscale)`.
    assert!(NIGHT_CSS.starts_with("html:has(#__snorkle_night):has(#__snorkle_night){filter:none!important}"));
    assert!(DARK_CSS.contains("html.snorkle-dark:has(#__snorkle_grayscale){filter:"), "the rule it must outrank");
    assert!(NIGHT_CSS.contains(
        "html.snorkle-dark::before{-webkit-backdrop-filter:invert(1) hue-rotate(180deg) grayscale(1)!important;"
    ));
    assert!(NIGHT_CSS
        .contains("html.snorkle-dark-checked.snorkle-dark-bg:not(.snorkle-dark){background-color:#000!important}"));
    assert!(
        NIGHT_CSS.contains(":is(:modal,:popover-open,:fullscreen):not(html)::after{"),
        "top layer gets its own red"
    );
    // A setting, never undone by an exemption the chrome doesn't offer,
    // and kept through Save for Later's plain preview.
    assert!(apply_js(m).contains("__snorkle_night"));
    let off = apply_js(ViewModes { night: false, ..m });
    assert!(off.contains("window[\"__snorkle_night_state\"];if(!s)return;s.obs.disconnect();s.st.remove()"));
    assert!(!plain_js().contains("__snorkle_night"));
    assert!(m.for_tab(&[Kind::Dark].into()).night);
    let mut l = LaunchModes::default();
    l.set(Kind::Night, true);
    assert!(l.modes().night && l.modes().get(Kind::Night));
}

#[test]
fn hidden_media_fades_in_and_is_revealed_under_overlays() {
    // Same filter functions in the same order, so the change animates.
    let hidden = "filter:grayscale(1) contrast(0) brightness(1.57)";
    let shown = "filter:grayscale(1) contrast(1) brightness(1)";
    assert!(NO_MEDIA_CSS.contains(hidden) && NO_MEDIA_CSS.contains(shown));
    assert!(NO_MEDIA_CSS.contains("transition:filter"));
    assert!(NO_MEDIA_CSS.contains(&format!("[{REVEAL_ATTR}]")));
    let on = style_js("__snorkle_no_images", NO_MEDIA_CSS, true, true);
    assert!(on.contains("elementsFromPoint") && on.contains("addEventListener('pointermove'"));
    assert!(on.contains("hasAttribute(\"data-snorkle-reveal\")"), "revealed video may play");
    let off = style_js("__snorkle_no_images", NO_MEDIA_CSS, false, true);
    assert!(off.contains("removeEventListener('pointermove'") && off.contains("removeAttribute("));
    assert!(!style_js("__snorkle_grayscale", GRAYSCALE_CSS, true, false).contains("pointermove"));
}

#[test]
fn hidden_media_accounts_for_loading() {
    // Lazy-loading placeholders at opacity 0 still show a gray box.
    assert!(
        NO_MEDIA_CSS.contains("img:not([data-snorkle-reveal]):not(:hover)")
            && NO_MEDIA_CSS.contains("{opacity:1!important}")
    );
    // A revealed image that has not loaded stays a pulsing box.
    assert!(NO_MEDIA_CSS.contains(&format!("[{LOADING_ATTR}][{LOADING_ATTR}][{LOADING_ATTR}]")));
    assert!(
        NO_MEDIA_CSS.contains("@property --snorkle-pulse")
            && NO_MEDIA_CSS.contains("brightness(var(--snorkle-pulse,1.57))")
    );
    let on = style_js("__snorkle_no_images", NO_MEDIA_CSS, true, true);
    assert!(on.contains("!e.complete") && on.contains("e.loading='eager'"));
    assert!(
        on.contains("addEventListener('load',s.onLoad,true)")
            && on.contains("addEventListener('error',s.onLoad,true)")
    );
    let off = style_js("__snorkle_no_images", NO_MEDIA_CSS, false, true);
    assert!(
        off.contains("removeEventListener('load',s.onLoad,true)")
            && off.contains("removeAttribute(\"data-snorkle-loading\")")
    );
}

#[test]
fn dark_mode_lets_sites_show_their_own_dark_theme() {
    // The first check waits for the dark appearance to reach the page.
    assert!(DARK_DETECT_JS.contains("function first(){if(!mq||mq.matches"));
    assert!(DARK_DETECT_JS.contains("setTimeout(go,600)"), "bounded wait");
    // Theme changes made later are checked again.
    assert!(DARK_DETECT_JS.contains("mq.addEventListener('change',onScheme)"));
    assert!(DARK_DETECT_JS.contains("attributeFilter:['class','style','data-theme'"));
    assert!(DARK_DETECT_JS.contains("document.addEventListener('transitionend',onTr,true)"));
    // Its own classes do not trigger it (no loop).
    assert!(DARK_DETECT_JS.contains("c.indexOf('snorkle-')!==0"));
    // `<meta name="color-scheme">` counts as opting in.
    assert!(DARK_DETECT_JS.contains("meta[name='color-scheme' i]"));
    // Everything it starts, stop ends.
    for off in [
        "aobs.disconnect()",
        "removeEventListener('transitionend',onTr,true)",
        "removeEventListener('change',onScheme)",
    ] {
        assert!(DARK_DETECT_JS.contains(off), "{off}");
    }
}

#[test]
fn dark_mode_counter_inverts_video_frames() {
    assert!(DARK_DETECT_JS.contains(&format!("BOX='{MEDIA_BOX_ATTR}'")));
    assert!(DARK_DETECT_JS.contains("shadowRoot") && DARK_DETECT_JS.contains("getRootNode()===document"));
    assert!(
        DARK_CSS.contains(&format!("input[type=image],[{MEDIA_BOX_ATTR}],nyt-betamax,")),
        "frames get the media filters"
    );
    // Media inside a frame get no filter of their own, over every
    // other dark rule (two ids in the selector).
    let none = format!("html:has(#__snorkle_dark):has(#__snorkle_dark) [{MEDIA_BOX_ATTR}] :is(");
    let at = DARK_CSS.find(&none).expect("inner media rule");
    assert!(DARK_CSS[at..].starts_with(&none) && DARK_CSS.ends_with("{filter:none!important}"));
    assert!(DARK_STOP_JS.contains("__snorkleDark.stop()"));
    assert!(DARK_DETECT_JS.contains("removeAttribute(BOX)"));
}

#[test]
fn dark_mode_keeps_dark_sections_dark() {
    assert!(DARK_DETECT_JS.contains(&format!("var FLIP='{FLIP_ATTR}'")));
    assert!(DARK_DETECT_JS.contains(&format!("n<{MAX_FLIP_SCAN}")), "bounded scan");
    assert!(DARK_DETECT_JS.contains("depth<2"), "at most two levels of flips");
    assert!(DARK_DETECT_JS.contains("position==='fixed'"), "sections holding fixed elements are not filtered");
    // Marked before the root is, so no media filter transitions from the
    // inverted state.
    assert!(DARK_DETECT_JS.contains("flips(l>0.5);d.classList.toggle('snorkle-dark'"));
    assert!(DARK_DETECT_JS.contains("removeAttribute(FLIP)"));
    assert!(DARK_CSS
        .contains(&format!("html.snorkle-dark [{FLIP_ATTR}]{{filter:invert(1) hue-rotate(180deg)!important}}")));
    let one = format!(":not([{FLIP_ATTR}] [{FLIP_ATTR}] *)");
    assert_eq!(DARK_CSS.matches(&one).count(), 5, "media under one flip get the dark-page filters");
    // The frame rule (two ids) still wins over them: it stays last.
    assert!(DARK_CSS.ends_with("{filter:none!important}"));
}

#[test]
fn plain_view_only_takes_modes_off_the_loaded_page() {
    let js = plain_js();
    for id in ["__snorkle_grayscale_state", "__snorkle_no_images_state", "__snorkle_dark_state"] {
        assert!(js.contains(&format!("var s=window[\"{id}\"];if(!s)return;")), "{id} removed");
    }
    assert!(js.contains(DARK_STOP_JS));
    assert!(!js.contains("createElement('style')"), "adds no style");
}

#[test]
fn hidden_video_is_grayed_on_its_frame() {
    assert!(
        NO_MEDIA_CSS.contains(&format!(":root:not(.snorkle-dark):has(#__snorkle_no_images) [{VIDEO_BOX_ATTR}]{{"))
    );
    assert!(NO_MEDIA_CSS.contains(&format!("[{VIDEO_BOX_ATTR}]:is(:hover,[{REVEAL_ATTR}],:has([{REVEAL_ATTR}]))")));
    assert!(
        NO_MEDIA_CSS.contains(":has(#__snorkle_no_images):has(#__snorkle_no_images)"),
        "inner media lose their own filter"
    );
    assert!(VIDEO_BOX_JS.contains(&format!("var VB='{VIDEO_BOX_ATTR}'")));
    let on = style_js("__snorkle_no_images", NO_MEDIA_CSS, true, true);
    assert!(on.contains("s.boxes=function") && on.contains("if(s.later)s.later();"));
    assert!(
        !style_js("__snorkle_grayscale", GRAYSCALE_CSS, true, false).contains("s.boxes"),
        "only hide media marks frames"
    );
    let off = style_js("__snorkle_no_images", NO_MEDIA_CSS, false, true);
    assert!(off.contains(&format!("removeAttribute('{VIDEO_BOX_ATTR}')")), "marks removed when turned off");
    // Web-component players (nyt-betamax) are media themselves, and
    // media in nested open shadow roots are found and paused there.
    assert!(MEDIA_SELECTOR.contains("nyt-betamax,") && NO_MEDIA_CSS.contains("nyt-betamax,"));
    assert!(
        VIDEO_BOX_JS.contains("if(depth<4)look(")
            && VIDEO_BOX_JS.contains("r.addEventListener('play',s.onPlay,true)")
    );
    assert!(off.contains("r.removeEventListener('play',s.onPlay,true)"));
    // Shadow roots (NYT's video feed) get the same rules as an adopted
    // sheet, without the document-only switch.
    let shadow = shadow_no_media_css();
    assert!(!shadow.contains(":root") && !shadow.contains("#__snorkle_no_images"));
    assert!(shadow.contains(&format!("[{VIDEO_BOX_ATTR}]{{")) && shadow.contains("nyt-betamax,"));
    assert!(on.contains("adoptedStyleSheets=r.adoptedStyleSheets.concat([s.sheet])"));
    assert!(off.contains("adoptedStyleSheets=r.adoptedStyleSheets.filter("), "sheet removed when turned off");
    assert!(
        VIDEO_BOX_JS.contains("if(q.width===0&&q.height===0){e=p;continue;}"),
        "display:contents wrappers are skipped"
    );
}
