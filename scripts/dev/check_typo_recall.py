"""Introduces a typo in every keyword of a long query and checks the typo is found without extra diagnostics (expects found >= 776 of 793, noisy 0).

Developer regression check, run by hand against a built server:
    GSQL_LSP_BIN=target/release/gsql-lsp python3 scripts/dev/check_typo_recall.py
Needs a project to test against (G2N_DIR, LONE_QUERY env vars; see the paths below).
"""
import sys, os, re, time, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__))); os.environ.setdefault('GSQL_LSP_BIN', 'gsql-lsp')
import lspc
f=os.environ.get('LONE_QUERY', os.path.expanduser('~/Documents/retrieve_staleflag_multiple_json.gsql')); text=open(f).read(); lines=text.split('\n')
uri='file://'+f; c=lspc.Client(root=None); v=1
c.notify('textDocument/didOpen',{'textDocument':{'uri':uri,'languageId':'gsql','version':1,'text':text}})
KW={'IF','THEN','ELSE','END','FOREACH','IN','DO','RANGE','FROM','WHERE','ACCUM','SELECT','RAISE','PRINT','AND','OR','NOT','CASE','WHEN','TYPEDEF','TUPLE','EXCEPTION','UNION','CREATE','OR','REPLACE','QUERY','FOR','GRAPH','SYNTAX','AS'}
def diags(t2):
    global v; v+=1
    c.notify("textDocument/didChange",{"textDocument":{"uri":uri,"version":v},"contentChanges":[{"text":t2}]})
    n=c.wait_notification("textDocument/publishDiagnostics",timeout=15,pred=lambda n:n["params"]["uri"]==uri and n["params"].get("version")==v)
    return n['params']['diagnostics']
base=len([d for d in diags(text) if d['code']!='no-schema'])
print('baseline diags',base)
stats=collections.Counter(); bad=[]
for i,l in enumerate(lines):
    for m in re.finditer(r'\b[A-Z]{2,}\b',l):
        w=m.group()
        if w not in KW: continue
        for new in (w[:-1], w[0]+w[2:] if len(w)>3 else None):
            if not new: continue
            t2=lines[:]; t2[i]=l[:m.start()]+new+l[m.end():]
            d=[x for x in diags('\n'.join(t2)) if x['code']!='no-schema']
            syn=[x for x in d if x['code']=='syntax-error']
            extra=len(d)-len(syn)-base
            ok = any(('did you mean `%s`'%w in x['message']) for x in syn)
            stats['total']+=1; stats['found']+=ok; stats['noisy']+= (extra>0 or len(syn)>1)
            if not ok or extra>0 or len(syn)>1: bad.append((i+1,w,new,ok,len(syn),extra,[x['message'][:50] for x in d][:3]))
print(dict(stats))
for b in bad[:25]: print(b)
c.proc.terminate()
