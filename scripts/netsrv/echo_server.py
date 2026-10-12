#!/usr/bin/env python3
"""TCP echo server for the kernel network tests (127.0.0.1:18080)."""
import socketserver, sys

class Echo(socketserver.BaseRequestHandler):
    def handle(self):
        while True:
            data = self.request.recv(4096)
            if not data:
                return
            self.request.sendall(data)

class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 18080
    with Server(("127.0.0.1", port), Echo) as s:
        s.serve_forever()
