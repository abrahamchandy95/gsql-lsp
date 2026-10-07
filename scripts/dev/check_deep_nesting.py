"""Deeply nested inputs must be diagnosed in about a second and answer requests at once.

Developer regression check, run by hand against a built server:
    GSQL_LSP_BIN=target/release/gsql-lsp python3 scripts/dev/check_deep_nesting.py
Needs a project to test against (G2N_DIR, LONE_QUERY env vars; see the paths below).
"""
import sys, os, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__))); os.environ.setdefault('GSQL_LSP_BIN', 'gsql-lsp')
import lspc
inputs=[
 "CREATE QUERY q() {\n  INT x = %s1%s;\n}\n" % ("("*100000, ")"*100000),
 "CREATE QUERY q() {\n  %sINT%s @@x;\n}\n" % ("ListAccum<"*20000, ">"*20000),
 "CREATE QUERY q(BOOL b) {\n%sPRINT 1;\n%s}\n" % ("IF b THEN\n"*5000, "END;\n"*5000),
 "CREATE QUERY q() {\n  INT x = %s;\n}\n" % " + ".join(["abs(1)"]*20000),
 "CREATE QUERY q(INT y) {\n  INT x = %s;\n  PRINT %s;\n}\n" % (" + ".join(["y"]*20000)," + ".join(['"s"']*20000)),
 "CREATE QUERY q() {\n  R = SELECT s FROM P:s ACCUM %s\n  PRINT R;\n}\n" % ("IF TRUE THEN @@n += 1 ELSE "*2000+"@@n += 2"+" END"*2000+";"),
]
for k,t in enumerate(inputs):
    uri='file:///deep%d.gsql'%k; c=lspc.Client(root=None); s=time.time()
    c.notify("textDocument/didOpen",{"textDocument":{"uri":uri,"languageId":"gsql","version":1,"text":t}})
    try:
        c.wait_notification("textDocument/publishDiagnostics",timeout=60,pred=lambda n:n["params"]["uri"]==uri)
        t1=time.time()-s
        for _ in range(3):
            c.request("textDocument/definition",{"textDocument":{"uri":uri},"position":{"line":1,"character":20}})
            c.request("textDocument/hover",{"textDocument":{"uri":uri},"position":{"line":1,"character":20}})
        print(k,'diag %.2fs'%t1,'requests %.2fs'%(time.time()-s-t1))
    except Exception as e: print(k,'FAILED',e)
    c.proc.terminate()
