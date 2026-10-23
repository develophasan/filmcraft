"""CrStudio yerel Chatterbox Multilingual sunucusu (yalnız 127.0.0.1).

POST /tts  {"text": "...", "out": "/yol/cikti.wav", "language": "tr",
            "voice_ref": "/yol/referans.wav" (isteğe bağlı), "exaggeration": 0.5, "cfg": 0.5}
       -> {"ok": true, "seconds": 4.2}
GET  /health -> {"ok": true, "device": "mps"}
"""
import argparse
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import torch

DEVICE = "mps" if torch.backends.mps.is_available() else ("cuda" if torch.cuda.is_available() else "cpu")
_orig_load = torch.load


def _load(*a, **k):
    k.setdefault("map_location", torch.device(DEVICE))
    return _orig_load(*a, **k)


torch.load = _load

from chatterbox.mtl_tts import ChatterboxMultilingualTTS  # noqa: E402
import torchaudio  # noqa: E402

MODEL = None
LOCK = threading.Lock()


def model():
    global MODEL
    if MODEL is None:
        MODEL = ChatterboxMultilingualTTS.from_pretrained(device=DEVICE)
    return MODEL


def synth(text, out, language="tr", voice_ref=None, exaggeration=0.5, cfg=0.5):
    with LOCK:
        m = model()
        kw = {"language_id": language, "exaggeration": float(exaggeration), "cfg_weight": float(cfg)}
        if voice_ref:
            kw["audio_prompt_path"] = voice_ref
        wav = m.generate(text, **kw)
        torchaudio.save(out, wav.cpu(), m.sr)
        return wav.shape[-1] / float(m.sr)


class H(BaseHTTPRequestHandler):
    def _send(self, code, obj):
        body = json.dumps(obj).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            self._send(200, {"ok": True, "device": DEVICE, "loaded": MODEL is not None})
        else:
            self._send(404, {"ok": False})

    def do_POST(self):
        if self.path != "/tts":
            return self._send(404, {"ok": False})
        try:
            n = int(self.headers.get("Content-Length", "0"))
            req = json.loads(self.rfile.read(min(n, 1_000_000)).decode("utf-8"))
            secs = synth(req["text"], req["out"], req.get("language", "tr"), req.get("voice_ref"),
                         req.get("exaggeration", 0.5), req.get("cfg", 0.5))
            self._send(200, {"ok": True, "seconds": secs})
        except Exception as e:  # noqa: BLE001
            self._send(500, {"ok": False, "error": str(e)})

    def log_message(self, *a):
        pass


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--selftest")
    a = ap.parse_args()
    if a.selftest:
        s = synth("Merhaba, bu CrStudio Türkçe seslendirme denemesidir.", a.selftest)
        print(f"selftest ok: {s:.2f} sn ({DEVICE})")
        sys.exit(0)
    print(f"CrStudio Chatterbox sunucusu 127.0.0.1:{a.port} ({DEVICE})", flush=True)
    ThreadingHTTPServer(("127.0.0.1", a.port), H).serve_forever()
