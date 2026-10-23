#!/usr/bin/env python3
"""CrStudio kontrol kanalı istemcisi (demo ve testler için).

  python3 tools/demo/ctl.py exec crstudio.status '{}'
  python3 tools/demo/ctl.py shot target/demo/01.png [panel]
  python3 tools/demo/ctl.py wait            # arka plan işi bitene kadar bekler
  python3 tools/demo/ctl.py raw ui.inspect '{}'

Uygulama `cargo run -p filmcraft -- --control 9876` ile açık olmalı (port: CRSTUDIO_PORT).
"""
import json
import os
import socket
import sys
import time

PORT = int(os.environ.get("CRSTUDIO_PORT", "9876"))


def call(method, params=None, timeout=600):
    with socket.create_connection(("127.0.0.1", PORT), timeout=timeout) as s:
        s.sendall((json.dumps({"id": 1, "method": method, "params": params or {}}) + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = s.recv(1 << 16)
            if not chunk:
                break
            buf += chunk
    reply = json.loads(buf.decode() or "{}")
    if not reply.get("ok", False):
        raise SystemExit(f"HATA: {reply.get('error')}")
    return reply.get("result")


def execute(cmd, params=None):
    return call("engine.execute", {"command": cmd, "params": params or {}})


def wait_job(limit=3600):
    t0 = time.time()
    last = ""
    while time.time() - t0 < limit:
        st = execute("crstudio.status")
        job = (st or {}).get("job") or {}
        line = f"{job.get('label','')} {job.get('progress','')} {job.get('detail','')}".strip()
        if line != last:
            print("…", line, flush=True)
            last = line
        if not job.get("running"):
            return st
        time.sleep(2)
    raise SystemExit("zaman aşımı")


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return
    op = argv[1]
    if op == "exec":
        r = execute(argv[2], json.loads(argv[3]) if len(argv) > 3 else {})
    elif op == "raw":
        r = call(argv[2], json.loads(argv[3]) if len(argv) > 3 else {})
    elif op == "shot":
        p = {"path": os.path.abspath(argv[2])}
        if len(argv) > 3:
            p["panel"] = argv[3]
        r = call("ui.screenshot", p)
    elif op == "wait":
        r = wait_job()
        r = {"stage": (r or {}).get("stage"), "job": (r or {}).get("job")}
    else:
        raise SystemExit(f"bilinmeyen işlem {op}")
    print(json.dumps(r, ensure_ascii=False, indent=1)[:4000])


if __name__ == "__main__":
    main(sys.argv)
