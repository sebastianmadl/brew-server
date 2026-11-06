//! The redesigned dashboard, served at `/` (unless `[dashboard] legacy_ui =
//! true`) and always at `/ui`. One page with a sidebar (FlowStation's layout), light
//! and dark following the system. Its own pages read the same JSON APIs as
//! the classic dashboard; the classic sub-pages (SIP, SMS Center, HA,
//! Settings, Map, Telemetry SDS) are shown inside it in their embedded form
//! (`?embed=1`, see `EMBED_STYLE`).

use axum::response::Html;

const VERSION: &str = env!("CARGO_PKG_VERSION");

static PAGE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let name = crate::dashboard::html_escape(crate::dashboard::server_name());
    PAGE_HTML.replace("__VERSION__", VERSION).replace("__SERVERNAME__", &name)
});

pub async fn page() -> Html<&'static str> {
    Html(PAGE.as_str())
}

// The classic pages' embedded form lives in `dashboard::STYLE` (`html.embed`).
const PAGE_HTML: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<meta name="color-scheme" content="light dark">
<title>__SERVERNAME__</title>
<style>
:root{
  --bg:#eceff4;--bg2:#ffffff;--bg3:#f3f5f9;--bg4:#d6dde7;
  --border:#dde3ec;--sep:rgba(20,30,50,0.09);
  --accent:#00876a;--accent2:#1565c0;--warn:#9a5400;--danger:#c0203a;--ok:#16876b;
  --text:#16202e;--text2:#3d4f66;--text3:#5f7188;
  --sidebar:#ffffff;--sidebar-border:#e3e8ef;
  --shadow:0 1px 3px rgba(20,30,50,.06),0 4px 16px -8px rgba(20,30,50,.10);
  --hair:inset 0 1px 0 rgba(255,255,255,.7);
  --on-accent:#ffffff;
  --mono:ui-monospace,'SF Mono','Cascadia Code',Menlo,Consolas,monospace;
  --sans:-apple-system,BlinkMacSystemFont,'Inter','Segoe UI',system-ui,sans-serif;
}
@media (prefers-color-scheme:dark){:root{
  --bg:#090d14;--bg2:#111824;--bg3:#19212f;--bg4:#2b3850;
  --border:#232e40;--sep:rgba(255,255,255,0.07);
  --accent:#00d4a8;--accent2:#4da6ff;--warn:#ffb224;--danger:#ff4d6d;--ok:#2ec6a6;
  --text:#eef3fb;--text2:#94abc9;--text3:#7f93b5;
  --sidebar:#070a10;--sidebar-border:#161d2c;
  --shadow:0 1px 2px rgba(0,0,0,.45),0 10px 28px -14px rgba(0,0,0,.65);
  --hair:inset 0 1px 0 rgba(255,255,255,.05);
  --on-accent:#04120e;
}}
@media (prefers-contrast:more){:root{--text2:var(--text);--text3:var(--text2);--border:var(--text3)}}
*{box-sizing:border-box}
html,body{margin:0;height:100%}
body{background:var(--bg);color:var(--text);font:14px/1.45 var(--sans);display:flex;-webkit-font-smoothing:antialiased}
button{font:inherit;color:inherit}
svg.i{width:18px;height:18px;stroke:currentColor;fill:none;stroke-width:1.8;stroke-linecap:round;stroke-linejoin:round;flex-shrink:0}
a{color:var(--accent2);text-decoration:none}
:focus-visible{outline:2px solid var(--accent2);outline-offset:2px}
[hidden]{display:none!important}
.sr{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0)}

/* Sidebar */
#sb{width:236px;min-width:236px;background:var(--sidebar);border-right:1px solid var(--sidebar-border);display:flex;flex-direction:column;height:100vh;height:100dvh;position:sticky;top:0;z-index:20}
.brand{padding:18px 16px 14px;border-bottom:1px solid var(--sidebar-border);display:flex;flex-direction:column;gap:12px}
.brand-row{display:flex;gap:10px;align-items:center}
.logo{width:34px;height:34px;border-radius:9px;display:grid;place-items:center;color:var(--accent);flex-shrink:0;
  background:linear-gradient(135deg,color-mix(in srgb,var(--accent) 16%,transparent),color-mix(in srgb,var(--accent2) 16%,transparent));
  border:1px solid color-mix(in srgb,var(--accent) 30%,transparent)}
.bname{font-weight:700;font-size:14px;line-height:1.2;overflow-wrap:anywhere}
.bsub{font:10.5px var(--mono);color:var(--text3);letter-spacing:.04em}
.hw{border:1px solid var(--sidebar-border);border-radius:10px;padding:4px;background:color-mix(in srgb,var(--text) 3%,transparent);box-shadow:var(--hair)}
.hw-row{display:flex;align-items:center;gap:9px;padding:6px 7px;border-radius:7px;cursor:pointer}
.hw-row:hover{background:color-mix(in srgb,var(--text) 4%,transparent)}
.hw-row+.hw-row{box-shadow:inset 0 1px 0 var(--sidebar-border)}
.hw-g{width:22px;height:22px;border-radius:6px;display:grid;place-items:center;color:var(--accent);background:color-mix(in srgb,var(--accent) 13%,transparent)}
.hw-g svg{width:13px;height:13px}
.hw-g.b{color:var(--accent2);background:color-mix(in srgb,var(--accent2) 13%,transparent)}
.hw-k{font:700 9px var(--mono);letter-spacing:.12em;text-transform:uppercase;color:var(--text3)}
.hw-v{font:600 11.5px var(--mono);color:var(--text2)}
.dot{width:8px;height:8px;border-radius:50%;background:var(--ok);margin-left:auto;flex-shrink:0}
.dot.w{background:var(--warn)} .dot.d{background:var(--danger)} .dot.i{background:var(--text3)}
nav{padding:8px 8px 12px;overflow:auto;flex:1}
.grp{font:700 10.5px var(--sans);letter-spacing:.1em;text-transform:uppercase;color:var(--text3);padding:14px 10px 6px}
.nv{display:flex;align-items:center;gap:11px;padding:8px 10px;border-radius:8px;color:var(--text2);font-size:14px;cursor:pointer;min-height:36px;border:0;background:none;width:100%;text-align:left}
.nv svg{color:var(--text3)}
.nv:hover{background:color-mix(in srgb,var(--text) 5%,transparent)}
.nv.on{background:color-mix(in srgb,var(--accent) 13%,transparent);color:var(--accent);font-weight:600}
.nv.on svg{color:var(--accent)}
.badge{margin-left:auto;font:700 10.5px var(--mono);padding:1px 7px;border-radius:99px;background:color-mix(in srgb,var(--accent) 16%,transparent);color:var(--accent)}
.badge.w{background:color-mix(in srgb,var(--warn) 18%,transparent);color:var(--warn)}
.badge:empty{display:none}
.lock{margin-left:auto;color:var(--text3);display:none} .lock svg{width:13px;height:13px}
body.ro .lock{display:inline-flex}
.sbfoot{border-top:1px solid var(--sidebar-border);padding:10px 14px 12px;font:11px var(--sans);color:var(--text3);line-height:1.6}
.sbfoot a{color:var(--accent2)}
.sbfoot .acct{display:flex;align-items:center;gap:8px;margin-bottom:6px;color:var(--text2)}
.sbfoot .acct button{margin-left:auto}

/* Main */
main{flex:1;min-width:0;display:flex;flex-direction:column;height:100vh;height:100dvh;overflow:auto}
.top{position:sticky;top:0;z-index:5;display:flex;align-items:center;gap:12px;padding:14px 28px;border-bottom:1px solid var(--sidebar-border);
  background:color-mix(in srgb,var(--bg) 82%,transparent);-webkit-backdrop-filter:blur(14px) saturate(1.4);backdrop-filter:blur(14px) saturate(1.4)}
@media (prefers-reduced-transparency:reduce){.top{background:var(--bg)}}
.top h1{font-size:22px;font-weight:700;margin:0;letter-spacing:-.01em}
.pill{display:inline-flex;align-items:center;gap:6px;font:600 11.5px var(--sans);padding:3px 10px;border-radius:99px;border:1px solid var(--border);color:var(--text2);background:var(--bg2);white-space:nowrap}
.pill i{width:7px;height:7px;border-radius:50%;background:var(--ok)}
.pill.w i{background:var(--warn)} .pill.d i{background:var(--danger)}
.sp{flex:1}
.btn{display:inline-flex;align-items:center;gap:7px;min-height:32px;padding:0 12px;border-radius:8px;border:1px solid var(--border);background:var(--bg2);color:var(--text);font:600 12.5px var(--sans);cursor:pointer;white-space:nowrap}
.btn:hover{border-color:var(--text3)}
.btn svg{width:15px;height:15px}
.btn.pri{background:var(--accent);border-color:var(--accent);color:var(--on-accent)}
.btn.danger{color:var(--danger)}
.btn:disabled{opacity:.45;cursor:default}
.btn.ic{width:32px;padding:0;justify-content:center}
.content{padding:22px 28px 40px;display:flex;flex-direction:column;gap:18px;width:100%;max-width:1500px}
.emg{display:none;align-items:center;gap:10px;flex-wrap:wrap;background:var(--danger);color:#fff;padding:10px 28px;font-weight:700}
.emg.on{display:flex}
.emg .c{background:rgba(255,255,255,.2);border-radius:5px;padding:2px 8px;font-weight:600;font-size:13px}

.tiles{display:grid;grid-template-columns:repeat(auto-fit,minmax(165px,1fr));gap:12px}
.tile{background:var(--bg2);border:1px solid var(--border);border-radius:12px;padding:14px 16px;box-shadow:var(--shadow),var(--hair);cursor:default}
a.tile{color:inherit;cursor:pointer}
.tile .k{font-size:12.5px;color:var(--text2);display:flex;align-items:center;gap:7px}
.tile .k svg{width:15px;height:15px;color:var(--text3)}
.tile .v{font-size:28px;font-weight:700;letter-spacing:-.02em;margin-top:4px;font-variant-numeric:tabular-nums}
.tile .s{font-size:12.5px;color:var(--text3)}
.tile.live{border-color:color-mix(in srgb,var(--accent) 45%,var(--border))}
.tile.live .v{color:var(--accent)}

.card{background:var(--bg2);border:1px solid var(--border);border-radius:12px;box-shadow:var(--shadow),var(--hair);overflow:hidden}
.ch{display:flex;align-items:center;gap:10px;padding:13px 18px;border-bottom:1px solid var(--sep);flex-wrap:wrap}
.ch h2{margin:0;font-size:15px;font-weight:650}
.ch .cnt{font:600 11.5px var(--mono);color:var(--text3)}
.ch a{font-size:12.5px;font-weight:600}
.help{width:22px;height:22px;border-radius:50%;border:1px solid var(--border);display:grid;place-items:center;color:var(--text3);font:700 11px var(--sans);cursor:pointer;background:none;padding:0}
.help[aria-expanded=true]{color:var(--accent2);border-color:var(--accent2)}
.note{padding:12px 18px;font-size:12.5px;color:var(--text2);background:color-mix(in srgb,var(--accent2) 6%,transparent);border-top:1px solid var(--sep);display:flex;gap:10px}
.note[hidden]{display:none}
.note svg{color:var(--accent2);width:16px;height:16px;margin-top:1px}
.empty{padding:16px 18px;color:var(--text3);font-size:13px}
.grid2{display:grid;grid-template-columns:1.35fr 1fr;gap:18px}
.tw{overflow-x:auto}
table{width:100%;border-collapse:collapse}
th{text-align:left;font:600 11px var(--sans);letter-spacing:.04em;text-transform:uppercase;color:var(--text3);padding:9px 18px;background:color-mix(in srgb,var(--text) 2.5%,transparent);white-space:nowrap}
td{padding:10px 18px;border-top:1px solid var(--sep);font-size:13.5px;vertical-align:middle}
td.m,.m{font-family:var(--mono);font-size:12.5px}
.mut{color:var(--text3)}
.num{font-variant-numeric:tabular-nums}
tr.click{cursor:pointer}
tr.click:hover td{background:color-mix(in srgb,var(--text) 2.5%,transparent)}
tr.off td{color:var(--text3)}
.chip{display:inline-flex;align-items:center;gap:5px;font:600 11.5px var(--sans);padding:2px 8px;border-radius:6px;background:var(--bg3);border:1px solid var(--border);color:var(--text2);white-space:nowrap;margin:1px 0}
.chip.a{color:var(--accent);background:color-mix(in srgb,var(--accent) 12%,transparent);border-color:color-mix(in srgb,var(--accent) 28%,transparent)}
.chip.b{color:var(--accent2);background:color-mix(in srgb,var(--accent2) 12%,transparent);border-color:color-mix(in srgb,var(--accent2) 28%,transparent)}
.chip.w{color:var(--warn);background:color-mix(in srgb,var(--warn) 12%,transparent);border-color:color-mix(in srgb,var(--warn) 30%,transparent)}
.chip.d{color:var(--danger);background:color-mix(in srgb,var(--danger) 12%,transparent);border-color:color-mix(in srgb,var(--danger) 30%,transparent)}
.chip.i{color:var(--text3)}
.tg{font:600 12px var(--mono);padding:2px 8px;border-radius:6px;background:var(--bg3);border:1px solid var(--border);display:inline-block;margin:1px 0}
.tg.src{color:var(--accent);border-color:color-mix(in srgb,var(--accent) 35%,transparent);background:color-mix(in srgb,var(--accent) 10%,transparent)}
.st{display:inline-flex;align-items:center;gap:7px;font-weight:600;font-size:13px;white-space:nowrap}
.st i{width:8px;height:8px;border-radius:50%;background:var(--ok)}
.st.w i{background:var(--warn)} .st.d i{background:var(--danger)} .st.i i{background:var(--text3)} .st.i{color:var(--text3)}

.row{display:flex;align-items:center;gap:14px;padding:13px 18px;border-top:1px solid var(--sep)}
.row:first-child{border-top:0}
.ric{width:38px;height:38px;border-radius:10px;display:grid;place-items:center;background:color-mix(in srgb,var(--accent) 14%,transparent);color:var(--accent);flex-shrink:0}
.ric.p{background:color-mix(in srgb,var(--accent2) 14%,transparent);color:var(--accent2)}
.ric.e{background:color-mix(in srgb,var(--danger) 14%,transparent);color:var(--danger)}
.ric.s{background:var(--bg3);color:var(--text2);border:1px solid var(--border)}
.rm{flex:1;min-width:0}
.rt{font-weight:650;font-size:14.5px;display:flex;gap:8px;align-items:center;flex-wrap:wrap}
.rs{font-size:12.5px;color:var(--text2);margin-top:2px;display:flex;gap:6px;flex-wrap:wrap;align-items:center}
.rr{text-align:right;font-size:12.5px;color:var(--text2);flex-shrink:0}
.rr b{font:600 13px var(--mono);color:var(--text2)}
.talk{display:inline-flex;align-items:center;gap:6px;color:var(--accent);font-weight:600}
.bars{display:inline-flex;gap:2px;align-items:flex-end;height:12px}
.bars b{width:3px;background:var(--accent);border-radius:1px;animation:eq 1s ease-in-out infinite}
.bars b:nth-child(2){animation-delay:-.3s}.bars b:nth-child(3){animation-delay:-.6s}.bars b:nth-child(4){animation-delay:-.15s}
@keyframes eq{0%,100%{height:4px}50%{height:12px}}
@media (prefers-reduced-motion:reduce){.bars b{animation:none;height:9px}}
.ev{display:flex;gap:10px;padding:10px 18px;border-top:1px solid var(--sep);align-items:center;font-size:13px;flex-wrap:wrap}
.ev:first-child{border-top:0}
.ev time{font:12px var(--mono);color:var(--text3);min-width:62px}
.arrow{color:var(--text3)}
.ml{margin-left:auto}

.bar{display:flex;gap:10px;align-items:center;flex-wrap:wrap}
.seg{display:inline-flex;background:var(--bg3);border:1px solid var(--border);border-radius:9px;padding:2px;flex-wrap:wrap}
.seg button{padding:5px 12px;border-radius:7px;font-size:12.5px;font-weight:600;color:var(--text2);border:0;background:none;cursor:pointer;min-height:28px}
.seg button[aria-pressed=true]{background:var(--bg2);color:var(--text);box-shadow:0 1px 2px rgba(0,0,0,.2)}
input.f,select.f{height:32px;padding:0 10px;border-radius:8px;border:1px solid var(--border);background:var(--bg3);color:var(--text);font:13px var(--sans);min-width:0}
input.f::placeholder{color:var(--text3)}
.search{position:relative;display:inline-flex;align-items:center}
.search svg{position:absolute;left:9px;width:15px;height:15px;color:var(--text3);pointer-events:none}
.search input{padding-left:30px;width:240px}
.kv{display:grid;grid-template-columns:repeat(auto-fit,minmax(200px,1fr));gap:10px 24px;padding:6px 0 10px}
.kv span.l{display:block;font-size:11px;color:var(--text3);text-transform:uppercase;letter-spacing:.04em;font-weight:600}
tr.exp td{background:var(--bg3);border-top:0}

/* Routing */
.tog{width:44px;height:26px;border-radius:13px;background:var(--bg4);position:relative;flex-shrink:0;border:0;cursor:pointer;padding:0}
.tog::after{content:"";position:absolute;top:2px;left:2px;width:22px;height:22px;border-radius:50%;background:#fff;box-shadow:0 1px 3px rgba(0,0,0,.35);transition:left .15s}
.tog[aria-checked=true]{background:var(--ok)} .tog[aria-checked=true]::after{left:20px}
.tog:disabled{opacity:.5;cursor:default}
@media (prefers-reduced-motion:reduce){.tog::after{transition:none}}
.pool .flow{display:flex;gap:6px;align-items:center;flex-wrap:wrap;margin-top:6px}
.pool.offp .rt{color:var(--text3)} .pool.offp .flow{opacity:.6}
.chk{width:18px;height:18px;accent-color:var(--accent);cursor:pointer}
.selbar{display:flex;gap:10px;align-items:center;padding:12px 18px;border-top:1px solid var(--sep);background:color-mix(in srgb,var(--accent) 6%,transparent);flex-wrap:wrap}
.msg{font-size:12.5px;padding:0 18px 12px}
.msg.err{color:var(--danger)} .msg.ok{color:var(--ok)}

/* Sheet (pool editor) */
.scrim{position:fixed;inset:0;background:rgba(0,0,0,.35);display:none;align-items:center;justify-content:center;z-index:50;padding:16px}
.scrim.on{display:flex}
.sheet{background:var(--bg2);border:1px solid var(--border);border-radius:14px;box-shadow:0 28px 64px -22px rgba(0,0,0,.5);width:min(520px,100%);max-height:90vh;overflow:auto}
.sheet h3{margin:0;font-size:17px}
.sheet .sb{padding:16px 20px;display:flex;flex-direction:column;gap:14px}
.sheet label.l{font-size:12px;font-weight:600;color:var(--text2);display:block;margin-bottom:5px}
.sheet .acts{display:flex;gap:10px;justify-content:flex-end;padding:14px 20px;border-top:1px solid var(--sep)}
.sheet input.f{width:100%}

iframe.embed{width:100%;border:0;min-height:calc(100vh - 130px);background:transparent}

/* Phone */
.tabbar{display:none}
@media (max-width:760px){
  #sb{position:fixed;left:0;top:0;transform:translateX(-100%);transition:transform .2s;box-shadow:var(--shadow)}
  body.sbopen #sb{transform:none}
  .top{padding:12px 16px}
  .top h1{font-size:26px}
  .hide-s{display:none!important}
  .content{padding:14px 16px 100px;gap:14px}
  .tiles{grid-template-columns:1fr 1fr;gap:10px}
  .grid2{grid-template-columns:1fr}
  th,td{padding-left:12px;padding-right:12px}
  .search input{width:100%}
  .tabbar{display:flex;position:fixed;left:12px;right:12px;bottom:calc(14px + env(safe-area-inset-bottom));height:62px;border-radius:31px;z-index:30;
    background:color-mix(in srgb,var(--bg2) 76%,transparent);-webkit-backdrop-filter:blur(20px) saturate(1.6);backdrop-filter:blur(20px) saturate(1.6);border:1px solid var(--border);box-shadow:var(--shadow);justify-content:space-around;align-items:center}
  .tab{display:flex;flex-direction:column;align-items:center;gap:2px;font-size:10.5px;color:var(--text3);font-weight:600;min-width:56px;min-height:44px;justify-content:center;border:0;background:none;cursor:pointer}
  .tab svg{width:22px;height:22px}
  .tab.on{color:var(--accent)}
}
@media (max-width:760px) and (prefers-reduced-transparency:reduce){.tabbar{background:var(--bg2)}}
</style></head>
<body>
<aside id="sb" aria-label="Navigation">
  <div class="brand">
    <div class="brand-row">
      <div class="logo" aria-hidden="true"><svg class="i" viewBox="0 0 24 24"><path d="M12 11v10M8 21h8M9.5 21l2.5-10 2.5 10"/><circle cx="12" cy="9" r="1.4"/><path d="M7.8 5.8a6 6 0 0 0 0 6.4M16.2 5.8a6 6 0 0 1 0 6.4M5 3.5a10 10 0 0 0 0 11M19 3.5a10 10 0 0 1 0 11"/></svg></div>
      <div><div class="bname">__SERVERNAME__</div><div class="bsub">brew-server v__VERSION__</div></div>
    </div>
    <div class="hw">
      <div class="hw-row" data-go="stations"><div class="hw-g" aria-hidden="true"><svg class="i" viewBox="0 0 24 24"><path d="M4 12h4l3-8 4 16 3-8h2"/></svg></div><div><div class="hw-k">Brew</div><div class="hw-v" id="hw-brew">…</div></div><span class="dot i" id="hw-brew-dot"></span></div>
      <div class="hw-row" data-go="ha"><div class="hw-g b" aria-hidden="true"><svg class="i" viewBox="0 0 24 24"><path d="M12 3l8 4v5c0 5-3.5 8-8 9-4.5-1-8-4-8-9V7z"/></svg></div><div><div class="hw-k">HA</div><div class="hw-v" id="hw-ha">…</div></div><span class="dot i" id="hw-ha-dot"></span></div>
    </div>
  </div>
  <nav id="nav"></nav>
  <div class="sbfoot">
    <div class="acct"><span id="acct"></span><button class="btn" id="loginbtn" hidden onclick="doLogin()">Log in</button><button class="btn" id="logoutbtn" hidden onclick="doLogout()">Log out</button></div>
    <a href="/classic">Classic dashboard</a><br>
    brew-server by ysamouhos<br>
    Redesign + extra features: <a href="https://github.com/sebastianmadl" target="_blank" rel="noopener">OE4SXM</a>
  </div>
</aside>

<main>
  <div class="emg" id="emg" role="alert"><span>EMERGENCY ACTIVE</span><span id="emg-list" style="display:flex;gap:8px;flex-wrap:wrap"></span></div>
  <div class="top">
    <h1 id="title">Overview</h1>
    <span class="pill" id="live"><i></i><span>Live</span></span>
    <span class="sp"></span>
  </div>
  <div class="content" id="content"></div>
</main>

<div class="tabbar" role="tablist" aria-label="Pages">
  <button class="tab" data-tab="overview"></button>
  <button class="tab" data-tab="calls"></button>
  <button class="tab" data-tab="stations"></button>
  <button class="tab" data-tab="routing"></button>
  <button class="tab" data-tab="more"></button>
</div>

<div class="scrim" id="scrim" onclick="if(event.target===this)closeSheet()"><div class="sheet" role="dialog" aria-modal="true" aria-labelledby="sheet-title" id="sheet"></div></div>

<script>
'use strict';
const I={
 home:'<path d="M3 11l9-7 9 7v9a1 1 0 0 1-1 1h-5v-6h-6v6H4a1 1 0 0 1-1-1z"/>',
 call:'<path d="M5 4h4l2 5-2.5 1.5a11 11 0 0 0 5 5L15 13l5 2v4a2 2 0 0 1-2 2A16 16 0 0 1 3 6a2 2 0 0 1 2-2"/>',
 sds:'<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/>',
 radios:'<circle cx="9" cy="8" r="3.5"/><path d="M2.5 20a6.5 6.5 0 0 1 13 0M16 4.5a3.5 3.5 0 0 1 0 7M21.5 20a6.5 6.5 0 0 0-4-6"/>',
 map:'<path d="M9 4L3 6v14l6-2 6 2 6-2V4l-6 2zM9 4v14M15 6v14"/>',
 bts:'<path d="M12 11v10M8 21h8M9.5 21l2.5-10 2.5 10"/><circle cx="12" cy="9" r="1.4"/><path d="M7.8 5.8a6 6 0 0 0 0 6.4M16.2 5.8a6 6 0 0 1 0 6.4"/>',
 routing:'<circle cx="5" cy="6" r="2"/><circle cx="5" cy="18" r="2"/><circle cx="19" cy="12" r="2"/><path d="M7 6h3a4 4 0 0 1 4 4 2 2 0 0 0 2 2h1M7 18h3a4 4 0 0 0 4-4"/>',
 roam:'<path d="M7 4L3 8l4 4M3 8h13a5 5 0 0 1 0 10h-2M17 20l4-4-4-4M21 16H8"/>',
 tele:'<path d="M3 12h4l3-7 4 14 3-7h4"/>',
 ctl:'<circle cx="12" cy="12" r="3"/><path d="M12 2v3M12 19v3M2 12h3M19 12h3M5 5l2 2M17 17l2 2M5 19l2-2M17 7l2-2"/>',
 sip:'<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M7 10h4M7 14h7M16 10h1"/>',
 sipcfg:'<path d="M4 6h10M4 12h6M4 18h10"/><circle cx="17" cy="6" r="2"/><circle cx="13" cy="12" r="2"/><circle cx="17" cy="18" r="2"/>',
 sms:'<path d="M4 6h16v12H4z"/><path d="M4 6l8 7 8-7"/>',
 tsds:'<path d="M4 4h16v16H4z"/><path d="M8 9h8M8 13h8M8 17h5"/>',
 ha:'<path d="M12 3l8 4v5c0 5-3.5 8-8 9-4.5-1-8-4-8-9V7z"/>',
 set:'<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-2.9 1.2V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-2.9-1.2l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1A1.7 1.7 0 0 0 3 14H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.2-2.9l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1A1.7 1.7 0 0 0 10 3.1V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 2.9 1.2l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0 1.2 2.9H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>',
 lock:'<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/>',
 info:'<circle cx="12" cy="12" r="9"/><path d="M12 11v6M12 7.5v.5"/>',
 more:'<circle cx="5" cy="12" r="1.5"/><circle cx="12" cy="12" r="1.5"/><circle cx="19" cy="12" r="1.5"/>',
 plus:'<path d="M12 5v14M5 12h14"/>',
 search:'<circle cx="11" cy="11" r="7"/><path d="M20 20l-3.5-3.5"/>',
 alert:'<path d="M12 3l10 18H2z"/><path d="M12 10v5M12 18v.5"/>'
};
const ic=(n,cls)=>`<svg class="i${cls?' '+cls:''}" viewBox="0 0 24 24" aria-hidden="true">${I[n]}</svg>`;
// Pages: [id, title, icon, group, embedded classic page or null, admin only]
const PAGES=[
 ['overview','Overview','home','Monitor'],['calls','Calls','call','Monitor'],['sds','SDS','sds','Monitor'],
 ['radios','Radios','radios','Monitor'],['map','Map','map','Monitor','/map'],
 ['stations','Stations & Cells','bts','Network'],['routing','Routing','routing','Network'],['roaming','Roaming','roam','Network'],
 ['telemetry','Telemetry','tele','Network'],['control','Control','ctl','Network',null,true],
 ['sip','SIP / VoIP','sip','Integrations','/sip'],['sipcfg','SIP Config','sipcfg','Integrations','/sip-config'],
 ['sms','SMS Center','sms','Integrations','/sms-center'],['tsds','Telemetry SDS','tsds','Integrations','/telemetry-sds'],
 ['ha','High Availability','ha','System','/ha'],['settings','Settings','set','System','/settings',true]
];
const PG=Object.fromEntries(PAGES.map(p=>[p[0],p]));
const $=id=>document.getElementById(id);
const esc=s=>String(s??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const tm=ms=>ms?new Date(ms).toLocaleTimeString([], {hour:'2-digit',minute:'2-digit',second:'2-digit'}):'';
const tmd=ms=>{if(!ms)return '';const d=new Date(ms),n=new Date();return d.toDateString()===n.toDateString()?tm(ms):d.toLocaleDateString()+' '+tm(ms);};
function dur(ms){const s=Math.max(0,Math.floor(ms/1000));const h=Math.floor(s/3600),m=Math.floor(s%3600/60),x=s%60;return (h?h+':'+String(m).padStart(2,'0'):String(m).padStart(2,'0'))+':'+String(x).padStart(2,'0');}
function ago(ms){if(!ms)return '—';const s=Math.max(0,(Date.now()-ms)/1000);if(s<60)return Math.floor(s)+' s';if(s<3600)return Math.floor(s/60)+' min';if(s<86400)return Math.floor(s/3600)+' h '+Math.floor(s%3600/60)+' min';return Math.floor(s/86400)+' d';}
const mhz=hz=>hz?(hz/1e6).toFixed(5).replace(/0+$/,'').replace(/\.$/,'')+' MHz':'—';
const isToday=ms=>new Date(ms).toDateString()===new Date().toDateString();
async function getJ(u){const r=await fetch(u,{cache:'no-store'});if(!r.ok)throw new Error(r.status+' '+(await r.text()));return r.json();}
async function postJ(u,body){const r=await fetch(u,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)});const t=await r.text();if(!r.ok)throw new Error(t||r.status);try{return JSON.parse(t)}catch(e){return t}}
function doLogin(){location.href='/login?next='+encodeURIComponent(location.pathname+location.hash);}
function doLogout(){location.href=location.protocol+'//logout:'+Date.now()+'@'+location.host+location.pathname;}

const D={status:null,conn:null,tele:[],pools:[],radios:[],who:{admin:true},ctl:[],emg:[],ha:null,regs:[]};
const UI={page:'overview',open:{},filter:{},sel:new Set(),help:{},msg:{}};
// Messages survive the periodic re-render (kept in UI.msg, drawn by msgDiv).
function setMsg(id,cls,text){UI.msg[id]=[cls,text];const e=$(id);if(e){e.className='msg '+cls;e.textContent=text;}}
function msgDiv(id,style){const m=UI.msg[id]||['',''];return `<div class="msg ${m[0]}" id="${id}" role="status"${style?` style="${style}"`:''}>${esc(m[1])}</div>`;}
let ok=true;

function buildNav(){
  let h='',g='';
  for(const [id,t,icn,grp,emb,adm] of PAGES){
    if(grp!==g){h+=`<div class="grp">${grp}</div>`;g=grp;}
    h+=`<button class="nv" data-go="${id}">${ic(icn)}<span>${t}</span>${adm?`<span class="lock" title="Admins only">${ic('lock')}<span class="sr">admins only</span></span>`:''}<span class="badge${id==='sms'?' w':''}" id="b-${id}"></span></button>`;
  }
  $('nav').innerHTML=h;
  document.querySelectorAll('.tab').forEach(t=>{const id=t.dataset.tab;const p=id==='more'?['more','More','more']:PG[id];t.innerHTML=ic(p[2])+'<span>'+(id==='stations'?'Stations':p[1])+'</span>';});
}
document.addEventListener('click',e=>{
  const g=e.target.closest('[data-go]');if(g){go(g.dataset.go);return;}
  const t=e.target.closest('[data-tab]');if(t){if(t.dataset.tab==='more')document.body.classList.toggle('sbopen');else go(t.dataset.tab);return;}
  if(document.body.classList.contains('sbopen')&&!e.target.closest('#sb'))document.body.classList.remove('sbopen');
});
function go(p){if(!PG[p])p='overview';if(location.hash!=='#'+p)location.hash=p;else show(p);}
window.addEventListener('hashchange',()=>show(location.hash.slice(1)));
function show(p){
  if(!PG[p])p='overview';UI.page=p;document.body.classList.remove('sbopen');
  document.querySelectorAll('.nv').forEach(n=>n.classList.toggle('on',n.dataset.go===p));
  document.querySelectorAll('.tab').forEach(n=>n.classList.toggle('on',n.dataset.tab===p));
  $('title').textContent=PG[p][1];
  document.title=PG[p][1]+' · '+document.querySelector('.bname').textContent;
  const emb=PG[p][4];
  if(emb){$('content').innerHTML=`<iframe class="embed" title="${esc(PG[p][1])}" src="${emb}?embed=1"></iframe>`;fitFrame();}
  else render(true);
  $('content').parentElement.scrollTop=0;
}
function fitFrame(){const f=document.querySelector('iframe.embed');if(!f)return;f.onload=()=>{try{const d=f.contentDocument;const fit=()=>{f.style.height=Math.max(window.innerHeight-130,d.documentElement.scrollHeight+20)+'px';};fit();new ResizeObserver(fit).observe(d.body);}catch(e){}};}
function helpBtn(key){return `<button class="help" aria-expanded="${!!UI.help[key]}" aria-controls="h-${key}" onclick="UI.help['${key}']=!UI.help['${key}'];render(true)" title="What is this?">?<span class="sr">Explain</span></button>`;}
function helpNote(key,text){return `<div class="note" id="h-${key}" ${UI.help[key]?'':'hidden'}>${ic('info')}<div>${text}</div></div>`;}

/* ---------- data ---------- */
async function refreshFast(){
  try{
    const [st,pools,emg]=await Promise.all([getJ('/api/status'),getJ('/api/pools'),getJ('/api/emergencies')]);
    D.status=st;D.pools=pools;D.emg=emg;ok=true;
  }catch(e){ok=false;}
  $('live').className='pill'+(ok?'':' d');$('live').lastChild.textContent=ok?'Live':'Offline';
  $('emg').classList.toggle('on',D.emg.length>0);
  $('emg-list').innerHTML=D.emg.map(e=>`<span class="c">ISSI ${e.issi}${e.destination!=null?' → '+e.destination:''}${e.bts?' · '+esc(e.bts):''}</span>`).join('');
  render();
}
async function refreshSlow(){
  const tasks=[getJ('/api/connections').then(v=>D.conn=v),getJ('/api/telemetry').then(v=>D.tele=v),getJ('/api/radios').then(v=>D.radios=v),
    getJ('/api/ha').then(v=>D.ha=v),getJ('/api/whoami').then(v=>D.who=v)];
  if(UI.page==='radios')tasks.push(getJ('/api/registrations').then(v=>D.regs=v));
  if(D.who.admin)tasks.push(getJ('/api/control').then(v=>D.ctl=v).catch(()=>{}));
  await Promise.allSettled(tasks);
  chrome();render();
}
function chrome(){
  const c=D.conn,st=D.status;
  const bs=c?c.brew_clients.filter(x=>x.mode==='Basestation').length:0;
  $('hw-brew').textContent=c?`${bs} station${bs===1?'':'s'}`:'…';
  $('hw-brew-dot').className='dot'+(ok?(bs?'':' w'):' d');
  const ha=D.ha;
  if(ha){$('hw-ha').textContent=ha.enabled?(ha.role[0].toUpperCase()+ha.role.slice(1)+(ha.vip_held?' · VIP held':'')):'Off (single node)';$('hw-ha-dot').className='dot'+(ha.enabled?(ha.role==='active'?'':' w'):' i');}
  const w=D.who;
  document.body.classList.toggle('ro',!w.admin);
  $('acct').textContent=w.username?('Signed in as '+w.username):(w.admin?'':'Read only');
  $('loginbtn').hidden=!(w.can_login&&!w.username);$('logoutbtn').hidden=!w.username;
  if(st){$('b-calls').textContent=st.active_calls.filter(x=>!legIds().has(x.uuid)).length||'';}
  $('b-radios').textContent=(c&&c.mobile_stations.length)||'';
  $('b-routing').textContent=D.pools.filter(p=>p.enabled).length||'';
}

/* ---------- pools ---------- */
function legIds(){
  // Leg calls: group calls on a running pool's legs from the same talker.
  const out=new Set();if(!D.status)return out;
  for(const p of D.pools){const l=p.live;if(!l)continue;
    for(const c of D.status.active_calls){if(c.kind==='group'&&c.source===l.talker&&l.legs.includes(c.destination))out.add(c.uuid);}}
  return out;
}
function poolOf(call){return D.pools.find(p=>p.live&&p.live.gssi===call.destination&&p.live.talker===call.source);}

/* ---------- render ---------- */
let lastSig='';
function render(force){
  const p=UI.page;if(PG[p][4])return;
  // Keep focus and typing alive: pages with inputs only re-render when forced or nothing is focused inside.
  const a=document.activeElement;
  if(!force&&a&&$('content').contains(a)&&(a.tagName==='INPUT'||a.tagName==='SELECT'||a.tagName==='TEXTAREA'))return;
  const fn=R[p];if(!fn)return;
  const html=fn();
  if(force||html!==lastSig){$('content').innerHTML=html;lastSig=html;}
}
const R={};
const callRow=(c,now)=>{
  const pool=poolOf(c);
  const grp=c.kind==='group';
  const em=c.priority>=15;
  const legs=pool?pool.live.legs.map(g=>`<span class="tg">${g}</span>`).join(' '):'';
  const wait=pool&&pool.live.waiting.length?` <span class="mut">waiting: ${pool.live.waiting.join(', ')}</span>`:'';
  return `<div class="row"><div class="ric${em?' e':grp?'':' p'}">${ic(grp?'radios':'call')}</div><div class="rm">
   <div class="rt">${grp?'TG '+c.destination:c.source+' → '+c.destination} <span class="chip ${em?'d':grp?'a':'b'}">${em?'Emergency':grp?'Group':'Individual'}</span>${pool?`<span class="chip">${ic('routing')} ${esc(pool.name)}</span>`:''}</div>
   <div class="rs">${grp?`<span class="talk"><span class="bars"><b></b><b></b><b></b><b></b></span>${c.source} talking</span>`:''}${pool?`<span class="mut">also on</span> ${legs}${wait}`:''}</div></div>
   <div class="rr"><b>${dur(now-c.started_at_ms)}</b><br>${c.voice_frames} frames · prio ${c.priority}</div></div>`;
};
function stationsList(){
  const c=D.conn;if(!c)return [];
  const byStation={};
  for(const x of c.cells||[]){(byStation[x.station]=byStation[x.station]||{name:x.station,cells:[],online:false,regs:0,addr:'',since:0,gone:0}).cells.push(x);
    const s=byStation[x.station];if(x.online!==false){s.online=true;s.regs=x.registered_issis;s.addr=x.remote_addr;s.since=x.connected_at_ms;}else s.gone=x.last_seen_ms;}
  for(const b of c.brew_clients.filter(b=>b.mode==='Basestation')){const s=byStation[b.station]||(byStation[b.station]={name:b.station,cells:[],online:true,regs:b.registered_issis,addr:b.remote_addr,since:b.connected_at_ms,gone:0});s.online=true;s.regs=b.registered_issis;s.since=b.connected_at_ms;s.addr=b.remote_addr;s.ver=b.version;}
  for(const t of D.tele){const m=Object.values(byStation).find(s=>s.addr&&t.ip&&s.addr.startsWith(t.ip+':')&&!s.tele);if(m&&Object.values(byStation).filter(s=>s.addr&&s.addr.startsWith(t.ip+':')).length===1)m.tele=t;}
  return Object.values(byStation).sort((a,b)=>(b.online-a.online)||a.name.localeCompare(b.name));
}
R.overview=()=>{
  const st=D.status,c=D.conn,now=Date.now();
  if(!st)return '<div class="empty">Loading…</div>';
  const legs=legIds();
  const calls=st.active_calls.filter(x=>!legs.has(x.uuid));
  const stations=stationsList();
  const on=stations.filter(s=>s.online).length,off=stations.length-on;
  const cells=c?(c.cells||[]).filter(x=>x.online!==false).length:0;
  const roam=c?c.roaming_log||[]:[];
  const roamToday=roam.filter(r=>isToday(r.at_ms));
  const radios=c?c.mobile_stations.length:0;
  const grpCalls=calls.filter(x=>x.kind==='group').length;
  const sdsToday=st.recent_sds.filter(s=>isToday(s.at_ms)).length;
  return `<div class="tiles">
   <a class="tile" data-go="stations"><div class="k">${ic('bts')}Stations</div><div class="v">${on}</div><div class="s">${cells} cell${cells===1?'':'s'}${off?' · '+off+' offline':''}</div></a>
   <a class="tile" data-go="radios"><div class="k">${ic('radios')}Radios</div><div class="v">${radios}</div><div class="s">registered now</div></a>
   <a class="tile${calls.length?' live':''}" data-go="calls"><div class="k">${ic('call')}Active calls</div><div class="v">${calls.length}</div><div class="s">${grpCalls} group · ${calls.length-grpCalls} individual</div></a>
   <div class="tile"><div class="k">${ic('radios')}Groups</div><div class="v">${st.groups}</div><div class="s">with affiliations</div></div>
   <a class="tile" data-go="roaming"><div class="k">${ic('roam')}Cell changes</div><div class="v">${roamToday.length}</div><div class="s">today · ${roamToday.filter(r=>r.kind==='handover'||String(r.reselection||'').startsWith('announced')).length} announced</div></a>
   <a class="tile" data-go="sds"><div class="k">${ic('sds')}SDS</div><div class="v">${sdsToday}</div><div class="s">today · ${st.total_sds} total</div></a>
  </div>
  <div class="grid2">
   <div class="card"><div class="ch"><h2>Live calls</h2><span class="cnt">${calls.length}</span><span class="sp"></span><a data-go="calls" href="#calls">All calls ›</a></div>
    ${calls.length?calls.map(x=>callRow(x,now)).join(''):'<div class="empty">No active calls</div>'}</div>
   <div class="card"><div class="ch"><h2>Stations</h2><span class="cnt">${stations.length}</span><span class="sp"></span><a data-go="stations" href="#stations">Cells ›</a></div>
    ${stations.length?stations.map(s=>`<div class="row"><div class="ric s">${ic('bts')}</div><div class="rm"><div class="rt">${esc(s.name)}</div><div class="rs">${s.cells.length} cell${s.cells.length===1?'':'s'}${s.cells.length?' · '+s.cells.map(x=>x.carrier).join(', '):''}${s.tele&&s.tele.version?' · FlowStation '+esc(s.tele.version):''}</div></div>
      <div class="rr">${s.online?'<span class="st"><i></i>Online</span>':'<span class="st i"><i></i>Offline</span>'}<br>${s.online?s.regs+' radio'+(s.regs===1?'':'s'):(s.gone?'gone '+ago(s.gone):'')}</div></div>`).join(''):'<div class="empty">No stations connected</div>'}</div>
  </div>
  <div class="grid2">
   <div class="card"><div class="ch"><h2>Recent cell changes</h2><span class="sp"></span><a data-go="roaming" href="#roaming">Roaming log ›</a></div>
    ${roam.length?roam.slice(0,5).map(r=>`<div class="ev"><time>${tm(r.at_ms)}</time><b class="m">${r.issi}</b>${place(r.from)}<span class="arrow">→</span>${place(r.to)}<span class="ml">${howOf(r)}</span></div>`).join(''):'<div class="empty">No cell changes yet</div>'}</div>
   <div class="card"><div class="ch"><h2>Recent SDS</h2><span class="sp"></span><a data-go="sds" href="#sds">All SDS ›</a></div>
    ${st.recent_sds.length?st.recent_sds.slice(0,5).map(s=>`<div class="ev"><time>${tm(s.at_ms)}</time><b class="m">${s.source}</b><span class="arrow">→</span><b class="m">${s.destination}</b><span class="ml mut">${s.reports?s.reports+' report'+(s.reports===1?'':'s'):''}</span></div>`).join(''):'<div class="empty">No SDS yet</div>'}</div>
  </div>`;
};
const RESEL={undeclared:['i','undeclared'],unannounced:['w','unannounced'],announced_type_1:['a','announced · type 1'],announced_type_2:['a','announced · type 2'],announced_type_3:['a','announced · type 3']};
function howOf(r){const t=RESEL[r.reselection];
  const where={handover:'<span class="chip b">prepared here</span>',within_station:'<span class="chip i">within station</span>'};
  if(!t)return r.kind==='handover'?'<span class="chip a">announced</span>':r.kind==='within_station'?where.within_station:'<span class="chip w">not announced</span>';
  return `<span class="chip ${t[0]}">${t[1]}</span>`+(where[r.kind]&&r.kind!=='within_station'?' '+where[r.kind]:(r.kind==='within_station'?' '+where.within_station:''));}
function place(p){return p.cell?`<span class="chip" title="carrier ${p.carrier??''}">${esc(p.cell)}</span>`:`<span class="chip">${esc(p.station)}</span>`;}

function seg(key,opts){const v=UI.filter[key]||opts[0][0];return `<div class="seg" role="group">${opts.map(([k,l])=>`<button aria-pressed="${v===k}" onclick="UI.filter['${key}']='${k}';render(true)">${l}</button>`).join('')}</div>`;}
function searchBox(key,ph){return `<label class="search">${ic('search')}<span class="sr">${ph}</span><input class="f" type="search" placeholder="${ph}" value="${esc(UI.filter[key]||'')}" oninput="UI.filter['${key}']=this.value;clearTimeout(window._st);window._st=setTimeout(()=>{render(true);const e=document.querySelector('.search input');if(e){e.focus();e.setSelectionRange(e.value.length,e.value.length)}},250)"></label>`;}
const match=(q,...v)=>!q||v.some(x=>String(x??'').toLowerCase().includes(q.toLowerCase()));

R.calls=()=>{
  const st=D.status,now=Date.now();if(!st)return '<div class="empty">Loading…</div>';
  const legs=legIds();const q=UI.filter.calls||'';const k=UI.filter.callk||'all';
  const act=st.active_calls.filter(x=>!legs.has(x.uuid));
  const rec=st.recent_calls.filter(c=>(k==='all'||c.kind===k)&&match(q,c.source,c.destination));
  return `<div class="card"><div class="ch"><h2>Live calls</h2><span class="cnt">${act.length}</span></div>${act.length?act.map(x=>callRow(x,now)).join(''):'<div class="empty">No active calls</div>'}</div>
  <div class="bar">${seg('callk',[['all','All'],['group','Group'],['private','Individual']])}${searchBox('calls','ISSI or group…')}<span class="sp"></span><span class="mut" style="font-size:12.5px">${st.total_calls} calls in total</span></div>
  <div class="card"><div class="ch"><h2>Recent calls</h2><span class="cnt">${rec.length}</span></div><div class="tw"><table><thead><tr><th>Started</th><th>Type</th><th>From</th><th>To</th><th>Duration</th><th>Prio</th><th>Voice frames</th></tr></thead><tbody>
   ${rec.slice(0,300).map(c=>`<tr><td class="m">${tmd(c.started_at_ms)}</td><td>${c.kind==='group'?'<span class="chip a">Group</span>':'<span class="chip b">Individual</span>'}</td><td class="m">${c.source}</td><td class="m">${c.destination}</td><td class="m">${c.ended_at_ms?dur(c.ended_at_ms-c.started_at_ms):'—'}</td><td class="num">${c.priority}</td><td class="num">${c.voice_frames}</td></tr>`).join('')||'<tr><td colspan="7" class="mut">No calls yet</td></tr>'}
  </tbody></table></div></div>`;
};
R.sds=()=>{
  const st=D.status;if(!st)return '<div class="empty">Loading…</div>';
  const q=UI.filter.sds||'';const list=st.recent_sds.filter(s=>match(q,s.source,s.destination));
  return `<div class="bar">${searchBox('sds','ISSI…')}<span class="sp"></span><span class="mut" style="font-size:12.5px">${st.total_sds} SDS in total</span></div>
  <div class="card"><div class="ch"><h2>Recent SDS</h2><span class="cnt">${list.length}</span></div><div class="tw"><table><thead><tr><th>Time</th><th>From</th><th>To</th><th>Reports</th></tr></thead><tbody>
  ${list.map(s=>`<tr><td class="m">${tmd(s.at_ms)}</td><td class="m">${s.source}</td><td class="m">${s.destination}</td><td class="num">${s.reports}</td></tr>`).join('')||'<tr><td colspan="4" class="mut">No SDS yet</td></tr>'}</tbody></table></div></div>`;
};
R.radios=()=>{
  const c=D.conn;if(!c)return '<div class="empty">Loading…</div>';
  const q=UI.filter.radios||'';
  const ms=c.mobile_stations.filter(m=>match(q,m.issi,m.basestation_name,(m.groups||[]).join(' '),m.cell&&m.cell.name));
  const regs=(D.regs||[]).filter(r=>match(q,r.issi,r.bts));
  return `<div class="bar">${searchBox('radios','ISSI, group or station…')}</div>
  <div class="card"><div class="ch"><h2>Registered radios</h2><span class="cnt">${ms.length}</span></div><div class="tw"><table><thead><tr><th>ISSI</th><th>Station / cell</th><th>Groups</th><th>Via</th><th>Station address</th></tr></thead><tbody>
  ${ms.sort((a,b)=>a.issi-b.issi).map(m=>`<tr><td><b class="m">${m.issi}</b></td><td>${m.cell?`<span class="chip">${esc(m.cell.name)}</span>`:`<span class="chip">${esc(m.basestation_name||'—')}</span>`}</td><td>${(m.groups||[]).map(g=>`<span class="tg">${g}</span>`).join(' ')||'<span class="mut">—</span>'}</td><td>${esc(m.mode)}</td><td class="m mut">${esc(m.basestation_addr||'')}</td></tr>`).join('')||'<tr><td colspan="5" class="mut">No radios registered</td></tr>'}
  </tbody></table></div></div>
  <div class="card"><div class="ch"><h2>Registration log</h2><span class="cnt">${regs.length}</span></div><div class="tw"><table><thead><tr><th>Time</th><th>ISSI</th><th>Event</th><th>Station</th></tr></thead><tbody>
  ${regs.slice(0,300).map(r=>`<tr><td class="m">${tmd(r.at_ms)}</td><td class="m">${r.issi}</td><td>${r.kind==='register'?'<span class="chip a">register</span>':r.kind==='timeout'?'<span class="chip w">timeout</span>':`<span class="chip i">${esc(r.kind)}</span>`}</td><td>${esc(r.bts||'')}${r.cell?' <span class="chip">'+esc(r.cell)+'</span>':''}</td></tr>`).join('')||'<tr><td colspan="4" class="mut">No registrations yet</td></tr>'}
  </tbody></table></div></div>`;
};
R.stations=()=>{
  const c=D.conn;if(!c)return '<div class="empty">Loading…</div>';
  const f=UI.filter.cellf||'all';
  const cells=(c.cells||[]).filter(x=>f==='all'||(f==='on')===(x.online!==false));
  const dups=c.duplicate_carriers||[];
  const caps=x=>(x.capabilities||[]).map(k=>`<span class="chip">${esc(k)}</span>`).join(' ')||'<span class="mut">—</span>';
  return `<div class="bar">${seg('cellf',[['all','All'],['on','Online'],['off','Offline']])}</div>
  ${dups.length?`<div class="card"><div class="note">${ic('alert')}<div><b>Duplicate carriers:</b> ${dups.map(d=>esc(JSON.stringify(d))).join(', ')}. The newest announcement serves.</div></div></div>`:''}
  <div class="card"><div class="ch"><h2>Cells</h2><span class="cnt">${cells.length}</span>${helpBtn('cells')}</div><div class="tw"><table><thead><tr><th>Cell</th><th>Status</th><th>Carrier</th><th>Downlink</th><th>LA</th><th>MCC / MNC</th><th>Neighbours</th><th>Radios</th></tr></thead><tbody>
  ${cells.map(x=>{const key='c'+x.name;const open=UI.open[key];const offc=x.online===false;
   return `<tr class="click${offc?' off':''}" onclick="UI.open['${esc(key)}']=!UI.open['${esc(key)}'];render(true)" aria-expanded="${!!open}"><td><b>${esc(x.name)}</b> ${x.cell===0&&!offc?'<span class="chip i">primary</span>':''}${x.duplicate?' <span class="chip w">duplicate</span>':''}</td>
   <td>${offc?'<span class="st i"><i></i>Offline</span>':'<span class="st"><i></i>Online</span>'}</td><td class="m">${x.carrier}</td><td class="num">${mhz(x.downlink_hz)}</td><td>${x.location_area}</td><td class="num">${x.mcc} / ${x.mnc}</td>
   <td>${x.neighbours?(x.neighbours.map(n=>`<span class="chip">${esc(n)}</span>`).join(' ')||'<span class="mut">none</span>'):'<span class="mut">—</span>'}</td><td class="num">${offc?'—':x.registered_issis}</td></tr>
   ${open?`<tr class="exp"><td colspan="8"><div class="kv"><div><span class="l">Station address</span>${offc?'—':`<span class="m">${esc(x.remote_addr||'—')}</span>`}</div><div><span class="l">${offc?'Gone':'Connected'}</span>${offc?(x.last_seen_ms?ago(x.last_seen_ms)+' ago':'—'):ago(x.connected_at_ms)}</div>
    <div><span class="l">Extensions</span>${caps(x)}</div><div><span class="l">Access level / MS power</span>${x.rxlev_access_min_dbm!=null?x.rxlev_access_min_dbm+' dBm':'—'} · ${x.ms_txpwr_max_dbm!=null?x.ms_txpwr_max_dbm+' dBm':'—'}</div></div></td></tr>`:''}`;}).join('')||'<tr><td colspan="8" class="mut">No cells announced</td></tr>'}
  </tbody></table></div>${helpNote('cells','Cells the stations announce (FlowStation with the <code>bts-info</code> extension), named <i>login/cell</i>: the Brew login of the station and the cell number in it (0 = primary). On the air a cell is identified by MCC, MNC, carrier and band plan. Click a row for details. A station that went away stays listed as offline until it connects again (stations with a login only).')}</div>
  <div class="card"><div class="ch"><h2>Brew connections</h2><span class="cnt">${c.brew_clients.length}</span></div><div class="tw"><table><thead><tr><th>Station</th><th>Mode</th><th>Version</th><th>Remote address</th><th>Connected</th><th>Radios</th><th>Cells</th></tr></thead><tbody>
  ${c.brew_clients.map(b=>`<tr><td><b>${esc(b.station)}</b></td><td>${esc(b.mode)}</td><td class="num">v${b.version}</td><td class="m">${esc(b.remote_addr||'—')}</td><td>${ago(b.connected_at_ms)}</td><td class="num">${b.registered_issis}</td><td>${(b.cells||[]).map(x=>`<span class="chip">${x.carrier}</span>`).join(' ')||'<span class="mut">—</span>'}</td></tr>`).join('')||'<tr><td colspan="7" class="mut">No Brew connections</td></tr>'}
  </tbody></table></div></div>
  ${c.sip&&c.sip.enabled?'':''}`;
};
R.roaming=()=>{
  const c=D.conn;if(!c)return '<div class="empty">Loading…</div>';
  const all=c.roaming_log||[];const f=UI.filter.roamf||'all';const q=UI.filter.roam||'';
  const ann=r=>r.kind==='handover'||String(r.reselection||'').startsWith('announced');
  const list=all.filter(r=>(f==='all'||(f==='ann'&&ann(r))||(f==='unann'&&!ann(r)&&r.kind!=='within_station')||(f==='within'&&r.kind==='within_station'))&&match(q,r.issi));
  const today=all.filter(r=>isToday(r.at_ms));
  const t=n=>today.filter(r=>r.reselection==='announced_type_'+n).length;
  return `<div class="bar">${seg('roamf',[['all','All'],['ann','Announced'],['unann','Unannounced'],['within','Within station']])}${searchBox('roam','Filter ISSI…')}<span class="sp"></span><span class="mut hide-s" style="font-size:12.5px">Last ${all.length} of at most 200 changes, in memory</span></div>
  <div class="tiles">
   <div class="tile"><div class="k">Today</div><div class="v">${today.length}</div><div class="s">cell changes</div></div>
   <div class="tile"><div class="k">Announced</div><div class="v">${today.filter(ann).length}</div><div class="s">type 1 · ${t(1)} / type 2 · ${t(2)} / type 3 · ${t(3)}</div></div>
   <div class="tile"><div class="k">Unannounced</div><div class="v">${today.filter(r=>!ann(r)&&r.kind!=='within_station').length}</div><div class="s">left without telling the cell</div></div>
   <div class="tile"><div class="k">Within station</div><div class="v">${today.filter(r=>r.kind==='within_station').length}</div><div class="s">multi-cell stations</div></div>
  </div>
  <div class="card"><div class="ch"><h2>Cell change log</h2><span class="cnt">${list.length}</span>${helpBtn('roam')}</div><div class="tw"><table><thead><tr><th>Time</th><th>ISSI</th><th>From</th><th></th><th>To</th><th>How</th></tr></thead><tbody>
  ${list.map(r=>`<tr><td class="m">${tmd(r.at_ms)}</td><td><b class="m">${r.issi}</b></td><td>${place(r.from)}</td><td class="arrow">→</td><td>${place(r.to)}</td><td>${howOf(r)}</td></tr>`).join('')||'<tr><td colspan="6" class="mut">No cell changes yet</td></tr>'}
  </tbody></table></div>${helpNote('roam','The last cell changes of the radios (cell reselection, TS 100 392-2 clause 18.3.4.7). When the station the radio left reports it (FlowStation with the <code>ms-reselection</code> extension), the category is shown: <i>undeclared</i> = no call running, <i>unannounced</i> = left a call without telling the cell, <i>announced type 1/2/3</i> = told the cell first (U-PREPARE) and got D-NEW-CELL; <i>prepared here</i> = the target was prepared through this server; <i>within station</i> = another cell of a multi-cell station. A radio only registers when it enters another location area, so moves within one location area show only once the station reports the cell.')}</div>`;
};
R.telemetry=()=>{
  const t=D.tele||[];
  const lvl=l=>l==='ok'?'':l==='degraded'?' w':' d';
  return t.length?`<div class="grid2" style="grid-template-columns:repeat(auto-fill,minmax(min(420px,100%),1fr))">${t.map(s=>{const h=s.health||{};
  return `<div class="card"><div class="ch"><h2>${esc(s.site_name||s.id)}</h2><span class="cnt">${esc(s.id)}</span><span class="sp"></span><span class="st${lvl(h.overall)}"><i></i>${esc(h.overall||'unknown')}</span></div>
   <div class="kv" style="padding:12px 18px"><div><span class="l">Version</span>${esc(s.build||s.version||'—')}</div><div><span class="l">Address</span><span class="m">${esc(s.ip||'—')}</span></div><div><span class="l">Connected</span>${ago(s.connected_at_ms)}</div><div><span class="l">Backhaul</span>${s.backhaul_connected==null?'—':s.backhaul_connected?'connected':'down'}</div>
    <div><span class="l">Radios</span>${s.registration_count}</div><div><span class="l">Calls</span>${Object.keys(s.active_calls||{}).length}</div><div><span class="l">TX EVM</span>${s.evm_pct!=null?s.evm_pct.toFixed(1)+' %':'—'}</div><div><span class="l">Emergencies</span>${(s.emergencies||[]).length||'none'}</div></div>
   ${(h.domains||[]).length?`<div style="padding:0 18px 12px;display:flex;gap:6px;flex-wrap:wrap">${h.domains.map(d=>`<span class="chip ${d.level==='ok'?'a':d.level==='degraded'?'w':'d'}" title="${esc(d.detail)}">${esc(d.domain)}: ${esc(d.detail)}</span>`).join('')}</div>`:''}
   <div class="tw"><table><thead><tr><th>Cell</th><th>Carrier</th><th>LA</th><th>RF</th><th>Radios</th></tr></thead><tbody>${(s.cells||[]).map(x=>`<tr><td>${x.id}${x.primary?' <span class="chip i">primary</span>':''}</td><td class="m">${x.main_carrier}${x.secondary_carrier?' + '+x.secondary_carrier:''}</td><td>${x.location_area}</td><td><span class="st${x.rf_state==='online'?'':' w'}" title="${esc(x.rf_detail||'')}"><i></i>${esc(x.rf_state||'—')}</span></td><td class="num">${x.registered_radios}</td></tr>`).join('')||'<tr><td colspan="5" class="mut">No cell report yet</td></tr>'}</tbody></table></div></div>`;}).join('')}</div>`
  :'<div class="card"><div class="empty">No station telemetry connections. Stations report here with FlowStation <code>[telemetry]</code>.</div></div>';
};
R.control=()=>{
  if(!D.who.admin)return `<div class="card"><div class="empty">Station control is for admins. ${D.who.can_login?'<button class="btn" onclick="doLogin()">Log in</button>':''}</div></div>`;
  const ids=D.ctl||[];
  if(!ids.length)return '<div class="card"><div class="empty">No station control connections. Stations connect here with FlowStation <code>[command]</code>.</div></div>';
  return ids.map(id=>{const s=id.replace(/[^a-zA-Z0-9_-]/g,'_');return `<div class="card"><div class="ch"><h2>${esc(id)}</h2><span class="st"><i></i>Connected</span></div>
   <div style="padding:14px 18px;display:flex;flex-direction:column;gap:10px">
    <div class="bar"><input class="f" id="${s}_kick" placeholder="ISSI" inputmode="numeric" style="width:120px"><button class="btn" onclick="ctl('${esc(id)}','${s}',{action:'KickMs',issi:+v('${s}_kick')})">Kick radio</button>
     <input class="f" id="${s}_clr" placeholder="ISSI (0 = all)" inputmode="numeric" style="width:130px"><button class="btn" onclick="ctl('${esc(id)}','${s}',{action:'ClearEmergency',issi:+v('${s}_clr')})">Clear emergency</button></div>
    <div class="bar"><input class="f" id="${s}_sds" placeholder="Live SDS text" style="flex:1;min-width:160px"><input class="f" id="${s}_sdsi" placeholder="from ISSI" inputmode="numeric" style="width:110px"><input class="f" id="${s}_sdsr" placeholder="repeat" value="0" inputmode="numeric" style="width:80px">
     <button class="btn" onclick="ctl('${esc(id)}','${s}',{action:'AddLiveSds',text:v('${s}_sds'),protocol_id:0,source_issi:+v('${s}_sdsi'),repeat_count:+v('${s}_sdsr')})">Add live SDS</button><button class="btn" onclick="ctl('${esc(id)}','${s}',{action:'ClearLiveSds'})">Clear live SDS</button></div>
    <div class="bar"><input class="f" id="${s}_al" placeholder="ISSI" inputmode="numeric" style="width:120px"><button class="btn" onclick="ctl('${esc(id)}','${s}',{action:'AmbienceListen',issi:+v('${s}_al'),enable:true})" title="The target radio keys its microphone (with indication on the radio). Needs station support.">Ambience listen</button><button class="btn" onclick="ctl('${esc(id)}','${s}',{action:'AmbienceListen',issi:+v('${s}_al'),enable:false})">Stop</button>
     <span class="sp"></span><button class="btn danger" onclick="if(confirm('Restart the service on ${esc(id)}?'))ctl('${esc(id)}','${s}','RestartService')">Restart service</button><button class="btn danger" onclick="if(confirm('Shut the service down on ${esc(id)}? It does not come back by itself.'))ctl('${esc(id)}','${s}','ShutdownService')">Shut down</button></div>
    ${msgDiv(s+'_res','padding:0')}<div class="mut" style="font-size:12px">DGNA for several radios at once: <a data-go="routing" href="#routing">Routing</a>.</div></div></div>`;}).join('');
};
const v=id=>($(id)||{}).value||'';
async function ctl(id,s,cmd){const body=typeof cmd==='string'?{action:cmd}:cmd;setMsg(s+'_res','mut','Sending…');
  try{const r=await postJ('/api/control/'+encodeURIComponent(id),body);setMsg(s+'_res','ok','Sent'+(r&&r.response?': '+JSON.stringify(r.response):''));}catch(e){setMsg(s+'_res','err',e.message);}}

/* ---------- routing ---------- */
R.routing=()=>{
  const adm=D.who.admin;
  const pools=D.pools;
  const poolRows=pools.map((p,i)=>{const l=p.live;const groups=p.groups.map((g,j)=>`<span class="tg${(!p.both_ways&&j===0)||(l&&l.gssi===g)?' src':''}">${g}</span>${j===0&&!p.both_ways?' <span class="arrow">→</span>':''}`).join(' ');
   const state=l?`<span class="talk"><span class="bars"><b></b><b></b><b></b><b></b></span>${l.talker} talking on ${l.gssi}</span> · heard on ${l.legs.length} group${l.legs.length===1?'':'s'}${l.waiting.length?` · ${l.waiting.join(', ')} join${l.waiting.length===1?'s':''} after ${l.waiting.length===1?'its':'their'} own over`:''}`:(p.enabled?'Idle':'Switched off');
   return `<div class="row pool${p.enabled?'':' offp'}"><button class="tog" role="switch" aria-checked="${p.enabled}" aria-label="Pool ${esc(p.name)} on" ${adm?'':'disabled'} onclick="togglePool(${i})"></button>
    <div class="rm"><div class="rt">${esc(p.name)} <span class="chip ${p.enabled?'a':'i'}">${p.enabled?'on':'off'}</span><span class="chip">${p.both_ways?'⇄ both ways':'→ one way'}</span></div>
    <div class="flow">${p.both_ways?groups.replace(/<\/span> <span class="tg/,'</span> <span class="arrow">⇄</span> <span class="tg'):groups}</div><div class="rs" style="margin-top:6px">${state}</div></div>
    ${adm?`<button class="btn" onclick="editPool(${i})">Edit</button><button class="btn danger" onclick="delPool(${i})">Delete</button>`:''}</div>`;}).join('');
  const q=UI.filter.dg||'';
  const radios=D.radios.filter(r=>match(q,r.issi,r.station,r.cell,r.groups.join(' '),r.catalog.map(g=>g.gssi+' '+(g.mnemonic||'')).join(' ')));
  for(const id of [...UI.sel])if(!D.radios.some(r=>r.issi===id&&r.radio))UI.sel.delete(id);
  const sel=UI.sel.size;
  const allSel=radios.filter(r=>r.radio).length>0&&radios.filter(r=>r.radio).every(r=>UI.sel.has(r.issi));
  const dg=r=>{if(!r.dgna)return '<span class="mut">—</span>';const d=r.dgna;const age=r.dgna_at_ms?' · '+ago(r.dgna_at_ms)+' ago':'';
    return `<span class="st${d.accepted?'':' w'}"><i></i>${d.accepted?(d.attach?'assigned ':'removed '):'rejected '}${d.gssi}</span><span class="mut" style="font-size:12px">${age}</span>${d.accepted||!d.detail?'':`<div class="mut" style="font-size:12px">${esc(d.detail.replace(/^Rejected:\s*/,''))}</div>`}`;};
  const groupsOf=r=>{const cat=new Map(r.catalog.map(g=>[g.gssi,g]));const all=[...new Set([...r.groups,...r.catalog.filter(g=>g.is_attached).map(g=>g.gssi)])].sort((a,b)=>a-b);
    return all.map(g=>{const c=cat.get(g);return `<span class="tg" title="${c&&c.mnemonic?esc(c.mnemonic):''}${c&&c.is_dynamic?' (DGNA)':''}">${g}${c&&c.mnemonic?' · '+esc(c.mnemonic):''}${c&&c.is_dynamic?' ★':''}</span>`;}).join(' ')||'<span class="mut">—</span>';};
  const f=UI.dgf||(UI.dgf={gssi:'',name:'',assign:true,mode:0});
  return `<div class="card"><div class="ch"><h2>Talkgroup pools</h2><span class="cnt">${pools.length}</span>${helpBtn('pools')}<span class="sp"></span>${adm?`<button class="btn pri" onclick="editPool(-1)">${ic('plus')}New pool</button>`:''}</div>
   ${poolRows||'<div class="empty">No pools yet. A pool plays the calls of one group on other groups too.</div>'}
   ${helpNote('pools','A pool switches groups together inside this server. A call on a group of the pool is played 1:1 on the other groups, as an ordinary group call on each group\'s own GSSI, so the radios see nothing unusual. <i>Both ways</i>: every group of the pool reaches all others. <i>One way</i>: only the first group is copied, to the rest. A group that has a call of its own waits until that over has ended and then joins. Changes apply at once and are saved to the config file, no restart. Federation peers do not get the copies.')}
   ${msgDiv('pool-msg')}</div>
  <div class="card"><div class="ch"><h2>Assign radios to a group (DGNA)</h2>${helpBtn('dgna')}<span class="sp"></span>${searchBox('dg','ISSI, group or station…')}</div>
   ${helpNote('dgna','Dynamic group number assignment (SS-DGNA, ETSI EN 300 392-12-22): the station tells the selected radios to add or remove a group, with an optional name the radio shows. Each command goes to the station the radio is registered on, over its control connection (FlowStation <code>[command]</code>). The answer of each radio shows under <i>Last DGNA</i>. ★ = group assigned by DGNA. Apps and other terminals cannot be reached.')}
   <div class="tw"><table><thead><tr><th style="width:44px"><input type="checkbox" class="chk" aria-label="Select all" ${allSel?'checked':''} ${adm?'':'disabled'} onchange="selAll(this.checked)"></th><th>ISSI</th><th>Station / cell</th><th>Groups now</th><th>Last DGNA</th></tr></thead><tbody>
   ${radios.map(r=>`<tr${r.radio?'':' class="off"'}><td><input type="checkbox" class="chk" aria-label="Select ${r.issi}" ${UI.sel.has(r.issi)?'checked':''} ${adm&&r.radio?'':'disabled'} onchange="selOne(${r.issi},this.checked)"></td><td><b class="m">${r.issi}</b></td><td><span class="chip">${esc(r.cell||r.station)}</span></td><td>${groupsOf(r)}</td><td>${r.radio?dg(r):'<span class="mut">not a radio, DGNA n/a</span>'}</td></tr>`).join('')||'<tr><td colspan="5" class="mut">No radios registered</td></tr>'}
   </tbody></table></div>
   ${adm?`<div class="selbar"><b>${sel} selected</b><span class="arrow">→</span>
    <input class="f" id="dg-gssi" placeholder="GSSI" inputmode="numeric" style="width:110px" value="${esc(f.gssi)}" oninput="UI.dgf.gssi=this.value">
    <input class="f" id="dg-name" placeholder="Name (optional)" maxlength="15" style="width:160px" value="${esc(f.name)}" oninput="UI.dgf.name=this.value">
    <div class="seg" role="group" aria-label="Assign or remove"><button aria-pressed="${f.assign}" onclick="UI.dgf.assign=true;render(true)">Assign</button><button aria-pressed="${!f.assign}" onclick="UI.dgf.assign=false;render(true)">Remove</button></div>
    <label class="mut" style="font-size:12.5px">Attachment <select class="f" id="dg-mode" style="max-width:220px" onchange="UI.dgf.mode=+this.value">${['0 · attached permanently','1 · attach on next ITSI attach','2 · not allowed on next ITSI attach','3 · attach on next location update','4 · not attached, radio may request','5 · not attached, radio may not request'].map((t,i)=>`<option value="${i}" ${f.mode===i?'selected':''}>${t}</option>`).join('')}</select></label>
    <span class="sp"></span><button class="btn" onclick="UI.sel.clear();render(true)" ${sel?'':'disabled'}>Clear</button><button class="btn pri" onclick="sendDgna()" ${sel?'':'disabled'}>Send DGNA to ${sel} radio${sel===1?'':'s'}</button></div>
    ${msgDiv('dg-msg')}`:'<div class="empty">Log in as an admin to send DGNA.</div>'}
  </div>`;
};
function selAll(on){D.radios.filter(r=>r.radio&&match(UI.filter.dg||'',r.issi,r.station,r.cell,r.groups.join(' '))).forEach(r=>on?UI.sel.add(r.issi):UI.sel.delete(r.issi));render(true);}
function selOne(i,on){on?UI.sel.add(i):UI.sel.delete(i);render(true);}
async function sendDgna(){
  const f=UI.dgf,gssi=+f.gssi;
  if(!(gssi>=1&&gssi<=16777215)){setMsg('dg-msg','err','Enter a GSSI (1 to 16777215).');$('dg-gssi').focus();return;}
  setMsg('dg-msg','mut','Sending…');
  try{const res=await postJ('/api/dgna',{issis:[...UI.sel],gssi,name:f.name||null,assign:f.assign,attachment_mode:f.mode});
    const bad=res.filter(r=>!r.sent);
    setMsg('dg-msg',bad.length?'err':'ok',bad.length?`Sent to ${res.length-bad.length} of ${res.length}. Not sent: `+bad.map(b=>b.issi+' ('+b.error+')').join(', '):`Sent to ${res.length} radio${res.length===1?'':'s'}. Their answers show under Last DGNA.`);
    await refreshSlow();
  }catch(e){setMsg('dg-msg','err',e.message);}
}
async function savePools(list,okText){
  try{await postJ('/api/pools/save',list);D.pools=await getJ('/api/pools');setMsg('pool-msg','ok',okText);render(true);return true;}
  catch(e){setMsg('pool-msg','err',e.message);render(true);return false;}
}
const plain=p=>({name:p.name,enabled:p.enabled,both_ways:p.both_ways,groups:p.groups});
function togglePool(i){const l=D.pools.map(plain);l[i].enabled=!l[i].enabled;savePools(l,`${l[i].name} switched ${l[i].enabled?'on':'off'}.`);}
function delPool(i){const p=D.pools[i];if(!confirm(`Delete pool "${p.name}"?`))return;const l=D.pools.map(plain);l.splice(i,1);savePools(l,`${p.name} deleted.`);}
function editPool(i){
  const p=i>=0?plain(D.pools[i]):{name:'',enabled:true,both_ways:true,groups:[]};
  $('sheet').innerHTML=`<div class="ch"><h3 id="sheet-title">${i>=0?'Edit pool':'New pool'}</h3></div><div class="sb">
   <div><label class="l" for="pe-name">Name</label><input class="f" id="pe-name" maxlength="40" value="${esc(p.name)}" placeholder="e.g. Bezirk Nord"></div>
   <div><label class="l" for="pe-groups">Groups (GSSI), separated by commas or spaces</label><input class="f" id="pe-groups" value="${p.groups.join(', ')}" placeholder="2321, 2323, 2324, 264" inputmode="numeric"></div>
   <div><span class="l" style="font-size:12px;font-weight:600;color:var(--text2)">Direction</span><div class="seg" role="radiogroup" id="pe-dir" style="margin-top:5px"><button role="radio" aria-pressed="${p.both_ways}" aria-checked="${p.both_ways}" onclick="setDir(true)">⇄ Both ways</button><button role="radio" aria-pressed="${!p.both_ways}" aria-checked="${!p.both_ways}" onclick="setDir(false)">→ One way, from the first group</button></div></div>
   <label style="display:flex;gap:10px;align-items:center"><button class="tog" role="switch" id="pe-on" aria-checked="${p.enabled}" onclick="this.setAttribute('aria-checked',this.getAttribute('aria-checked')!=='true')"></button> Switched on</label>
   <div class="msg" id="sheet-msg" style="padding:0"></div></div>
   <div class="acts"><button class="btn" onclick="closeSheet()">Cancel</button><button class="btn pri" onclick="savePool(${i})">Save</button></div>`;
  $('scrim').classList.add('on');setTimeout(()=>$('pe-name').focus(),30);
}
function setDir(b){document.querySelectorAll('#pe-dir button').forEach((x,j)=>{const on=(j===0)===b;x.setAttribute('aria-pressed',on);x.setAttribute('aria-checked',on);});}
function closeSheet(){$('scrim').classList.remove('on');}
document.addEventListener('keydown',e=>{if(e.key==='Escape')closeSheet();});
async function savePool(i){
  const groups=$('pe-groups').value.split(/[\s,;]+/).filter(Boolean).map(Number);
  if(groups.some(g=>!Number.isInteger(g))){$('sheet-msg').className='msg err';$('sheet-msg').textContent='Groups must be numbers.';return;}
  const p={name:$('pe-name').value.trim(),groups,both_ways:document.querySelector('#pe-dir button').getAttribute('aria-pressed')==='true',enabled:$('pe-on').getAttribute('aria-checked')==='true'};
  const l=D.pools.map(plain);if(i>=0)l[i]=p;else l.push(p);
  const m=$('sheet-msg');m.className='msg mut';m.textContent='Saving…';
  try{await postJ('/api/pools/save',l);closeSheet();D.pools=await getJ('/api/pools');setMsg('pool-msg','ok',`${p.name} saved.`);render(true);}
  catch(e){m.className='msg err';m.textContent=e.message;}
}

buildNav();
show(location.hash.slice(1)||'overview');
refreshFast();refreshSlow();
setInterval(refreshFast,2000);setInterval(refreshSlow,5000);
setInterval(()=>{if(UI.page==='overview'||UI.page==='calls')render();},1000);
</script>
</body></html>"##;
