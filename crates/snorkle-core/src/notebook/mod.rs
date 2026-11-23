//! SECURITY-REVIEW: the Notebook (owner request 2026-11-15): one notebook,
//! not tied to a page or a tab, in the right panel — the notepad of
//! laffan/zotero-helper (ink, typed notes, quotes and clips), with
//! captures from any page.
//!
//! From the page (channel C23, posting to C19's client-world handler, so
//! page scripts cannot call it): a constant watcher ([`watch_js`]) in the
//! client world, main frame, every page, reports
//! - ⌥-click on a selection, or on text (it selects that sentence):
//!   `{"nb":"text","r":[x,y,w,h],"vw":w}`; the text is then read as the
//!   selection menu reads it (C20);
//! - ⌥-click on an image, video or canvas: `{"nb":"img", ...}` with its
//!   box (cut to what is in view);
//! - ⌥-drag, or a drag while the notebook's scissors are on (clip mode):
//!   `{"nb":"clip", ...}` with the box drawn;
//! - Escape in clip mode: `{"nb":"end"}`; a press or a scroll after a
//!   pick: `{"nb":"hide"}` (its "Send to Notebook" goes);
//! - ⌥-click on a link (2026-11-16): `{"nb":"link","url":...,"r":...}`;
//!   the notebook fetches the start of that page (cookie-free) for its
//!   title ([`page_title`]);
//! - a drag beginning in the page (2026-11-16): `{"nb":"dtext", ...}` with
//!   where the selection is, or `{"nb":"dimg", ...}` with the image's box;
//!   and `{"nb":"dend"}` when it ends. Dropped on the notebook, the drag
//!   becomes a quote (the selection read with C20) or a clip of that box:
//!   the drag's own data (the page's HTML, image bytes) is never used.
//!
//! Reports carry geometry only, parsed strictly ([`parse_report`]). An
//! image or a region becomes a picture the app draws itself from its own
//! web view (`takeSnapshotWithConfiguration`, as Save for Later's
//! thumbnails), never the page's image bytes, so the chrome decodes only
//! app-made JPEGs. Dragging the notebook's quote button onto a page shows
//! where it would land (constant code taking two numbers, [`hover_js`])
//! and reads the sentence there when let go ([`drop_js`], read back like
//! C20: [`parse_drop`]).
//!
//! Storage (`notebook.json`): what the chrome sends is checked and
//! rebuilt field by field ([`clean`]): ids, numbers, colours, http(s)
//! URLs, page text cleaned and capped, images only as
//! `data:image/{jpeg,png};base64` within bounds. Captures from private
//! tabs are refused (natively and in the chrome), and notes written while
//! a private tab is shown keep no address.

use serde_json::{Map, Value};
use url::Url;

use crate::selection::{SelectionRect, HANDLER_NAME};
use crate::text::sanitize_page_text;

/// Largest report accepted, in bytes (a link report carries its address).
pub const MAX_REPORT: usize = 5000;
/// Largest region text read back ([`region_text_js`]), and kept.
pub const MAX_REGION_BYTES: usize = 32 * 1024;
pub const MAX_REGION_CHARS: usize = 4000;
/// Largest notebook accepted (it holds its clips' images).
pub const MAX_NOTEBOOK_BYTES: usize = 64 * 1024 * 1024;
/// Largest clip image (a data URL), and the most kept.
pub const MAX_IMAGE_CHARS: usize = 4 * 1024 * 1024;
pub const MAX_STROKES: usize = 200_000;
pub const MAX_POINTS: usize = 40_000;
pub const MAX_NOTES: usize = 50_000;
pub const MAX_LINKS: usize = 20_000;
/// A typed note's words; a quote's text; a title.
pub const MAX_TEXT_CHARS: usize = 20_000;
pub const MAX_QUOTE_CHARS: usize = 4_000;
pub const MAX_TITLE_CHARS: usize = 300;
/// Largest quote read back by [`drop_js`] (a sentence, or the selection).
pub const MAX_DROP_BYTES: usize = 32 * 1024;

/// What the watcher reports.
#[derive(Clone, Debug, PartialEq)]
pub enum Report {
    /// ⌥-click on text: the selection (made or found) is here.
    Text(SelectionRect),
    /// ⌥-click on an image: its box in view.
    Image(SelectionRect),
    /// ⌥-drag, or a drag in clip mode: the box drawn.
    Clip(SelectionRect),
    /// Clip mode was left from the page (Escape).
    End,
    /// The page was pressed or scrolled after a pick: "Send to Notebook"
    /// goes.
    Hide,
    /// A drag began in the page: selected text (where the selection is),
    /// or an image (its box in view). What is dropped in the notebook is
    /// taken from the page by these, not from the drag's data.
    DragText(SelectionRect),
    DragImage(SelectionRect),
    /// A drag that began in the page ended (dropped anywhere, or not).
    DragEnd,
    /// ⌥-click on a link (2026-11-16): its address (http(s), checked) and
    /// its box. The notebook titles it with the linked page's own title
    /// ([`page_title`]).
    Link(String, SelectionRect),
}

/// Watches for ⌥-click, ⌥-drag and clip mode (see the module docs).
/// Constant code, main frame, document end. `window.__snorkleNb` (client
/// world only) also holds the functions [`clip_mode_js`], [`hover_js`],
/// [`drop_js`], [`LEAVE_JS`] and [`region_text_js`] call.
///
/// While ⌥ is held over the page (seen on key events and on the modifier
/// state of pointer moves), and in clip mode, a cover lies over the page:
/// a crosshair, and every press goes to it, so nothing on the page is
/// selected or followed. ⌥ with ⇧ (or ⌘, ⌃) is not the notebook's, so ⌥⇧
/// keys select word by word as usual (owner request 2026-11-16). The cover
/// is clear while ⌥ is only held (2026-11-16): the page dims once a drag
/// begins (the stage's shade around its box); in clip mode the cover also
/// fades the page slightly. A drag on the cover draws a box; let go, the
/// box stays as a stage (zotero-helper's clip mode): its corners size it,
/// a press inside moves it, a press outside draws another, and Clip (or
/// ↩) clips it, × (or Escape) drops it. A click without a drag is ⌥-click.
/// While a stage is up or being drawn, nothing on the page can be
/// selected (resizing it over text selected the text in WebKit).
/// Over the page's selection the cover lets presses through (owner request
/// 2026-11-16): an ⌥-drag there drags the selection (into the notebook),
/// and an ⌥-click sends it, kept as it was.
/// The quote button over the selection marks and takes all of it, not the
/// sentence under it.
/// Every handler takes only real input (`isTrusted`), so a page cannot
/// drive the cover or the stage with synthetic events.
pub fn watch_js() -> String {
    WATCH_JS.replace("__HANDLER__", HANDLER_NAME)
}

const WATCH_JS: &str = r#"(function(){var w=window.webkit,h=w&&w.messageHandlers&&w.messageHandlers.__HANDLER__;
if(!h||window.__snorkleNb)return;var nb=window.__snorkleNb={clip:false},Z='2147483647';
var css=document.createElement('style');css.textContent='::highlight(snorkle-nb){background-color:rgba(255,95,87,.3)}';(document.head||document.documentElement).appendChild(css);
function ok(e){return !!(e&&e.isTrusted);}
function optKey(e){return !!(e.altKey&&!e.shiftKey&&!e.metaKey&&!e.ctrlKey);}
function vp(){var v=window.visualViewport;return [v?v.offsetLeft:0,v?v.offsetTop:0,Math.round(v?v.width:innerWidth),Math.round(v?v.height:innerHeight)];}
function report(o){h.postMessage(JSON.stringify(o));}
function rectOf(kind,r){var o=vp(),x0=Math.max(0,r.left-o[0]),y0=Math.max(0,r.top-o[1]),x1=Math.min(o[2],r.right-o[0]),y1=Math.min(o[3],r.bottom-o[1]);
if(x1-x0<1||y1-y0<1)return null;return {nb:kind,r:[Math.round(x0),Math.round(y0),Math.round(x1-x0),Math.round(y1-y0)],vw:o[2]};}
var picked=0;function unpick(){if(picked){picked=0;report({nb:'hide'});}}
function post(kind,r){var m=rectOf(kind,r);if(!m)return;if(kind==='text'||kind==='img')picked=1;report(m);}
function el(styles,tag){var e=document.createElement(tag||'div');for(var k in styles)e.style.setProperty(k,styles[k],'important');document.documentElement.appendChild(e);return e;}
function at(e,k,v){e.style.setProperty(k,v,'important');}
var ov=null,pass=false,lastX=-1,lastY=-1,selPress=null,noSel=null;
function under(fn){if(ov)at(ov,'pointer-events','none');try{return fn();}finally{if(ov)at(ov,'pointer-events',pass?'none':'auto');}}
var MEDIA=/^(IMG|VIDEO|CANVAS|PICTURE)$/;
function mediaAt(x,y){var l=document.elementsFromPoint?document.elementsFromPoint(x,y):[];for(var i=0;i<l.length&&i<12;i++){var e=l[i];if(MEDIA.test(e.tagName)||e.hasAttribute('data-snorkle-video-box'))return e;if(e.tagName==='svg'&&e.getBoundingClientRect().width>40)return e;}return null;}
function linkAt(x,y){var l=document.elementsFromPoint?document.elementsFromPoint(x,y):[];for(var i=0;i<l.length&&i<12;i++){var a=l[i].closest?l[i].closest('a[href]'):null;if(a)return /^https?:/i.test(a.href||'')?a:null;}return null;}
function blockOf(n){var e=n.nodeType===1?n:n.parentElement;while(e&&e!==document.body&&e!==document.documentElement){var d=getComputedStyle(e).display;if(d!=='inline'&&d!=='contents'&&d.indexOf('inline-')!==0)return e;e=e.parentElement;}return document.body||document.documentElement;}
function sentenceAt(x,y){var c=document.caretRangeFromPoint?document.caretRangeFromPoint(x,y):null;if(!c||!c.startContainer||c.startContainer.nodeType!==3)return null;
var node=c.startContainer,off=c.startOffset,hit=document.elementFromPoint(x,y);if(!hit||!hit.contains(node))return null;
var b=blockOf(node),tw=document.createTreeWalker(b,NodeFilter.SHOW_TEXT),nodes=[],pos=0,here=-1,t;
while((t=tw.nextNode())&&nodes.length<5000&&pos<40000){if(t===node)here=pos+off;nodes.push([t,pos]);pos+=t.nodeValue.length;}
if(here<0)return null;var text=nodes.map(function(n){return n[0].nodeValue;}).join(''),s=0,e=text.length;
if(window.Intl&&Intl.Segmenter){var g=new Intl.Segmenter(undefined,{granularity:'sentence'}).segment(text).containing(Math.min(here,text.length-1));if(g){s=g.index;e=g.index+g.segment.length;}}
else{var re=/[.!?。！？]+["'”’)\]]*\s+/g,m;while((m=re.exec(text))){if(m.index+m[0].length<=here)s=m.index+m[0].length;else{e=m.index+m[0].length;break;}}}
while(e>s&&/\s/.test(text[e-1]))e--;while(s<e&&/\s/.test(text[s]))s++;if(e<=s)return null;
function loc(p,end){for(var i=0;i<nodes.length;i++){var n=nodes[i],len=n[0].nodeValue.length;if(p<n[1]+len||(end&&p===n[1]+len))return [n[0],p-n[1]];}var z=nodes[nodes.length-1];return [z[0],z[0].nodeValue.length];}
var r=document.createRange(),a=loc(s,false),z=loc(e,true);r.setStart(a[0],a[1]);r.setEnd(z[0],z[1]);return r;}
function inSelection(x,y){var s=window.getSelection();if(!s||!s.rangeCount||s.isCollapsed||!String(s).trim())return null;var r=s.getRangeAt(0),l=r.getClientRects();
for(var i=0;i<l.length;i++){var q=l[i];if(x>=q.left-4&&x<=q.right+4&&y>=q.top-4&&y<=q.bottom+4)return r;}return null;}
function quoteAt(x,y){var s=inSelection(x,y);return s?s.cloneRange():sentenceAt(x,y);}
function optionClick(x,y){return under(function(){var r=inSelection(x,y);if(r){post('text',r.getBoundingClientRect());return true;}
var m=mediaAt(x,y);if(m){post('img',m.getBoundingClientRect());return true;}
var a=linkAt(x,y);if(a){var b=a.getBoundingClientRect(),q=rectOf('link',b.width&&b.height?b:{left:x,top:y,right:x+1,bottom:y+1});if(q){q.url=String(a.href).slice(0,4096);picked=1;report(q);}return true;}
r=sentenceAt(x,y);if(!r)return false;var s=window.getSelection();s.removeAllRanges();s.addRange(r);post('text',r.getBoundingClientRect());return true;});}
var alt=false,spent=false,stage=null,drag=null,swallow=0,MIN=12;
function cover(on){if(on&&!ov){ov=el({position:'fixed',left:'0',top:'0',width:'100vw',height:'100vh',margin:'0',padding:'0',border:'0','z-index':Z,background:'transparent',cursor:'crosshair','touch-action':'none','-webkit-user-select':'none','user-select':'none'});ov.addEventListener('pointerdown',down,true);}
else if(!on&&ov){ov.remove();ov=null;}}
function noSelect(on){if(on&&!noSel){noSel=document.createElement('style');noSel.textContent='*{-webkit-user-select:none!important;user-select:none!important}';(document.head||document.documentElement).appendChild(noSel);}
else if(!on&&noSel){noSel.remove();noSel=null;}}
function sync(){var on=(alt&&!spent)||nb.clip||!!stage||!!drag;cover(on);if(ov)at(ov,'background',nb.clip?'rgba(128,128,128,.14)':'transparent');
pass=!!(on&&alt&&!nb.clip&&!stage&&!drag&&!selPress&&lastX>=0&&inSelection(lastX,lastY))||(!!selPress&&on);if(ov)at(ov,'pointer-events',pass?'none':'auto');noSelect(!!stage||!!drag);}
function ours(t){return !!t&&(t===ov||(!!stage&&(t===stage.box||(!!stage.bar&&stage.bar.contains(t))||stage.handles.some(function(p){return p[1]===t;}))));}
function setAlt(v){if(v===alt)return;alt=v;if(!v)spent=false;sync();}
function norm(b){return [Math.min(b[0],b[2]),Math.min(b[1],b[3]),Math.max(b[0],b[2]),Math.max(b[1],b[3])];}
function place(){var b=stage.b;at(stage.box,'left',b[0]+'px');at(stage.box,'top',b[1]+'px');at(stage.box,'width',(b[2]-b[0])+'px');at(stage.box,'height',(b[3]-b[1])+'px');
stage.handles.forEach(function(p){at(p[1],'left',((p[0].indexOf('w')>=0?b[0]:b[2])-7)+'px');at(p[1],'top',((p[0].indexOf('n')>=0?b[1]:b[3])-7)+'px');});
if(stage.bar){at(stage.bar,'left',Math.max(4,Math.min(b[0],innerWidth-140))+'px');at(stage.bar,'top',Math.max(4,Math.min(b[3]+8,innerHeight-44))+'px');}}
function grab(e,kind){if(!ok(e)||!stage||(e.pointerType==='mouse'&&e.button!==0))return;e.preventDefault();e.stopPropagation();
drag={id:e.pointerId,kind:kind,x:e.clientX,y:e.clientY,b0:stage.b.slice(),moved:false};try{e.target.setPointerCapture(e.pointerId);}catch(x){}sync();}
function show(b){if(!stage){stage={b:b,handles:[],bar:null};
stage.box=el({position:'fixed','z-index':Z,margin:'0',padding:'0',border:'2px dashed #ff3b30','box-sizing':'border-box',background:'transparent','box-shadow':'0 0 0 100vmax rgba(0,0,0,.16)',cursor:'move','touch-action':'none','-webkit-user-select':'none','user-select':'none'});
stage.box.addEventListener('pointerdown',function(e){grab(e,'move');},true);
['nw','ne','sw','se'].forEach(function(c){var k=el({position:'fixed','z-index':Z,margin:'0',padding:'0',width:'14px',height:'14px','border-radius':'7px',border:'2px solid #ff3b30',background:'#fff','box-sizing':'border-box',cursor:(c==='nw'||c==='se')?'nwse-resize':'nesw-resize','touch-action':'none','-webkit-user-select':'none','user-select':'none'});
k.addEventListener('pointerdown',function(e){grab(e,c);},true);stage.handles.push([c,k]);});}
stage.b=b;place();}
function button(text,act,primary){var b=document.createElement('div');b.textContent=text;var st={display:'inline-block',padding:'6px 12px',margin:'0','border-radius':'7px',cursor:'pointer',font:'600 13px -apple-system,BlinkMacSystemFont,sans-serif','line-height':'18px',color:primary?'#fff':'#111',background:primary?'#ff3b30':'#eee','user-select':'none','-webkit-user-select':'none'};
for(var k in st)b.style.setProperty(k,st[k],'important');b.setAttribute('role','button');
b.addEventListener('pointerdown',function(e){if(!ok(e))return;e.preventDefault();e.stopPropagation();},true);
b.addEventListener('pointerup',function(e){if(!ok(e))return;e.preventDefault();e.stopPropagation();swallow=Date.now();act();},true);
b.addEventListener('click',function(e){if(!ok(e))return;e.preventDefault();e.stopPropagation();act();},true);return b;}
function bar(){if(stage.bar)return;var b=el({position:'fixed','z-index':Z,display:'flex',gap:'6px',padding:'4px',margin:'0',background:'#fff','border-radius':'10px','box-shadow':'0 4px 16px rgba(0,0,0,.28)'});
b.appendChild(button('Clip',confirm,true));b.appendChild(button('×',function(){cancel(true);},false));stage.bar=b;place();}
function clearStage(){if(!stage)return;stage.box.remove();stage.handles.forEach(function(p){p[1].remove();});if(stage.bar)stage.bar.remove();stage=null;}
function confirm(){if(!stage)return;var b=stage.b;clearStage();drag=null;spent=alt;nb.clip=false;sync();
requestAnimationFrame(function(){post('clip',{left:b[0],top:b[1],right:b[2],bottom:b[3]});});}
function cancel(all){clearStage();drag=null;if(all){if(nb.clip){nb.clip=false;report({nb:'end'});}spent=alt;}sync();}
function down(e){if(!ok(e)||(e.pointerType==='mouse'&&e.button!==0))return;e.preventDefault();e.stopPropagation();
drag={id:e.pointerId,kind:'new',x:e.clientX,y:e.clientY,moved:false,alt:alt&&!nb.clip,b0:null};try{ov.setPointerCapture(e.pointerId);}catch(x){}sync();}
document.addEventListener('pointermove',function(e){if(!ok(e))return;lastX=e.clientX;lastY=e.clientY;
if(e.pointerType!=='touch'&&!drag&&!selPress){var a=optKey(e);if(a!==alt)setAlt(a);else if(alt)sync();}
if(!drag||e.pointerId!==drag.id)return;e.preventDefault();var dx=e.clientX-drag.x,dy=e.clientY-drag.y;
if(drag.kind==='new'){if(!drag.moved&&Math.abs(dx)+Math.abs(dy)<6)return;drag.moved=true;show(norm([drag.x,drag.y,e.clientX,e.clientY]));return;}
var b=drag.b0.slice();if(drag.kind==='move')b=[b[0]+dx,b[1]+dy,b[2]+dx,b[3]+dy];
else{if(drag.kind.indexOf('w')>=0)b[0]+=dx;if(drag.kind.indexOf('e')>=0)b[2]+=dx;if(drag.kind.indexOf('n')>=0)b[1]+=dy;if(drag.kind.indexOf('s')>=0)b[3]+=dy;b=norm(b);}
drag.moved=true;stage.b=b;place();},true);
function up(e){if(ok(e)&&selPress&&e.pointerId===selPress.id){var sp=selPress;selPress=null;
if(e.type==='pointerup'&&!sp.dragged&&Math.abs(e.clientX-sp.x)+Math.abs(e.clientY-sp.y)<6){swallow=Date.now();
setTimeout(function(){var s=window.getSelection();s.removeAllRanges();s.addRange(sp.r);post('text',sp.r.getBoundingClientRect());},0);}sync();return;}
if(!ok(e)||!drag||e.pointerId!==drag.id)return;var d=drag;drag=null;e.preventDefault();e.stopPropagation();swallow=Date.now();
if(d.kind==='new'){var b=stage&&stage.b;if(e.type!=='pointerup'||!d.moved){if(d.moved||!b||!stage.bar)cancel(false);else sync();if(e.type==='pointerup'&&!d.moved&&d.alt)optionClick(e.clientX,e.clientY);sync();return;}
if(b&&b[2]-b[0]>=MIN&&b[3]-b[1]>=MIN)bar();else cancel(false);}
sync();}
document.addEventListener('pointerup',up,true);document.addEventListener('pointercancel',up,true);
document.addEventListener('pointerdown',function(e){if(ok(e)&&pass&&optKey(e)&&!nb.clip&&!stage){var s=window.getSelection();
selPress=s&&s.rangeCount?{id:e.pointerId,x:e.clientX,y:e.clientY,r:s.getRangeAt(0).cloneRange(),dragged:false}:null;return;}
if(!ok(e)||optKey(e)||nb.clip)return;var t=e.target;if(ov&&(t===ov||(stage&&(t===stage.box||(stage.bar&&stage.bar.contains(t))))))return;unpick();},true);
window.addEventListener('scroll',function(){unpick();},{passive:true,capture:true});
['click','mousedown','mouseup','dblclick','auxclick','contextmenu'].forEach(function(t){document.addEventListener(t,function(e){if(!ok(e))return;
var own=swallow&&Date.now()-swallow<400;if(t==='mousedown'&&selPress){e.stopPropagation();return;}
if(t==='click'&&stage&&stage.bar&&stage.bar.contains(e.target)){e.preventDefault();return;}
if((optKey(e)&&t!=='contextmenu')||ours(e.target)||own){e.preventDefault();e.stopPropagation();if(t==='click')swallow=0;}},true);});
document.addEventListener('selectstart',function(e){if(ov||drag||stage)e.preventDefault();},true);
document.addEventListener('keydown',function(e){if(!ok(e))return;if(e.key==='Alt'){if(!e.metaKey&&!e.ctrlKey&&!e.shiftKey)setAlt(true);return;}if(e.key==='Shift'&&alt){setAlt(false);return;}
if(stage&&stage.bar&&e.key==='Enter'){e.preventDefault();e.stopPropagation();confirm();}else if((stage||nb.clip)&&e.key==='Escape'){e.preventDefault();e.stopPropagation();cancel(true);}},true);
document.addEventListener('keyup',function(e){if(!ok(e))return;if(e.key==='Alt')setAlt(false);else if(e.key==='Shift'&&optKey(e))setAlt(true);},true);
window.addEventListener('blur',function(){setAlt(false);});
nb.setClip=function(on){nb.clip=!!on;if(!on){clearStage();drag=null;}sync();};
nb.regionText=function(x,y,wd,ht){var o=vp();x+=o[0];y+=o[1];var x1=x+wd,y1=y+ht,out=[],n=0,len=0,last=null,rg=document.createRange();
function hit(q){return q.width>0&&q.height>0&&q.right>x&&q.left<x1&&q.bottom>y&&q.top<y1;}
var tw=document.createTreeWalker(document.body||document.documentElement,NodeFilter.SHOW_TEXT|NodeFilter.SHOW_ELEMENT,{acceptNode:function(e){return e.nodeType===1&&/^(SCRIPT|STYLE|NOSCRIPT|TEMPLATE)$/.test(e.tagName)?NodeFilter.FILTER_REJECT:NodeFilter.FILTER_ACCEPT;}}),t;
while((t=tw.nextNode())&&n<40000&&len<4000){n++;
if(t.nodeType===1){if(t.tagName==='IMG'&&t.alt&&hit(t.getBoundingClientRect())){out.push('\n'+t.alt);len+=t.alt.length;}continue;}
var v=t.nodeValue;if(!v||!v.trim())continue;rg.selectNodeContents(t);var rs=rg.getClientRects(),on=false;for(var i=0;i<rs.length;i++)if(hit(rs[i])){on=true;break;}
if(!on)continue;var bk=blockOf(t);out.push((last&&bk!==last?'\n':'')+v);last=bk;len+=v.length;}
return JSON.stringify({t:out.join(' ').replace(/[ \t]+/g,' ').replace(/ *\n */g,'\n').slice(0,4000)});};
document.addEventListener('dragstart',function(e){if(!ok(e))return;var t=e.target&&e.target.nodeType===1?e.target:(e.target&&e.target.parentElement),s=window.getSelection(),
txt=s&&s.rangeCount&&!s.isCollapsed&&String(s).trim(),img=t&&t.closest?t.closest('img,picture,video,canvas'):null;
if(selPress)selPress.dragged=true;
if(img&&!(txt&&s.containsNode(img,true)))post('dimg',img.getBoundingClientRect());else if(txt)post('dtext',s.getRangeAt(0).getBoundingClientRect());},true);
document.addEventListener('dragend',function(e){if(!ok(e))return;report({nb:'dend'});if(selPress){selPress=null;sync();}},true);
var dot=null,line=null,mark=null,doneAt=0;
function clearHover(){doneAt=Date.now();if(dot){dot.remove();dot=null;}if(line){line.remove();line=null;}if(mark){mark.clear();mark=null;}if(window.CSS&&CSS.highlights)CSS.highlights.delete('snorkle-nb');}
function point(fx,fy){var o=vp();return [o[0]+fx*o[2],o[1]+fy*o[3]];}
nb.hover=function(fx,fy,fy0){if(Date.now()-doneAt<300)return;var p=point(fx,fy),o=vp();
if(!dot)dot=el({position:'fixed','z-index':Z,'pointer-events':'none',width:'10px',height:'10px','border-radius':'5px',background:'#ff5f57',margin:'0',padding:'0'});
at(dot,'left',(p[0]-5)+'px');at(dot,'top',(p[1]-5)+'px');
if(!line)line=el({position:'fixed','z-index':Z,'pointer-events':'none',height:'0',margin:'0',padding:'0','border-top':'2px dashed #ff5f57','transform-origin':'0 0'});
var x0=o[0]+o[2],y0=o[1]+fy0*o[3],dx=p[0]-x0,dy=p[1]-y0;at(line,'left',x0+'px');at(line,'top',y0+'px');at(line,'width',Math.hypot(dx,dy)+'px');at(line,'transform','rotate('+Math.atan2(dy,dx)+'rad)');
var r=under(function(){return quoteAt(p[0],p[1]);});
if(window.CSS&&CSS.highlights&&window.Highlight){if(!mark){mark=new Highlight();CSS.highlights.set('snorkle-nb',mark);}mark.clear();if(r)mark.add(r);}};
nb.drop=function(fx,fy){var p=point(fx,fy),r=under(function(){return quoteAt(p[0],p[1]);});clearHover();return JSON.stringify({t:r?String(r).slice(0,4000):''});};
nb.leave=clearHover;
window.addEventListener('pagehide',function(){nb.setClip(false);clearHover();setAlt(false);});})();"#;

/// Turns clip mode on or off in a page (the notebook's scissors).
pub fn clip_mode_js(on: bool) -> String {
    format!("window.__snorkleNb&&window.__snorkleNb.setClip({on});")
}

/// A fraction of the page's view, finite and within 0–1.
fn frac(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// The quote button dragged over a page: where it would land (fractions
/// of the view) and the height it left the page's right edge at.
pub fn hover_js(fx: f64, fy: f64, from_y: f64) -> String {
    format!("window.__snorkleNb&&window.__snorkleNb.hover({:.5},{:.5},{:.5});", frac(fx), frac(fy), frac(from_y))
}

/// The quote button let go over a page: the sentence there, as
/// `{"t": text}` (read back, [`parse_drop`]).
pub fn drop_js(fx: f64, fy: f64) -> String {
    format!("(function(){{var n=window.__snorkleNb;return n?n.drop({:.5},{:.5}):'';}})()", frac(fx), frac(fy))
}

/// The text shown in a region of the page (visual-viewport CSS pixels,
/// as the clip's report gave it), for the notebook's search: text nodes
/// laid out in the region and images' `alt`, as `{"t": text}` (read back,
/// [`parse_region_text`]).
pub fn region_text_js(x: f64, y: f64, w: f64, h: f64) -> String {
    let n = |v: f64| if v.is_finite() { v.clamp(-100_000.0, 100_000.0) } else { 0.0 };
    format!(
        "(function(){{var n=window.__snorkleNb;return n&&n.regionText?n.regionText({:.1},{:.1},{:.1},{:.1}):'';}})()",
        n(x),
        n(y),
        n(w).max(0.0),
        n(h).max(0.0)
    )
}

/// [`region_text_js`]'s answer: cleaned and capped; `None` if empty or
/// malformed.
pub fn parse_region_text(text: &str) -> Option<String> {
    if text.len() > MAX_REGION_BYTES {
        return None;
    }
    let v: Value = serde_json::from_str(text).ok()?;
    let t = crate::ai::clean_multiline(v.get("t")?.as_str()?, MAX_REGION_CHARS);
    (!t.is_empty()).then_some(t)
}

/// A page's title from the start of its HTML (the first `<title>`, else
/// `og:title`), entities decoded, cleaned and capped; `None` when it has
/// none. Only reads text: nothing is rendered or run.
pub fn page_title(html: &[u8]) -> Option<String> {
    let head = &html[..html.len().min(512 * 1024)];
    let text = String::from_utf8_lossy(head);
    let lower = text.to_ascii_lowercase();
    let from_title = || -> Option<String> {
        let open = lower.find("<title")?;
        let start = open + lower[open..].find('>')? + 1;
        let end = start + lower[start..].find("</title")?;
        Some(text[start..end].to_string())
    };
    let from_og = || -> Option<String> {
        let at = lower.find("og:title")?;
        let tag_start = lower[..at].rfind('<')?;
        let tag_end = at + lower[at..].find('>')?;
        let tag = &text[tag_start..tag_end];
        let c = tag.to_ascii_lowercase().find("content=")? + "content=".len();
        let rest = &tag[c..];
        let quote = rest.chars().next().filter(|q| *q == '"' || *q == '\'')?;
        let body = &rest[1..];
        Some(body[..body.find(quote)?].to_string())
    };
    [from_title(), from_og()]
        .into_iter()
        .flatten()
        .map(|raw| sanitize_page_text(&decode_entities(&raw), MAX_TITLE_CHARS))
        .find(|t| !t.is_empty())
}

/// The few HTML entities titles use: the five named ones, `&nbsp;` and
/// numeric references (control and invalid code points dropped).
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest.find(';').filter(|e| *e <= 12);
        let Some(end) = end else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let ch = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            n if n.starts_with("#x") || n.starts_with("#X") => {
                u32::from_str_radix(&n[2..], 16).ok().and_then(char::from_u32)
            }
            n if n.starts_with('#') => n[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) if !c.is_control() => out.push(c),
            Some(_) => {}
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The quote button left the page (or the drag ended elsewhere).
pub const LEAVE_JS: &str = "window.__snorkleNb&&window.__snorkleNb.leave();";

/// The sentence [`drop_js`] read: cleaned and capped; `None` if there is
/// none or the answer is malformed.
pub fn parse_drop(text: &str) -> Option<String> {
    if text.len() > MAX_DROP_BYTES {
        return None;
    }
    let v: Value = serde_json::from_str(text).ok()?;
    let t = crate::ai::clean_multiline(v.get("t")?.as_str()?, MAX_QUOTE_CHARS);
    (!t.is_empty()).then_some(t)
}

/// Whether a body posted to the selection handler is a notebook report.
pub fn is_report(body: &str) -> bool {
    body.starts_with("{\"nb\":")
}

/// Parses a [`watch_js`] report. Fails closed.
pub fn parse_report(body: &str) -> Option<Report> {
    if body.len() > MAX_REPORT || !is_report(body) {
        return None;
    }
    if body == r#"{"nb":"end"}"# {
        return Some(Report::End);
    }
    if body == r#"{"nb":"hide"}"# {
        return Some(Report::Hide);
    }
    if body == r#"{"nb":"dend"}"# {
        return Some(Report::DragEnd);
    }
    let v: Value = serde_json::from_str(body).ok()?;
    let kind = v.get("nb")?.as_str()?;
    let r = v.get("r")?.as_array()?;
    if r.len() != 4 {
        return None;
    }
    let n: Vec<f64> = r.iter().map(Value::as_f64).collect::<Option<Vec<f64>>>()?;
    let vw = v.get("vw")?.as_f64()?;
    let bounded = |x: f64| x.is_finite() && x.abs() <= 100_000.0;
    if !n.iter().all(|x| bounded(*x)) || n[2] < 1.0 || n[3] < 1.0 || !bounded(vw) || vw < 1.0 {
        return None;
    }
    let rect = SelectionRect { x: n[0], y: n[1], w: n[2], h: n[3], vw };
    match kind {
        "text" => Some(Report::Text(rect)),
        "img" => Some(Report::Image(rect)),
        "dtext" => Some(Report::DragText(rect)),
        "dimg" => Some(Report::DragImage(rect)),
        "link" => web_url(v.get("url")?.as_str()?).map(|u| Report::Link(u, rect)),
        "clip" => Some(Report::Clip(rect)),
        _ => None,
    }
}

// ---- storage -------------------------------------------------------------

/// An id the chrome made: letters, digits and `-`, at most 64.
fn id(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    (!s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then(|| s.to_string())
}

fn num(v: Option<&Value>, max: f64) -> Option<f64> {
    v?.as_f64().filter(|n| n.is_finite() && n.abs() <= max)
}

/// The theme's ink (""), or `#rrggbb`.
fn color(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    let ok = s.is_empty() || (s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit()));
    ok.then(|| s.to_ascii_lowercase())
}

/// An http(s) address without credentials, at most 4,096 characters.
pub fn web_url(raw: &str) -> Option<String> {
    if raw.len() > 4096 {
        return None;
    }
    let u = Url::parse(raw).ok()?;
    (matches!(u.scheme(), "http" | "https") && u.username().is_empty() && u.password().is_none() && u.host().is_some())
        .then(|| u.to_string())
}

/// A clip's picture: `data:image/jpeg` or `data:image/png`, base64.
fn image(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    let payload = s.strip_prefix("data:image/jpeg;base64,").or_else(|| s.strip_prefix("data:image/png;base64,"))?;
    (s.len() <= MAX_IMAGE_CHARS
        && !payload.is_empty()
        && payload.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')))
    .then(|| s.to_string())
}

/// Typed text: control characters other than line breaks and tabs go.
fn typed(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).take(max).collect()
}

/// Where something came from: an address and a title, either may be
/// missing (a private tab's, or the start page).
fn source(o: &Map<String, Value>, out: &mut Map<String, Value>) {
    if let Some(u) = o.get("url").and_then(Value::as_str).and_then(web_url) {
        out.insert("url".into(), Value::String(u));
    }
    if let Some(t) = o.get("title").and_then(Value::as_str) {
        let t = sanitize_page_text(t, MAX_TITLE_CHARS);
        if !t.is_empty() {
            out.insert("title".into(), Value::String(t));
        }
    }
}

fn stroke(v: &Value) -> Option<Value> {
    let o = v.as_object()?;
    let pts = o.get("points")?.as_array()?;
    if pts.len() < 2 || pts.len() > MAX_POINTS || pts.len() % 2 != 0 {
        return None;
    }
    let points: Vec<Value> = pts.iter().map(|p| num(Some(p), 1e7).map(Value::from)).collect::<Option<Vec<Value>>>()?;
    let width = num(o.get("width"), 50.0).filter(|w| *w > 0.0)?;
    Some(serde_json::json!({
        "id": id(o.get("id"))?,
        "points": points,
        "width": width,
        "color": color(o.get("color"))?,
        "note": id(o.get("note"))?,
    }))
}

fn note(v: &Value) -> Option<Value> {
    let o = v.as_object()?;
    let mut out = Map::new();
    out.insert("id".into(), Value::String(id(o.get("id"))?));
    out.insert("createdMs".into(), Value::from(o.get("createdMs").and_then(Value::as_u64).unwrap_or(0)));
    source(o, &mut out);
    Some(Value::Object(out))
}

fn link(v: &Value) -> Option<Value> {
    let o = v.as_object()?;
    let mut out = Map::new();
    out.insert("id".into(), Value::String(id(o.get("id"))?));
    out.insert("x".into(), Value::from(num(o.get("x"), 1e7)?));
    out.insert("y".into(), Value::from(num(o.get("y"), 1e7)?));
    for k in ["w", "h"] {
        if let Some(n) = num(o.get(k), 10_000.0).filter(|n| *n > 0.0) {
            out.insert(k.into(), Value::from(n));
        }
    }
    if o.get("fixed").and_then(Value::as_bool) == Some(true) {
        out.insert("fixed".into(), Value::Bool(true));
    }
    if let Some(t) = o.get("text").and_then(Value::as_str) {
        out.insert("text".into(), Value::String(typed(t, MAX_TEXT_CHARS)));
    }
    if let Some(img) = image(o.get("image")) {
        out.insert("image".into(), Value::String(img));
        if let Some(dark) = image(o.get("imageDark")) {
            out.insert("imageDark".into(), Value::String(dark));
        }
    }
    let t = o.get("target")?.as_object()?;
    let kind = match t.get("kind")?.as_str()? {
        k @ ("page" | "quote" | "clip") => k,
        _ => return None,
    };
    let mut target = Map::new();
    target.insert("kind".into(), Value::String(kind.into()));
    source(t, &mut target);
    if let Some(q) = t.get("text").and_then(Value::as_str) {
        let q = crate::ai::clean_multiline(q, MAX_QUOTE_CHARS);
        if !q.is_empty() {
            target.insert("text".into(), Value::String(q));
        }
    }
    // A clip has its picture; a quote its words.
    if (kind == "clip" && !out.contains_key("image"))
        || (kind == "quote" && !target.contains_key("text") && !out.contains_key("text"))
    {
        return None;
    }
    out.insert("target".into(), Value::Object(target));
    Some(Value::Object(out))
}

/// The notebook as the chrome sends it, checked and rebuilt (see the
/// module docs): what is malformed is left out; `None` when the whole is
/// not a notebook or is too large.
pub fn clean(json: &str) -> Option<String> {
    if json.len() > MAX_NOTEBOOK_BYTES {
        return None;
    }
    let v: Value = serde_json::from_str(json).ok()?;
    let o = v.as_object()?;
    let list = |k: &str, max: usize, f: fn(&Value) -> Option<Value>| -> Vec<Value> {
        o.get(k).and_then(Value::as_array).map(|a| a.iter().take(max).filter_map(f).collect()).unwrap_or_default()
    };
    let strokes = list("strokes", MAX_STROKES, stroke);
    let notes = list("notes", MAX_NOTES, note);
    let links = list("links", MAX_LINKS, link);
    let pan = o.get("pan").and_then(Value::as_object);
    let pan = serde_json::json!({
        "x": pan.and_then(|p| num(p.get("x"), 1e7)).unwrap_or(0.0),
        "y": pan.and_then(|p| num(p.get("y"), 1e7)).unwrap_or(0.0),
    });
    let out = serde_json::json!({ "version": 1, "strokes": strokes, "notes": notes, "links": links, "pan": pan });
    let text = serde_json::to_string(&out).ok()?;
    (text.len() <= MAX_NOTEBOOK_BYTES).then_some(text)
}

#[cfg(test)]
mod tests;
