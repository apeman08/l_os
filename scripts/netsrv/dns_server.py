#!/usr/bin/env python3
"""Tiny DNS responder for the kernel network tests (UDP 127.0.0.1:5353).
test.example -> 10.0.2.2 ; everything else -> NXDOMAIN."""
import socket, struct, sys

NAMES = {"test.example": bytes([10, 0, 2, 2])}

def name_at(msg, pos):
    labels = []
    while msg[pos]:
        l = msg[pos]
        labels.append(msg[pos + 1:pos + 1 + l].decode())
        pos += 1 + l
    return ".".join(labels).lower(), pos + 1

def answer(msg):
    if len(msg) < 12:
        return None
    ident, flags, qd = struct.unpack("!HHH", msg[:6])
    name, pos = name_at(msg, 12)
    qtype, qclass = struct.unpack("!HH", msg[pos:pos + 4])
    question = msg[12:pos + 4]
    if name in NAMES and qtype == 1 and qclass == 1:
        head = struct.pack("!HHHHHH", ident, 0x8180, 1, 1, 0, 0)
        rr = b"\xc0\x0c" + struct.pack("!HHIH", 1, 1, 60, 4) + NAMES[name]
        return head + question + rr
    head = struct.pack("!HHHHHH", ident, 0x8183, 1, 0, 0, 0)  # NXDOMAIN
    return head + question

if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 5353
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("127.0.0.1", port))
    while True:
        data, addr = s.recvfrom(512)
        try:
            reply = answer(data)
        except Exception:
            reply = None
        if reply:
            s.sendto(reply, addr)
