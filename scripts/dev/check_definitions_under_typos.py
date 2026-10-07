"""Go to definition of a variable must keep working while a nearby keyword is mistyped (expects 0 failures).

Developer regression check, run by hand against a built server:
    GSQL_LSP_BIN=target/release/gsql-lsp python3 scripts/dev/check_definitions_under_typos.py
Needs a project to test against (G2N_DIR, LONE_QUERY env vars; see the paths below).
"""
import sys, os, re, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__))); os.environ.setdefault('GSQL_LSP_BIN', 'gsql-lsp')
import lspc
f=os.environ.get('LONE_QUERY', os.path.expanduser('~/Documents/retrieve_staleflag_multiple_json.gsql')); text=open(f).read(); lines=text.split('\n')
uri='file://'+f; c=lspc.Client(root=None); v=1
KW={'IF','THEN','ELSE','END','FOREACH','IN','DO','RANGE','FROM','WHERE','ACCUM','SELECT','RAISE','PRINT','AND','OR','NOT','CASE','WHEN','TYPEDEF','TUPLE','EXCEPTION','UNION'}
rline=[i for i,l in enumerate(lines) if 'IF r.containsKey("skdpn")' in l][0]; rcol=lines[rline].index('r.contains')
c.notify('textDocument/didOpen',{'textDocument':{'uri':uri,'languageId':'gsql','version':1,'text':text}})
bad=[]; n=0; slow=0
for i,l in enumerate(lines):
    if i<95 or i>260: continue
    for m in re.finditer(r'\b[A-Z]{2,}\b',l):
        if m.group() not in KW: continue
        w=m.group()
        for new in (w[:-1], w[:1]+w[2:] if len(w)>2 else None):
            if not new or new==w: continue
            t2=lines[:]; t2[i]=l[:m.start()]+new+l[m.end():]; v+=1
            c.notify("textDocument/didChange",{"textDocument":{"uri":uri,"version":v},"contentChanges":[{"text":'\n'.join(t2)}]})
            t=time.time()
            r=c.request("textDocument/definition",{"textDocument":{"uri":uri},"position":{"line":rline,"character":rcol}})
            dt=(time.time()-t)*1000
            ok=bool(r.get('result')) 
            if i==rline and False: pass
            n+=1
            if dt>250: slow+=1
            if not ok: bad.append((i+1,w,new,round(dt)))
print('mutations',n,'definition failed',len(bad),'slow>250ms',slow)
for b in bad[:40]: print(b)
c.proc.terminate()
