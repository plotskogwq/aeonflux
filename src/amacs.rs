// -*- mode: rust; -*-
//
// This file is part of aeonflux.
// Copyright (c) 2020 The Brave Authors
// See LICENSE for licensing information.
//
// Authors:
// - isis agora lovecruft <isis@patternsinthevoid.net>

//! Implementation of the MAC_GGM scheme in https://eprint.iacr.org/2019/1416.pdf.
//!
//! Algebraic Message Authentication Codes (or AMACs for short) are MACs with an
//! algebraic polynomial structure.  They are symmetrically keyed, meaning the
//! keypair used to create an AMAC must also be the keypair used to verify its
//! correctness.  Due to the symmetric setting and the algebraic structure, a
//! proof of correctness of the AMAC can be constructed which requires sending
//! only a vector of commitments to the AMAC.  This is the underlying primitive
//! used for our anonymous credential scheme.

#[cfg(all(not(feature = "std"), feature = "alloc"))]
use alloc::vec::Vec;
#[cfg(all(not(feature = "alloc"), feature = "std"))]
use std::vec::Vec;

use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::Identity;
use curve25519_dalek::traits::MultiscalarMul;

use rand_core::CryptoRng;
use rand_core::RngCore;

use serde::{self, Serialize, Deserialize, Serializer, Deserializer};
use serde::de::Visitor;

use zeroize::Zeroize;

use crate::errors::MacError;
use crate::parameters::SystemParameters;
use crate::symmetric::Plaintext;

/// Determine the size of a [`SecretKey`], in bytes.
pub(crate) fn sizeof_secret_key(number_of_attributes: u32) -> usize {
    32 * (5 + number_of_attributes) as usize + 4
}

/// An AMAC secret key is \(( (w, w', x_0, x_1, \vec{y_{n}}, W ) \in \mathbb{Z}_q \))
/// where \(( W := G_w * w \)). (The \(( G_w \)) is one of the orthogonal generators
/// from the [`SystemParameters`].)
#[derive(Clone, Debug)]
pub struct SecretKey {
    pub(crate) w: Scalar,
    pub(crate) w_prime: Scalar,
    pub(crate) x_0: Scalar,
    pub(crate) x_1: Scalar,
    pub(crate) y: Vec<Scalar>,
    pub(crate) W: RistrettoPoint,
}

// We can't derive this because generally in elliptic curve cryptography group
// elements aren't used as secrets, thus curve25519-dalek doesn't impl Zeroize
// for RistrettoPoint.
impl Zeroize for SecretKey {
    fn zeroize(&mut self) {
        self.w.zeroize();
        self.w_prime.zeroize();
        self.x_0.zeroize();
        self.x_1.zeroize();
        self.y.zeroize();

        self.W = RistrettoPoint::identity();
    }
}

/// Overwrite the secret key material with zeroes (and the identity element)
/// when it drops out of scope.
impl Drop for SecretKey {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl SecretKey {
    /// Given the [`SystemParameters`], generate a secret key.
    ///
    /// The size of the secret key is linear in the size of the desired number
    /// of attributes for the anonymous credential.
    pub fn generate<R>(csprng: &mut R, system_parameters: &SystemParameters) -> SecretKey
    where
        R: RngCore + CryptoRng,
    {
        let w:       Scalar = Scalar::random(csprng);
        let w_prime: Scalar = Scalar::random(csprng);
        let x_0:     Scalar = Scalar::random(csprng);
        let x_1:     Scalar = Scalar::random(csprng);

        let mut y: Vec<Scalar> = Vec::with_capacity(system_parameters.NUMBER_OF_ATTRIBUTES as usize);

        for _ in 0..system_parameters.NUMBER_OF_ATTRIBUTES {
            y.push(Scalar::random(csprng));
        }

        let W: RistrettoPoint = &system_parameters.G_w * &w;

        SecretKey { w, w_prime, x_0, x_1, y, W }
    }

    /// Serialise this AMAC secret key to a vector of bytes.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::with_capacity(sizeof_secret_key(self.y.len() as u32));

        bytes.extend(&(self.y.len() as u32).to_le_bytes());
        bytes.extend(self.w.as_bytes());
        bytes.extend(self.w_prime.as_bytes());
        bytes.extend(self.x_0.as_bytes());
        bytes.extend(self.x_1.as_bytes());

        for y in self.y.iter() {
            bytes.extend(y.as_bytes());
        }

        bytes.extend(self.W.compress().as_bytes());
        bytes
    }

    /// Attempt to deserialise this AMAC secret key from bytes.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<SecretKey, MacError> {
        // We assume no one is going to create a key for less that one attributes.
        if bytes.len() < sizeof_secret_key(1) {
            return Err(MacError::KeypairDeserialisation);
        }

        let mut index: usize = 0;
        let mut chunk: [u8; 32] = [0u8; 32];

        let mut tmp = [0u8; 4];

        tmp.copy_from_slice(&bytes[index..index+4]);
        let number_of_attributes = u32::from_le_bytes(tmp); index +=  4; chunk.copy_from_slice(&bytes[index..index+32]);
        let w       = Scalar::from_canonical_bytes(chunk)?; index += 32; chunk.copy_from_slice(&bytes[index..index+32]);
        let w_prime = Scalar::from_canonical_bytes(chunk)?; index += 32; chunk.copy_from_slice(&bytes[index..index+32]);
        let x_0     = Scalar::from_canonical_bytes(chunk)?; index += 32; chunk.copy_from_slice(&bytes[index..index+32]);
        let x_1     = Scalar::from_canonical_bytes(chunk)?; index += 32; chunk.copy_from_slice(&bytes[index..index+32]);

        let mut y: Vec<Scalar> = Vec::with_capacity(number_of_attributes as usize);

        for _ in 0..number_of_attributes {
            y.push(Scalar::from_canonical_bytes(chunk)?); index += 32;
        }

        let W = CompressedRistretto::from_slice(&bytes[index..index+32]).decompress()?;

        Ok(SecretKey{ w, w_prime, x_0, x_1, y, W })
    }
}

impl_serde_with_to_bytes_and_from_bytes!(SecretKey, "A valid byte sequence representing an amacs::SecretKey");

/// Attributes may be either group elements \(( M_i \in \mathbb{G} \)) or
/// scalars \(( m_j \in \mathbb{Z}_q \)), written as \(( M_j = G_m_j * m_j \))
/// where \(( G_m_j \)) is taken from the [`SystemParameters`].
///
/// When a `Credential` is shown, its attributes may be either revealed or
/// hidden from the credential issuer.  These represent all the valid attribute
/// types.
#[derive(Clone)]
pub enum Attribute {
    /// A scalar attribute which is revealed upon credential presentation.
    PublicScalar(Scalar),
    /// A scalar attribute which is hidden upon credential presentation.
    SecretScalar(Scalar),
    /// A group element attribute which is always revealed upon credential presentation.
    PublicPoint(RistrettoPoint),
    /// A group element attribute which can be hidden or revealed upon credential presentation.
    EitherPoint(Plaintext),
    /// A group element attribute which is hidden upon credential presentation.
    SecretPoint(Plaintext),
}

// We can't derive this because generally in elliptic curve cryptography group
// elements aren't used as secrets, thus curve25519-dalek doesn't impl Zeroize
// for RistrettoPoint.
impl Zeroize for Attribute {
    fn zeroize(&mut self) {
        match self {
            Attribute::SecretScalar(x) => x.zeroize(),
            Attribute::SecretPoint(x) => x.zeroize(),
            _ => return,
        }
    }
}

/// Overwrite the secret attributes with zeroes (and the identity element)
/// when it drops out of scope.
impl Drop for Attribute {
    fn drop(&mut self) {
        self.zeroize();
    }
}


/// These are the form of the attributes during credential presentation, when
/// some may be be hidden either by commiting to them and proving them in
/// zero-knowledge (as is the case for hidden scalar attributes) or by
/// encrypting them and proving the ciphertext's validity in zero-knowledge (as
/// is the case for the hidden group element attributes).
#[derive(Clone)]
pub enum EncryptedAttribute {
    /// A scalar attribute which is revealed upon credential presentation.
    PublicScalar(Scalar),
    /// A scalar attribute which is hidden upon credential presentation.
    SecretScalar,
    /// A group element attribute which is revealed upon credential presentation.
    PublicPoint(RistrettoPoint),
    /// A group element attribute which is hidden upon credential presentation.
    SecretPoint,
}

/// Messages are computed from `Attribute`s by scalar multiplying the scalar
/// portions by their respective generator in `SystemParameters.G_m`.
pub(crate) struct Messages(pub(crate) Vec<RistrettoPoint>);

impl Messages {
    pub(crate) fn from_attributes(
        attributes: &Vec<Attribute>,
        system_parameters: &SystemParameters
    ) -> Messages
    {
        let mut messages: Vec<RistrettoPoint> = Vec::with_capacity(attributes.len());

        for (i, attribute) in attributes.iter().enumerate() {
            let M_i: RistrettoPoint = match attribute {
                Attribute::PublicScalar(m) => m * system_parameters.G_m[i],
                Attribute::SecretScalar(m) => m * system_parameters.G_m[i],
                Attribute::PublicPoint(M)  => *M,
                Attribute::EitherPoint(p)  => p.M1,
                Attribute::SecretPoint(p)  => p.M1,
            };
            messages.push(M_i);
        }
        Messages(messages)
    }
}

/// An algebraic message authentication code, \(( (t,U,V) \in \mathbb{Z}_q \times \mathbb{G} \times \mathbb{G} \)).
pub(crate) struct Amac {
    pub(crate) t: Scalar,
    pub(crate) U: RistrettoPoint,
    pub(crate) V: RistrettoPoint,
}

impl Amac {
    /// Compute \(( V = W + (U (x_0 + x_1 t)) + \sigma{i=1}{n} M_i y_i \)).
    fn compute_V(
        system_parameters: &SystemParameters,
        secret_key: &SecretKey,
        attributes: &Vec<Attribute>,
        t: &Scalar,
        U: &RistrettoPoint,
    ) -> RistrettoPoint
    {
        let messages: Messages = Messages::from_attributes(attributes, system_parameters);

        // V = W + U * x_0 + U * x_1 * t
        let mut V: RistrettoPoint = secret_key.W + (U * secret_key.x_0) + (U * (secret_key.x_1 * t));

        // V = W + U * x_0 + U * x_1 + U * t + \sigma{i=1}{n} M_i y_i
        V += RistrettoPoint::multiscalar_mul(&secret_key.y[..], &messages.0[..]);
        V
    }

    /// Compute an algebraic message authentication code with a secret key for a
    /// vector of messages.
    pub(crate) fn tag<R>(
        csprng: &mut R,
        system_parameters: &SystemParameters,
        secret_key: &SecretKey,
        messages: &Vec<Attribute>,
    ) -> Result<Amac, MacError>
    where
        R: RngCore + CryptoRng,
    {
        if messages.len() > system_parameters.NUMBER_OF_ATTRIBUTES as usize {
            return Err(MacError::MessageLengthError{length: system_parameters.NUMBER_OF_ATTRIBUTES as usize});
        }

        let t: Scalar = Scalar::random(csprng);
        let U: RistrettoPoint = RistrettoPoint::random(csprng);
        let V: RistrettoPoint = Amac::compute_V(system_parameters, secret_key, messages, &t, &U);

        Ok(Amac { t, U, V })
    }

    /// Verify this algebraic MAC w.r.t. a secret key and vector of messages.
    #[allow(unused)] // We never actually call this function as the AMAC is verified indirectly in a NIZK.
    pub(crate) fn verify(
        &self,
        system_parameters: &SystemParameters,
        secret_key: &SecretKey,
        messages: &Vec<Attribute>,
    ) -> Result<(), MacError> {
        let V_prime = Amac::compute_V(system_parameters, secret_key, messages, &self.t, &self.U);

        if self.V == V_prime {
            return Ok(());
        }
        Err(MacError::AuthenticationError)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use rand::thread_rng;

    #[test]
    fn secret_key_generate() {
        let mut rng = thread_rng();
        let params = SystemParameters::generate(&mut rng, 2).unwrap();
        let sk = SecretKey::generate(&mut rng, &params);

        assert!(sk.w != Scalar::zero());
    }

    #[test]
    fn secret_key_from_bytes_2_attributes() {
        let mut rng = thread_rng();
        let params = SystemParameters::generate(&mut rng, 2).unwrap();
        let sk = SecretKey::generate(&mut rng, &params);
        let bytes = sk.to_bytes();
        let sk_prime = SecretKey::from_bytes(&bytes);

        assert!(sk_prime.is_ok());
    }

    #[test]
    fn secret_key_sizeof() {
        let mut rng = thread_rng();
        let params = SystemParameters::generate(&mut rng, 2).unwrap();
        let sk = SecretKey::generate(&mut rng, &params);
        let sizeof = sizeof_secret_key(2);
        let serialised = sk.to_bytes();

        // We use 4 bytes for storing the number of attributes.
        assert!(sizeof == serialised.len(), "{} != {}", sizeof, serialised.len());
    }

    #[test]
    fn amac_verification() {
        let mut rng = thread_rng();
        let params = SystemParameters::generate(&mut rng, 8).unwrap();
        let sk = SecretKey::generate(&mut rng, &params);
        let mut messages = Vec::new();

        let P1: Plaintext = (&[0u8; 30]).into();
        let P2: Plaintext = (&[1u8; 30]).into();
        let P3: Plaintext = (&[2u8; 30]).into();

        messages.push(Attribute::PublicScalar(Scalar::random(&mut rng)));
        messages.push(Attribute::SecretPoint(P1));
        messages.push(Attribute::PublicScalar(Scalar::random(&mut rng)));
        messages.push(Attribute::SecretPoint(P2));
        messages.push(Attribute::SecretPoint(P3));
        messages.push(Attribute::SecretScalar(Scalar::random(&mut rng)));
        messages.push(Attribute::PublicPoint(RistrettoPoint::random(&mut rng)));
        messages.push(Attribute::PublicScalar(Scalar::random(&mut rng)));

        let amac = Amac::tag(&mut rng, &params, &sk, &messages).unwrap();

        assert!(amac.verify(&params, &sk, &messages).is_ok());
    }
}
