import re
from pathlib import Path

p = Path(r"d:/Hady/02_Workspace/Cross-SystemConnect/src/ui/SettingsPage.cpp")
text = p.read_text(encoding="utf-8")
m = re.search(r'R"HTML\((.*)\)HTML"', text, re.S)
assert m, "html not found"
html = m.group(1)
bridge = """
async function getConfig(){
  const r=await fetch('csc://localhost/config');
  return r.json();
}
async function saveConfig(body){
  const r=await fetch('csc://localhost/config',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)});
  return r.json();
}
async function getStatus(){
  const r=await fetch('csc://localhost/status');
  return r.json();
}
"""
html = html.replace("async function load(){", bridge + "\nasync function load(){")
out = Path(r"d:/Hady/02_Workspace/Cross-SystemConnect/crates/csc-ui/ui")
out.mkdir(parents=True, exist_ok=True)
(out / "index.html").write_text(html, encoding="utf-8")
print("wrote", out / "index.html", "bytes", len(html))
