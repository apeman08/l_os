#!/bin/sh
# Network tests: they need host-side servers (the guest reaches the host's
# loopback as 10.0.2.2 through QEMU's user-mode networking) and they check QEMU's
# own packet capture to prove that nothing is sent without authorization.
# `cargo test` boots each test through scripts/qemu_runner.sh (UEFI image,
# OVMF), which records the NIC's traffic in target/net-trace.pcap.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
mkdir -p target

pids=""
cleanup() { for p in $pids; do kill "$p" 2>/dev/null || true; done; }
trap cleanup EXIT

python3 scripts/netsrv/echo_server.py 18080 & pids="$pids $!"
python3 scripts/netsrv/dns_server.py 5353 & pids="$pids $!"

# TLS servers (openssl s_server, test-only certificates from tests/fixtures/tls)
fx=tests/fixtures/tls
tls() { # port cert key version-flag
  openssl s_server -accept "$1" -cert "$fx/$2.pem" -key "$fx/$3.key" "$4" -www -quiet </dev/null >/dev/null 2>&1 &
  pids="$pids $!"
}
tls 18443 server_ec        server_ec        -tls1_3
tls 18444 server_wrongname server_wrongname -tls1_3
tls 18445 server_rogue     server_rogue     -tls1_3
tls 18446 server_ec        server_ec        -tls1_2
tls 18447 server_rsa       server_rsa       -tls1_3
tls 18448 server_expired   server_expired   -tls1_3
tls 18449 server_future    server_future    -tls1_3
RECORD=target/plaintext-recorded.bin
python3 scripts/netsrv/record_server.py 18090 "$RECORD" & pids="$pids $!"
sleep 1

echo "== network_denied: nothing is ever authorized; the capture must stay empty"
rm -f target/net-trace.pcap
cargo test --features net-tests --test network_denied
# A pcap with no packets is just the 24-byte global header.
size=$(wc -c < target/net-trace.pcap)
if [ "$size" -ne 24 ]; then
  echo "FAIL: QEMU captured traffic ($size bytes) although nothing was authorized"
  exit 1
fi
echo "OK: QEMU's capture contains no frame ($size bytes = header only)"

echo "== network: authorized traffic against the host test servers"
cargo test --features net-tests --test network

echo "== tls: HTTPS from ring 3 (TLS 1.3 via kernel sockets)"
cargo test --features net-tests --test tls
# After the failed handshake with a non-TLS server: it must have received a TLS
# ClientHello (0x16 0x03) and no plaintext HTTP request of any kind.
first=$(head -c 2 "$RECORD" | od -An -tx1 | tr -d ' \n')
if [ "$first" != "1603" ]; then echo "FAIL: first bytes sent to the plaintext server were $first, not a TLS record"; exit 1; fi
if grep -a -q "GET " "$RECORD"; then echo "FAIL: a plaintext HTTP request reached the non-TLS server"; exit 1; fi
if grep -a -q -- "--MORE--" "$RECORD"; then echo "FAIL: the client kept sending after the failed handshake"; exit 1; fi
echo "OK: the non-TLS server received only a TLS ClientHello; no plaintext fallback"
