use alloc::boxed::Box;
use crypto_common::OutputSizeUser;
use hmac::Mac;
use paste::paste;
use rustls::crypto;
use sha2::{Sha256, Sha384};

macro_rules! impl_hmac {
    ($name:ident, $ty:ty) => {
        paste! {
            #[allow(non_camel_case_types)]
            struct [<Hmac_ $ty>];

            impl crypto::hmac::Hmac for [<Hmac_ $ty>] {
                fn with_key(&self, key: &[u8]) -> Box<dyn crypto::hmac::Key> {
                    // HMAC accepts keys of any length, so this cannot fail.
                    let mac = hmac::Hmac::<$ty>::new_from_slice(key).expect("HMAC accepts any key length");
                    Box::new([<Key_ $ty>](mac))
                }
                fn hash_output_len(&self) -> usize {
                    $ty::output_size()
                }
            }

            #[allow(non_camel_case_types)]
            struct [<Key_ $ty>](hmac::Hmac<$ty>);

            impl crypto::hmac::Key for [<Key_ $ty>] {
                fn sign_concat(&self, first: &[u8], middle: &[&[u8]], last: &[u8]) -> crypto::hmac::Tag {
                    let mut ctx = self.0.clone();
                    ctx.update(first);
                    for m in middle {
                        ctx.update(m);
                    }
                    ctx.update(last);
                    crypto::hmac::Tag::new(&ctx.finalize().into_bytes()[..])
                }
                fn tag_len(&self) -> usize {
                    $ty::output_size()
                }
            }

            pub const $name: &dyn crypto::hmac::Hmac = &[<Hmac_ $ty>];
        }
    };
}

impl_hmac! {SHA256, Sha256}
impl_hmac! {SHA384, Sha384}
