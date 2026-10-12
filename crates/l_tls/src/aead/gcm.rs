use super::{DecryptBufferAdapter, EncryptBufferAdapter};
use aead::AeadInPlace;
use alloc::boxed::Box;
use crypto_common::{KeyInit, KeySizeUser};
use paste::paste;
use rustls::crypto::cipher::{
    self, AeadKey, InboundOpaqueMessage, InboundPlainMessage, MessageDecrypter, MessageEncrypter,
    OutboundOpaqueMessage, OutboundPlainMessage, PrefixedPayload, Tls13AeadAlgorithm,
};
use rustls::{ConnectionTrafficSecrets, ContentType, ProtocolVersion};

macro_rules! impl_gcm_tls13 {
    ($name:ident, $aead:ty, $secrets:ident) => {
        paste! {
            pub struct [<Tls13 $name>];

            impl Tls13AeadAlgorithm for [<Tls13 $name>] {
                fn encrypter(&self, key: AeadKey, iv: cipher::Iv) -> Box<dyn MessageEncrypter> {
                    Box::new([<Cipher $name>](new_cipher::<$aead>(&key), iv))
                }

                fn decrypter(&self, key: AeadKey, iv: cipher::Iv) -> Box<dyn MessageDecrypter> {
                    Box::new([<Cipher $name>](new_cipher::<$aead>(&key), iv))
                }

                fn key_len(&self) -> usize {
                    <$aead>::key_size()
                }

                fn extract_keys(
                    &self,
                    key: AeadKey,
                    iv: cipher::Iv,
                ) -> Result<ConnectionTrafficSecrets, cipher::UnsupportedOperationError> {
                    Ok(ConnectionTrafficSecrets::$secrets { key, iv })
                }
            }

            struct [<Cipher $name>]($aead, cipher::Iv);

            impl MessageEncrypter for [<Cipher $name>] {
                fn encrypt(&mut self, m: OutboundPlainMessage<'_>, seq: u64) -> Result<OutboundOpaqueMessage, rustls::Error> {
                    let total_len = self.encrypted_payload_len(m.payload.len());
                    let mut payload = PrefixedPayload::with_capacity(total_len);

                    let nonce = cipher::Nonce::new(&self.1, seq).0;
                    let aad = cipher::make_tls13_aad(total_len);
                    payload.extend_from_chunks(&m.payload);
                    payload.extend_from_slice(&m.typ.to_array());

                    self.0
                        .encrypt_in_place(&nonce.into(), &aad, &mut EncryptBufferAdapter(&mut payload))
                        .map_err(|_| rustls::Error::EncryptError)
                        .map(|_| OutboundOpaqueMessage::new(ContentType::ApplicationData, ProtocolVersion::TLSv1_2, payload))
                }

                fn encrypted_payload_len(&self, payload_len: usize) -> usize {
                    payload_len + 1 + 16
                }
            }

            impl MessageDecrypter for [<Cipher $name>] {
                fn decrypt<'a>(&mut self, mut m: InboundOpaqueMessage<'a>, seq: u64) -> Result<InboundPlainMessage<'a>, rustls::Error> {
                    let payload = &mut m.payload;
                    let nonce = cipher::Nonce::new(&self.1, seq).0;
                    let aad = cipher::make_tls13_aad(payload.len());

                    self.0
                        .decrypt_in_place(&nonce.into(), &aad, &mut DecryptBufferAdapter(payload))
                        .map_err(|_| rustls::Error::DecryptError)?;

                    m.into_tls13_unpadded_message()
                }
            }
        }
    };
}

/// The key length is fixed by the suite, so a mismatch is a bug in rustls, not
/// something a peer can cause.
fn new_cipher<A: KeyInit>(key: &AeadKey) -> A {
    A::new_from_slice(key.as_ref()).expect("rustls passes a key of the suite's length")
}

impl_gcm_tls13! {Aes128Gcm, aes_gcm::Aes128Gcm, Aes128Gcm}
impl_gcm_tls13! {Aes256Gcm, aes_gcm::Aes256Gcm, Aes256Gcm}
