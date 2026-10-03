//! Progress of files arriving on this computer. The sender shows nothing;
//! this window opens on the receiver when a batch starts and also asks what to
//! do with names that already exist.

use crate::{ipc_client, UserEvent};
use serde_json::Value;
use std::collections::HashSet;
use tao::dpi::LogicalSize;
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder, WindowId};
use wry::{WebView, WebViewBuilder};

const HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><style>
:root{--bg:#141b24;--card:#1c2632;--line:#2b3a4c;--fg:#e8f1ff;--dim:#8fa3ba;--accent:#3b8cff;--bad:#ff6b6b;--good:#4cd49a}
html,body{margin:0;background:var(--bg);color:var(--fg);font:13px system-ui,"Microsoft YaHei",sans-serif}
body{padding:10px;box-sizing:border-box}
.item{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:10px 12px;margin-bottom:10px}
.head{display:flex;justify-content:space-between;gap:8px;align-items:baseline}
.title{font-weight:600;word-break:break-all}
.from{color:var(--dim);white-space:nowrap}
.bar{height:6px;background:#0d131a;border-radius:3px;margin:8px 0;overflow:hidden}
.bar>div{height:100%;background:var(--accent);transition:width .2s}
.item.done .bar>div{background:var(--good)}
.item.failed .bar>div,.item.cancelled .bar>div{background:var(--bad)}
.item.paused_link_lost .bar>div{background:#e6b35a}
.meta{display:flex;justify-content:space-between;color:var(--dim);gap:8px}
.msg{margin-top:6px;color:var(--dim);word-break:break-all}
.item.failed .msg{color:var(--bad)}
.acts{display:flex;gap:6px;justify-content:flex-end;margin-top:8px;flex-wrap:wrap}
button{background:#243244;color:var(--fg);border:1px solid var(--line);border-radius:6px;padding:4px 12px;font:inherit;cursor:pointer}
button:hover{border-color:var(--accent)}
button.primary{background:var(--accent);border-color:var(--accent)}
.conf{margin-top:8px;border-top:1px solid var(--line);padding-top:8px}
.conf p{margin:0 0 6px}
.row{display:flex;justify-content:space-between;align-items:center;gap:8px;padding:4px 0}
.row .n{word-break:break-all}
.row .d{color:var(--dim);font-size:12px}
.seg{display:flex;flex-shrink:0}
.seg button{border-radius:0;padding:3px 8px}
.seg button:first-child{border-radius:6px 0 0 6px}
.seg button:last-child{border-radius:0 6px 6px 0}
.seg button.on{background:var(--accent);border-color:var(--accent)}
.all{color:var(--dim);display:flex;gap:6px;align-items:center;justify-content:flex-end;margin:4px 0}
.empty{color:var(--dim);text-align:center;padding:30px 0}
</style></head><body><div id="list"><div class="empty">没有正在接收的文件</div></div><script>
const picks={};
const LABEL={skip:'跳过',keep_both:'保留两者',replace:'替换'};
const STATE={waiting:'等待中',conflict:'有重名文件',running:'正在接收',done:'已完成',failed:'失败',cancelled:'已取消',paused_link_lost:'等待重连'};
function post(m){ window.ipc.postMessage(JSON.stringify(m)); }
function size(n){ const u=['B','KB','MB','GB','TB']; let i=0; while(n>=1024&&i<u.length-1){n/=1024;i++;} return (i?n.toFixed(1):n)+' '+u[i]; }
function eta(t){ if(!t.bytesPerSec||t.state!=='running') return ''; const s=Math.ceil((t.bytesTotal-t.bytesDone)/t.bytesPerSec);
  return s>=3600?('剩余 '+Math.floor(s/3600)+' 小时 '+Math.ceil(s%3600/60)+' 分'):s>=60?('剩余 '+Math.floor(s/60)+' 分 '+(s%60)+' 秒'):('剩余 '+s+' 秒'); }
function esc(s){ return String(s).replace(/[&<>"]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c])); }
function when(t){ if(!t) return ''; const d=new Date(t*1000); return d.toLocaleString(); }
function pick(id,index,c){ (picks[id]=picks[id]||{})[index]=c; render(last); }
function pickAll(id,c){ const t=last.find(x=>x.id===id); if(!t) return; t.conflicts.forEach(k=>pick(id,k.index,c)); }
function resolve(id){ const t=last.find(x=>x.id===id); if(!t) return;
  const p=picks[id]||{}; post({action:'resolve',id,decisions:t.conflicts.map(k=>({index:k.index,choice:p[k.index]||'keep_both'}))}); }
let last=[];
function render(list){
  last=list||[];
  const el=document.getElementById('list');
  if(!last.length){ el.innerHTML='<div class="empty">没有正在接收的文件</div>'; return; }
  el.innerHTML=last.map(t=>{
    const pct=t.bytesTotal?Math.min(100,Math.floor(t.bytesDone*100/t.bytesTotal)):(t.state==='done'?100:0);
    const from=t.via?(esc(t.peer)+'（经 '+esc(t.via)+'）'):esc(t.peer);
    const fin=['done','failed','cancelled','paused_link_lost'].includes(t.state);
    let acts='';
    if(!fin) acts+=`<button onclick="post({action:'cancel',id:${t.id}})">取消</button>`;
    if(t.state==='done'&&t.openPath) acts+=`<button onclick='post({action:"open",path:${esc(JSON.stringify(t.openPath))}})'>打开</button>`;
    if(fin&&t.dest) acts+=`<button onclick='post({action:"open",path:${esc(JSON.stringify(t.dest))}})'>打开文件夹</button>`;
    if(fin) acts+=`<button onclick="post({action:'dismiss',id:${t.id}})">关闭</button>`;
    let conf='';
    if(t.state==='conflict'&&t.conflicts&&t.conflicts.length){
      const p=picks[t.id]||{};
      conf=`<div class="conf"><p>目标文件夹里已有 ${t.conflicts.length} 个同名项目：</p>`+
        t.conflicts.map(k=>{
          const c=p[k.index]||'keep_both';
          const d=k.isDirectory?'文件夹（替换会合并内容）':('新：'+size(k.newSize)+'　原有：'+size(k.oldSize)+(k.oldMtimeUnix?'，'+when(k.oldMtimeUnix):''));
          return `<div class="row"><div><div class="n">${esc(k.name)}</div><div class="d">${d}</div></div><div class="seg">`+
            ['skip','keep_both','replace'].map(v=>`<button class="${c===v?'on':''}" onclick="pick(${t.id},${k.index},'${v}')">${LABEL[v]}</button>`).join('')+
            `</div></div>`;
        }).join('')+
        (t.conflicts.length>1?`<div class="all">全部：`+['skip','keep_both','replace'].map(v=>`<button onclick="pickAll(${t.id},'${v}')">${LABEL[v]}</button>`).join('')+`</div>`:'')+
        `<div class="acts"><button class="primary" onclick="resolve(${t.id})">继续</button></div></div>`;
    }
    const files=t.filesTotal>1?`${t.filesDone}/${t.filesTotal} 个文件　`:'';
    const speed=t.state==='running'&&t.bytesPerSec?size(t.bytesPerSec)+'/s　':'';
    const msg=t.message?`<div class="msg">${esc(t.message)}</div>`:(t.dest&&!fin?`<div class="msg">保存到 ${esc(t.dest)}</div>`:'');
    return `<div class="item ${t.state}"><div class="head"><div class="title">${esc(t.title)}</div><div class="from">来自 ${from}</div></div>`+
      `<div class="bar"><div style="width:${pct}%"></div></div>`+
      `<div class="meta"><span>${files}${size(t.bytesDone)} / ${size(t.bytesTotal)}</span><span>${speed}${eta(t)||STATE[t.state]||''}</span></div>`+
      msg+conf+(acts?`<div class="acts">${acts}</div>`:'')+`</div>`;
  }).join('');
}
</script></body></html>"#;

pub struct TransferWindow {
    window: Window,
    webview: WebView,
    /// Batches the user closed the window on; they do not reopen it.
    hidden: HashSet<u64>,
    /// Batches whose conflict prompt already brought the window forward.
    asked: HashSet<u64>,
}

fn handle(msg: &str) {
    let Ok(v) = serde_json::from_str::<Value>(msg) else {
        return;
    };
    let id = v.get("id").and_then(|n| n.as_u64());
    let action = v
        .get("action")
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .to_string();
    let path = v.get("path").and_then(|p| p.as_str()).map(str::to_string);
    let decisions = v.get("decisions").cloned();
    std::thread::spawn(move || {
        let result = match (action.as_str(), id) {
            ("cancel", Some(id)) => ipc_client::cancel_transfer(id).map(|_| ()),
            ("dismiss", Some(id)) => ipc_client::dismiss_transfer(id).map(|_| ()),
            ("resolve", Some(id)) => {
                ipc_client::resolve_conflict(id, decisions.unwrap_or(Value::Null)).map(|_| ())
            }
            ("open", _) => match path {
                Some(p) if !p.is_empty() => open::that(p).map_err(Into::into),
                _ => Ok(()),
            },
            _ => Ok(()),
        };
        if let Err(e) = result {
            tracing::warn!("transfer window {action}: {e:#}");
        }
    });
}

impl TransferWindow {
    pub fn create(target: &EventLoopWindowTarget<UserEvent>) -> Option<Self> {
        let mut wb = WindowBuilder::new()
            .with_title("Cross-SystemConnect 文件接收")
            .with_inner_size(LogicalSize::new(460.0, 340.0))
            .with_min_inner_size(LogicalSize::new(320.0, 160.0))
            .with_visible(false);
        if let Some(ic) = crate::icon::app_window_icon() {
            wb = wb.with_window_icon(Some(ic));
        }
        let window = match wb.build(target) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("transfer window: {e}");
                return None;
            }
        };
        let builder = WebViewBuilder::new()
            .with_html(HTML)
            .with_ipc_handler(|req| handle(req.body()));
        #[cfg(not(target_os = "linux"))]
        let webview = builder.build(&window);
        #[cfg(target_os = "linux")]
        let webview = {
            use tao::platform::unix::WindowExtUnix;
            use wry::WebViewBuilderExtUnix;
            builder.build_gtk(window.default_vbox()?)
        };
        match webview {
            Ok(webview) => Some(Self {
                window,
                webview,
                hidden: HashSet::new(),
                asked: HashSet::new(),
            }),
            Err(e) => {
                tracing::error!("transfer window webview: {e}");
                None
            }
        }
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    /// New list from the core. Opens the window for a batch it has not shown.
    pub fn update(&mut self, list: &[Value]) {
        let json = serde_json::to_string(list).unwrap_or_else(|_| "[]".into());
        let _ = self.webview.evaluate_script(&format!("render({json});"));
        let ids: HashSet<u64> = list.iter().filter_map(|t| t.get("id")?.as_u64()).collect();
        self.hidden.retain(|id| ids.contains(id));
        self.asked.retain(|id| ids.contains(id));
        if list.is_empty() {
            self.window.set_visible(false);
            return;
        }
        let state = |t: &Value| {
            t.get("state")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string()
        };
        let mut raise = false;
        let mut open_new = false;
        for t in list {
            let Some(id) = t.get("id").and_then(|n| n.as_u64()) else {
                continue;
            };
            let st = state(t);
            if st == "conflict" && self.asked.insert(id) {
                self.hidden.remove(&id);
                raise = true;
            }
            let finished = matches!(st.as_str(), "done" | "failed" | "cancelled");
            if !self.hidden.contains(&id) && !finished {
                open_new = true;
            }
        }
        let any_shown = list.iter().any(|t| {
            t.get("id")
                .and_then(|n| n.as_u64())
                .is_some_and(|id| !self.hidden.contains(&id))
        });
        if !any_shown {
            self.window.set_visible(false);
        } else if raise || (open_new && !self.window.is_visible()) {
            self.window.set_visible(true);
            if raise {
                self.window.set_focus();
            }
        }
    }

    /// Closing hides the window until another batch arrives.
    pub fn close(&mut self, list: &[Value]) {
        let mut finished = Vec::new();
        for t in list {
            if let Some(id) = t.get("id").and_then(|n| n.as_u64()) {
                self.hidden.insert(id);
                let st = t.get("state").and_then(|s| s.as_str()).unwrap_or("");
                if matches!(st, "done" | "failed" | "cancelled") {
                    finished.push(id);
                }
            }
        }
        self.window.set_visible(false);
        if !finished.is_empty() {
            std::thread::spawn(move || {
                for id in finished {
                    let _ = ipc_client::dismiss_transfer(id);
                }
            });
        }
    }
}
