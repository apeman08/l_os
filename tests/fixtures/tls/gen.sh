#!/bin/sh
# Regenerates the TEST-ONLY certificates used by the TLS tests.
# The private keys here are public on purpose; never use them for anything real.
set -eu
cd "$(dirname "$0")"
rm -rf *.pem *.der *.key *.csr *.srl *.cnf ca_db
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

# --- leaf certificates with fixed validity windows, issued by the trusted CA:
#     one that expired in January 2026 and one that becomes valid in 2099
mkdir ca_db; : > ca_db/index.txt; echo 1000 > ca_db/serial
cat > ca_issue.cnf <<CNF
[ca]
default_ca = CA_default
[CA_default]
database = ca_db/index.txt
new_certs_dir = ca_db
serial = ca_db/serial
default_md = sha256
policy = policy_any
unique_subject = no
[policy_any]
commonName = supplied
[v3_server]
basicConstraints = CA:FALSE
keyUsage = digitalSignature
extendedKeyUsage = serverAuth
subjectAltName = DNS:test.example
CNF
window() { # output name, start, end
  openssl req -new -key server_ec.key -subj "/CN=$1" -out "$1.csr"
  openssl ca -config ca_issue.cnf -batch -notext -cert ca.pem -keyfile ca.key -in "$1.csr" \
    -out "$1.pem" -startdate "$2" -enddate "$3" -extensions v3_server 2>/dev/null
  cp server_ec.key "$1.key"
}
window server_expired 20260101000000Z 20260102000000Z
window server_future  20990101000000Z 21000101000000Z

for f in ca server_ec; do openssl x509 -in $f.pem -outform der -out $f.der; done
rm -rf *.csr *.srl *.cnf ca_db
ls
