#!/bin/sh
# Network tests: they need host-side servers (the guest reaches the host's
# loopback as 10.0.2.2 through QEMU's user-mode networking) and they check QEMU's
# own packet capture to prove that nothing is sent without authorization.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
mkdir -p target

pids=""
cleanup() { for p in $pids; do kill "$p" 2>/dev/null || true; done; }
trap cleanup EXIT

python3 scripts/netsrv/echo_server.py 18080 & pids="$pids $!"
python3 scripts/netsrv/dns_server.py 5353 & pids="$pids $!"
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
