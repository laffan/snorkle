// Constant browser-composited filter. No page pixels are read by JavaScript.
(function(){
if(window.__snorkleDither)return;
const ns='http://www.w3.org/2000/svg', state={stopped:false};
window.__snorkleDither=state;
function node(tag,attrs,parent){const n=document.createElementNS(ns,tag);Object.entries(attrs).forEach(([k,v])=>n.setAttribute(k,v));if(parent)parent.appendChild(n);return n;}
const svg=node('svg',{'aria-hidden':'true',width:'0',height:'0'});
svg.style.cssText='position:fixed;left:0;top:0;width:0;height:0;pointer-events:none;overflow:hidden';
const defs=node('defs',{},svg), f=node('filter',{id:'__snorkle_dither',x:'0%',y:'0%',width:'100%',height:'100%', 'color-interpolation-filters':'sRGB'},defs);
node('feImage',{href:'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAEUlEQVR4nGP4/58BBkGI4T8AUr8H+TUxP1AAAAAASUVORK5CYII=',x:0,y:0,width:2,height:2,result:'cell',style:'image-rendering:pixelated'},f);
node('feTile',{in:'cell',result:'grid'},f);
const offsets=node('feComponentTransfer',{in:'grid',result:'offsets'},f);
['R','G'].forEach(c=>node('feFunc'+c,{type:'linear',slope:'.5'},offsets));
node('feDisplacementMap',{in:'SourceGraphic',in2:'offsets',scale:2,xChannelSelector:'R',yChannelSelector:'G',result:'pixels'},f);
node('feColorMatrix',{in:'pixels',type:'matrix',values:'.2126 .7152 .0722 0 0 .2126 .7152 .0722 0 0 .2126 .7152 .0722 0 0 0 0 0 1 0',result:'gray'},f);
node('feImage',{href:'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAYAAADED76LAAAAUklEQVR4nIXKERoAIAwG0EEQBEEQBINgEAyCYPe/1vr2X2Dw7BEReVBVaK2BmQGlgZk99N7hnANrLchDrdXDew9KKXDvhTyIiIc5J+y9YYwBafgEPFZBZ+5wBwAAAABJRU5ErkJggg==',x:0,y:0,width:8,height:8,result:'bayer',style:'image-rendering:pixelated'},f);
node('feTile',{in:'bayer',result:'thresholds'},f);
// PNG values are rank*4/255. Multiplying by 255/256 yields exact rank/64.
node('feComposite',{in:'gray',in2:'thresholds',operator:'arithmetic',k1:0,k2:'.75',k3:String(.75*255/256),k4:'-.0625',result:'ordered'},f);
const quant=node('feComponentTransfer',{in:'ordered',result:'quantized'},f);
['R','G','B'].forEach(c=>node('feFunc'+c,{type:'discrete',tableValues:'0 0.3333333333 0.6666666667 1'},quant));
node('feComposite',{in:'quantized',in2:'SourceAlpha',operator:'in'},f);
function put(){if(state.stopped)return;const root=document.documentElement;if(root&&!svg.isConnected)root.appendChild(svg);if(root&&state.ready)root.classList.add('snorkle-dither-ready');}
const obs=new MutationObserver(put);obs.observe(document,{childList:true,subtree:true});
state.stop=function(){state.stopped=true;obs.disconnect();svg.remove();document.documentElement?.classList.remove('snorkle-dither-ready');delete window.__snorkleDither;};
// Pages that disallow embedded images keep ordinary dark-mode dimming.
let loaded=0;['data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAYAAADED76LAAAAUklEQVR4nIXKERoAIAwG0EEQBEEQBINgEAyCYPe/1vr2X2Dw7BEReVBVaK2BmQGlgZk99N7hnANrLchDrdXDew9KKXDvhTyIiIc5J+y9YYwBafgEPFZBZ+5wBwAAAABJRU5ErkJggg==','data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAEUlEQVR4nGP4/58BBkGI4T8AUr8H+TUxP1AAAAAASUVORK5CYII='].forEach(src=>{const image=new Image();image.onload=()=>{if(++loaded===2){state.ready=true;put();}};image.src=src;});
put();
})();
