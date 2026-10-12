#!/usr/bin/env python3
"""Plaintext server that records everything it receives (first connection data
appended to a file) and answers with a plaintext HTTP reply. Used to prove that
a failed TLS handshake never makes the client fall back to plaintext HTTP."""
import socketserver, sys

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 18090
OUT = sys.argv[2] if len(sys.argv) > 2 else "/tmp/larp_recorded.bin"

class H(socketserver.BaseRequestHandler):
    def handle(self):
        data = self.request.recv(8192)
        with open(OUT, "ab") as f:
            f.write(data)
            f.write(b"\n--END--\n")
        try:
            self.request.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello")
            # keep reading so a plaintext fallback would also be recorded
            self.request.settimeout(1.0)
            more = self.request.recv(8192)
            if more:
                with open(OUT, "ab") as f:
                    f.write(more)
                    f.write(b"\n--MORE--\n")
        except Exception:
            pass

class S(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

if __name__ == "__main__":
    open(OUT, "wb").close()
    with S(("127.0.0.1", PORT), H) as s:
        s.serve_forever()
