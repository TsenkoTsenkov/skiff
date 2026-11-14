#!/usr/bin/env python3
"""Tiny HTTP server that fake pods' ClusterPortForward points at."""
import http.server, socketserver, sys, time
PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 18090
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = f"hello from a fake pod (port-forward works)\npath={self.path}\ntime={time.strftime('%FT%TZ', time.gmtime())}\n".encode()
        self.send_response(200); self.send_header("Content-Type", "text/plain"); self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def log_message(self, *a): pass
class S(socketserver.ThreadingMixIn, http.server.HTTPServer): daemon_threads = True; allow_reuse_address = True
S(("127.0.0.1", PORT), H).serve_forever()
