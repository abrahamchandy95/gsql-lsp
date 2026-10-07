"""Every x.attribute use in the G2N queries must resolve to the schema file (expects 3068 of 3068).

Developer regression check, run by hand against a built server:
    GSQL_LSP_BIN=target/release/gsql-lsp python3 scripts/dev/check_attribute_lookups.py
Needs a project to test against (G2N_DIR, LONE_QUERY env vars; see the paths below).
"""
import sys, re, time, glob, collections, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__))); os.environ.setdefault('GSQL_LSP_BIN', 'gsql-lsp')
import lspc
ROOT=os.environ.get('G2N_DIR', os.path.expanduser('~/Downloads/G2N-main'))
c=lspc.Client(root='file://'+ROOT); time.sleep(6)
schema=open(ROOT+'/schema/01_schema.gsql').read()
attrs=set(re.findall(r'^\s*(?:PRIMARY_ID\s+)?(\w+)\s+(?:STRING|INT|UINT|DOUBLE|FLOAT|BOOL|DATETIME)\b',schema,re.M))
def pos_of(text,off): return {"line":text.count('\n',0,off),"character":off-(text.rfind('\n',0,off)+1)}
tot=ok=0; bad=collections.Counter(); ex={}; where=collections.Counter()
for f in sorted(glob.glob(ROOT+'/queries/*.gsql')):
    text=open(f).read(); uri='file://'+f
    c.notify("textDocument/didOpen",{"textDocument":{"uri":uri,"languageId":"gsql","version":1,"text":text}})
    clean=re.sub(r'/\*.*?\*/|//[^\n]*|"(?:\\.|[^"\\])*"',lambda m:' '*len(m.group(0)),text,flags=re.S)
    for m in re.finditer(r'(?<![\w@])([A-Za-z_]\w*)\.(\w+)\b(?!\s*\()',clean):
        if m.group(2) not in attrs: continue
        tot+=1
        r=c.request("textDocument/definition",{"textDocument":{"uri":uri},"position":pos_of(text,m.start(2)+1)},timeout=20).get('result') or []
        if r and r[0]['uri'].endswith('01_schema.gsql'): ok+=1
        else: bad[m.group(0)]+=1; ex.setdefault(m.group(0),(os.path.basename(f),pos_of(text,m.start())['line']+1))
print('attribute uses',tot,'resolved to schema',ok,'unresolved',tot-ok)
for k,n in bad.most_common(15): print(n,k,ex[k])
c.proc.terminate()
