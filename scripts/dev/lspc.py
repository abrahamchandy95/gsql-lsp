#!/usr/bin/env python3
"""LSP test client for gsql-lsp (raw framing control, timing)."""
import json, os, queue, subprocess, threading, time

BIN = os.environ.get("GSQL_LSP_BIN", "gsql-lsp")

def frame(obj):
    body = json.dumps(obj).encode() if not isinstance(obj, (bytes, str)) else (obj.encode() if isinstance(obj, str) else obj)
    return b"Content-Length: %d\r\n\r\n" % len(body) + body

class Client:
    def __init__(self, root=None, encodings=None, capabilities=None, init_options=None,
                 folders=None, initialize=True, extra_init=None, args=None):
        self.proc = subprocess.Popen([BIN] + (args or []), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE)
        self.q = queue.Queue()
        self.next_id = 1
        self.backlog = []
        self.stderr_lines = []
        self.all = []
        threading.Thread(target=self._reader, daemon=True).start()
        threading.Thread(target=self._err_reader, daemon=True).start()
        if initialize:
            caps = capabilities if capabilities is not None else {}
            if encodings is not None:
                caps.setdefault("general", {})["positionEncodings"] = encodings
            params = {"processId": None, "rootUri": root, "capabilities": caps}
            if folders is not None:
                params["workspaceFolders"] = folders
            if init_options is not None:
                params["initializationOptions"] = init_options
            if extra_init:
                params.update(extra_init)
            self.init_result = self.request("initialize", params)
            self.notify("initialized", {})

    def _err_reader(self):
        for line in self.proc.stderr:
            self.stderr_lines.append(line.decode(errors="replace"))

    def _reader(self):
        f = self.proc.stdout
        while True:
            length = None
            while True:
                line = f.readline()
                if not line:
                    self.q.put(None)
                    return
                s = line.decode().strip()
                if not s:
                    if length is not None:
                        break
                    continue
                if s.lower().startswith("content-length:"):
                    length = int(s.split(":", 1)[1])
            body = f.read(length)
            msg = json.loads(body)
            msg["_t"] = time.time()
            self.all.append(msg)
            self.q.put(msg)

    def send(self, msg):
        self.raw(frame(msg))

    def raw(self, data: bytes):
        self.proc.stdin.write(data)
        self.proc.stdin.flush()

    def notify(self, method, params):
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def send_request(self, method, params, rid=None):
        if rid is None:
            rid = self.next_id
            self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        return rid

    def wait_response(self, rid, timeout=30):
        for i, m in enumerate(self.backlog):
            if m.get("id") == rid and "method" not in m:
                return self.backlog.pop(i)
        deadline = time.time() + timeout
        while True:
            left = deadline - time.time()
            if left <= 0:
                raise TimeoutError("response %r" % (rid,))
            try:
                msg = self.q.get(timeout=left)
            except queue.Empty:
                raise TimeoutError("response %r" % (rid,))
            if msg is None:
                raise RuntimeError("server exited")
            if msg.get("id") == rid and "method" not in msg:
                return msg
            self.backlog.append(msg)

    def request(self, method, params, timeout=30):
        """Returns the full response message (with result or error)."""
        rid = self.send_request(method, params)
        return self.wait_response(rid, timeout)

    def result(self, method, params, timeout=30):
        r = self.request(method, params, timeout)
        if "error" in r:
            raise RuntimeError("%s failed: %s" % (method, r["error"]))
        return r["result"]

    def wait_notification(self, method, timeout=10, pred=None):
        for i, m in enumerate(self.backlog):
            if m.get("method") == method and "id" not in m and (pred is None or pred(m)):
                return self.backlog.pop(i)
        deadline = time.time() + timeout
        while True:
            left = deadline - time.time()
            if left <= 0:
                return None
            try:
                msg = self.q.get(timeout=left)
            except queue.Empty:
                return None
            if msg is None:
                raise RuntimeError("server exited")
            if msg.get("method") == method and "id" not in msg and (pred is None or pred(msg)):
                return msg
            self.backlog.append(msg)

    def drain(self, timeout=0.3):
        """Collect everything that arrives within timeout (quiet period)."""
        out = []
        while True:
            try:
                msg = self.q.get(timeout=timeout)
            except queue.Empty:
                break
            if msg is None:
                break
            out.append(msg)
        self.backlog.extend(out)
        return out

    def diags(self, uri, timeout=10, pred=None):
        m = self.wait_notification("textDocument/publishDiagnostics", timeout,
                                   lambda m: m["params"]["uri"] == uri and (pred is None or pred(m)))
        return m

    def open(self, uri, text, version=1, wait=True, timeout=30):
        self.notify("textDocument/didOpen", {"textDocument": {"uri": uri, "languageId": "gsql", "version": version, "text": text}})
        if wait:
            return self.diags(uri, timeout=timeout)

    def change(self, uri, version, changes):
        self.notify("textDocument/didChange", {"textDocument": {"uri": uri, "version": version}, "contentChanges": changes})

    def shutdown_exit(self, timeout=5):
        try:
            r = self.request("shutdown", None, timeout=timeout)
            self.notify("exit", None)
            self.proc.wait(timeout=timeout)
        except Exception as e:
            self.proc.kill()
            self.proc.wait()
            return ("killed", repr(e))
        return self.proc.returncode

    def kill(self):
        try:
            self.proc.kill()
        except Exception:
            pass

def rng(sl, sc, el, ec):
    return {"start": {"line": sl, "character": sc}, "end": {"line": el, "character": ec}}

def pos(l, c):
    return {"line": l, "character": c}

def td(uri):
    return {"textDocument": {"uri": uri}}

def decode_tokens(data, legend=None):
    out = []
    line = 0; start = 0
    for i in range(0, len(data), 5):
        dl, ds, ln, ty, mod = data[i:i+5]
        if dl:
            line += dl; start = ds
        else:
            start += ds
        out.append((line, start, ln, legend[ty] if legend else ty, mod))
    return out
