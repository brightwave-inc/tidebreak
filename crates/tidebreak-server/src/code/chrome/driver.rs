//! Snapshot scripts run in a Chrome isolated world. Viewport mapping and
//! other evals that pass no world run in the page's main world. Page scripts
//! cannot replace the reference table; every action checks the same live
//! node and fingerprint.

pub const SNAPSHOT_SCRIPT: &str = r#"(options => {
  const entries = new Map(), nodes = [];
  const visible = el => { const r=el.getBoundingClientRect(), s=getComputedStyle(el); return r.width>0 && r.height>0 && s.visibility!=="hidden" && s.display!=="none"; };
  const label = el => (el.getAttribute('aria-label') || el.labels?.[0]?.innerText || el.innerText || el.getAttribute('placeholder') || el.getAttribute('name') || '').trim().slice(0,512);
  const fingerprint = el => JSON.stringify([el.tagName,el.getAttribute('role'),el.getAttribute('type'),label(el),el.getAttribute('href'),el.disabled,el.checked,el.value,el.isContentEditable]);
  let truncated=false,visited=0;
  function walk(root, depth) {
    if(depth>32 || nodes.length>=options.max || visited>=10000) {truncated=true; return;}
    for(const el of root.children || []) {
      if(++visited>10000){truncated=true;break;}
      if(nodes.length>=options.max) {truncated=true; break;}
      if(['SCRIPT','STYLE','NOSCRIPT','TEMPLATE'].includes(el.tagName)) continue;
      const interactive=el.matches('button,a[href],input:not([type=hidden]),textarea,select,[tabindex],[contenteditable=true],[role=button],[role=checkbox],[draggable=true]');
      if(visible(el) && (interactive || !el.children.length)) {
        const text=(el.innerText||'').trim().slice(0,2000);
        if(interactive || text) {
          const ref=interactive ? options.prefix+'-'+nodes.length : null;
          const sensitive=el.matches('input[type=password],input[autocomplete=one-time-code]');
          const r=el.getBoundingClientRect();
          if(ref) entries.set(ref,{el, fingerprint:fingerprint(el)});
          nodes.push({kind:interactive?'interactive':'content', ref, tag:el.tagName.toLowerCase(),role:el.getAttribute('role')||el.tagName.toLowerCase(),name:label(el),frame:options.frame,text:interactive?null:text,value:sensitive?null:(el.value===undefined?null:String(el.value).slice(0,256)),href:el.getAttribute('href'),inputType:el.getAttribute('type'),disabled:Boolean(el.disabled),checked:el.checked===undefined?null:Boolean(el.checked),sensitive,actions:interactive?['click','double_click','hover','type','fill','select','check','press','scroll','drag']:[],bounds:{x:r.x,y:r.y,width:r.width,height:r.height}});
        }
      }
      if(el.shadowRoot) walk(el.shadowRoot,depth+1);
      walk(el,depth+1);
    }
  }
  walk(document,0);
  globalThis.__tidebreakChrome={entries,fingerprint,snapshot:options.snapshot};
  return {nodes,truncated};
})"#;

pub const PROBE_SCRIPT: &str = r#"(args => {
 const state=globalThis.__tidebreakChrome;
 const entry=state?.snapshot===args.snapshot?state.entries.get(args.ref):null;
 if(!entry || !entry.el.isConnected || state.fingerprint(entry.el)!==entry.fingerprint) return {ok:false,reason:'stale_target'};
 const el=entry.el;
 if(el.disabled) return {ok:false,reason:'disabled'};
 if(args.scroll) el.scrollIntoView({block:'center',inline:'center',behavior:'instant'});
 const r=el.getBoundingClientRect();
 const x=r.left+r.width/2,y=r.top+r.height/2;
 if(!r.width || !r.height || x<0 || y<0 || x>innerWidth || y>innerHeight) return {ok:false,reason:'not_visible'};
 const hit=document.elementFromPoint(x,y);
 if(hit!==el && !el.contains(hit) && el.getRootNode().host!==hit) return {ok:false,reason:'occluded'};
 if(args.focus) {el.focus(); if(document.activeElement!==el && el.getRootNode().activeElement!==el) return {ok:false,reason:'focus_failed'};}
 if(args.operation==='fill') {
   if(el.isContentEditable) {el.textContent=args.value;}
   else {const proto=el instanceof HTMLTextAreaElement?HTMLTextAreaElement.prototype:HTMLInputElement.prototype;
     const set=Object.getOwnPropertyDescriptor(proto,'value')?.set; if(!set) return {ok:false,reason:'invalid_value'}; set.call(el,args.value);}
   el.dispatchEvent(new Event('input',{bubbles:true}));el.dispatchEvent(new Event('change',{bubbles:true}));
 }
 if(args.operation==='select') {
   if(!(el instanceof HTMLSelectElement)) return {ok:false,reason:'invalid_value'};
   if(!Array.from(el.options).some(option=>option.value===args.value)) return {ok:false,reason:'invalid_value'};
   el.value=args.value;el.dispatchEvent(new Event('input',{bubbles:true}));el.dispatchEvent(new Event('change',{bubbles:true}));
 }
 if(args.operation==='check') {
   if(!el.matches('input[type=checkbox],input[type=radio]')) return {ok:false,reason:'invalid_value'};
   if(el.checked!==args.value) el.click();
 }
 return {ok:true,x,y,viewport:{width:innerWidth,height:innerHeight}};
})"#;
