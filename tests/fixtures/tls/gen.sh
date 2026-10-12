#!/bin/sh
# Regenerates the TEST-ONLY certificates used by the TLS tests.
# The private keys here are public on purpose; never use them for anything real.
set -eu
cd "$(dirname "$0")"
rm -f *.pem *.der *.key *.csr *.srl *.cnf
DAYS=36500

ext() { # name, SAN list
  cat > "$1.cnf" <<CNF
[req]
distinguished_name = dn
[dn]
[v3_server]
basicConstraints = CA:FALSE
keyUsage = digitalSignature
extendedKeyUsage = serverAuth
subjectAltName = $2
CNF
}

# --- trusted test CA (ECDSA P-256)
openssl ecparam -name prime256v1 -genkey -noout -out ca.key
openssl req -x509 -new -key ca.key -sha256 -days $DAYS -subj "/CN=Larp OS Test CA" -out ca.pem \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign"

issue() { # base name, key type (ec|rsa), SAN, signing CA prefix
  base=$1; kind=$2; san=$3; ca=$4
  if [ "$kind" = ec ]; then openssl ecparam -name prime256v1 -genkey -noout -out "$base.key"
  else openssl genrsa -out "$base.key" 2048 2>/dev/null; fi
  ext "$base" "$san"
  openssl req -new -key "$base.key" -subj "/CN=$base" -out "$base.csr" -config "$base.cnf"
  openssl x509 -req -in "$base.csr" -CA "$ca.pem" -CAkey "$ca.key" -CAcreateserial -days $DAYS -sha256 \
    -extfile "$base.cnf" -extensions v3_server -out "$base.pem" 2>/dev/null
}

issue server_ec  ec  "DNS:test.example" ca
issue server_rsa rsa "DNS:test.example" ca
issue server_wrongname ec "DNS:other.example" ca

# --- a second CA that the client does NOT trust
openssl ecparam -name prime256v1 -genkey -noout -out rogue_ca.key
openssl req -x509 -new -key rogue_ca.key -sha256 -days $DAYS -subj "/CN=Rogue CA" -out rogue_ca.pem \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign"
issue server_rogue ec "DNS:test.example" rogue_ca

# --- self-signed leaf
openssl ecparam -name prime256v1 -genkey -noout -out server_selfsigned.key
ext server_selfsigned "DNS:test.example"
openssl req -x509 -new -key server_selfsigned.key -sha256 -days $DAYS -subj "/CN=selfsigned" \
  -config server_selfsigned.cnf -extensions v3_server -out server_selfsigned.pem

for f in ca server_ec; do openssl x509 -in $f.pem -outform der -out $f.der; done
rm -f *.csr *.srl *.cnf
ls
