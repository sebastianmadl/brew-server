//! TAA1 authentication algorithms for the authentication centre (AuC): the
//! session keys KS / KS' from K (EN 300 392-7 clause 4.1.1, ETSI TS 104 053-3
//! clauses 5.6 / 5.7), on the HURDLE-II block cipher.
//!
//! The server holds K and hands a Basestation only (RS, KS, KS'); the
//! Basestation derives RES1 / DCK1 (TA12) and RES2 / DCK2 (TA22) itself.
//! Ported from Bost FlowStation's `tetra-security` crate (Apache-2.0, same
//! reference vectors, Midnight Blue TETRA_crypto).
//!
//! Byte strings are written most significant byte first.

// The whole TAA1 set as in the reference: TA41 is for SCK delivery (Fleet Control phase 8b),
// TA12 / TA22 run on the station and are here to test the sets.
#![allow(dead_code)]

pub mod hurdle;

use hurdle::Hurdle;

/// Basic block structure BL1 (clause 5.2): the left 64-bit block is
/// enciphered under K, XORed into the right block, and that is enciphered
/// again — two-block CBC with a zero IV.
pub fn bl1(key: &[u8; 16], data: &[u8; 16]) -> [u8; 16] {
    let bc = Hurdle::new(key);
    let left: [u8; 8] = data[..8].try_into().expect("8 bytes");
    let right: [u8; 8] = data[8..].try_into().expect("8 bytes");
    let c1 = bc.encrypt(&left);
    let mut mixed = [0u8; 8];
    for i in 0..8 {
        mixed[i] = right[i] ^ c1[i];
    }
    let c2 = bc.encrypt(&mixed);
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&c1);
    out[8..].copy_from_slice(&c2);
    out
}

/// Expansion EXP1 (clause 5.4.1): 80 → 120 bits. Each adjacent pair of bytes
/// is followed by their XOR: B9 B8 A B7 B6 C … B1 B0 F.
pub fn exp1(b: &[u8; 10]) -> [u8; 15] {
    let mut out = [0u8; 15];
    for p in 0..5 {
        out[3 * p] = b[2 * p];
        out[3 * p + 1] = b[2 * p + 1];
        out[3 * p + 2] = b[2 * p] ^ b[2 * p + 1];
    }
    out
}

/// Expansion EXP2 (clause 5.4.2): EXP1 followed by G = A + C + D + E + F
/// mod 256 appended on the right.
pub fn exp2(b: &[u8; 10]) -> [u8; 16] {
    let e = exp1(b);
    let g = e[2].wrapping_add(e[5]).wrapping_add(e[8]).wrapping_add(e[11]).wrapping_add(e[14]);
    let mut out = [0u8; 16];
    out[..15].copy_from_slice(&e);
    out[15] = g;
    out
}

/// TA11 (and TA41): session authentication key KS = BL1_K(EXP2(RS))
/// (clause 5.6). `k` is the subscriber's authentication key K, `rs` the
/// random seed; TA41 is the same function used for the ESI key.
pub fn ta11(k: &[u8; 16], rs: &[u8; 10]) -> [u8; 16] {
    bl1(k, &exp2(rs))
}

/// TA41 is identical to TA11 (clause 5.6.0).
pub fn ta41(k: &[u8; 16], rs: &[u8; 10]) -> [u8; 16] {
    ta11(k, rs)
}

/// TA21: the infrastructure-authentication session key KS' — as TA11, but
/// with the bytes of RS reversed first (clause 5.6.2).
pub fn ta21(k: &[u8; 16], rs: &[u8; 10]) -> [u8; 16] {
    let mut rev = [0u8; 10];
    for i in 0..10 {
        rev[i] = rs[9 - i];
    }
    bl1(k, &exp2(&rev))
}

/// Result of TA12 / TA22: the 32-bit response and the 80-bit derived cipher key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthResult {
    /// RES1 (TA12) or RES2 (TA22) — what the challenged side sends back.
    pub res: [u8; 4],
    /// DCK1 (TA12) or DCK2 (TA22) — half of the derived cipher key.
    pub dck: [u8; 10],
}

/// TA12 (and TA22): from the session key KS and the challenge RAND, the
/// response and the derived cipher key (clause 5.7). With B15 … B0 the
/// output of BL1: RES = B15⊕B12 ‖ B9 ‖ B6 ‖ B3⊕B0 and DCK is the ten
/// remaining bytes B14 B13 B11 B10 B8 B7 B5 B4 B2 B1.
pub fn ta12(ks: &[u8; 16], rand: &[u8; 10]) -> AuthResult {
    let b = bl1(ks, &exp2(rand));
    AuthResult {
        res: [b[0] ^ b[3], b[6], b[9], b[12] ^ b[15]],
        dck: [b[1], b[2], b[4], b[5], b[7], b[8], b[10], b[11], b[13], b[14]],
    }
}

/// TA22 is identical to TA12 (clause 5.7.0), applied to KS' and RAND2.
pub fn ta22(ks: &[u8; 16], rand: &[u8; 10]) -> AuthResult {
    ta12(ks, rand)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h<const N: usize>(s: &str) -> [u8; N] {
        let v: Vec<u8> = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect();
        v.try_into().unwrap()
    }

    #[test]
    fn ta11_and_ta21_match_reference_vectors() {
        // Independent implementation (Midnight Blue, TETRA_crypto, Apache-2.0).
        let cases = [
            (
                "77e79fee7fc654dc6544644fdf476815",
                "00000000000000000000",
                "9C8451A35695D33C3094371202485453",
                "9C8451A35695D33C3094371202485453",
            ),
            (
                "c62e22850340bceb5552222860173d7e",
                "565a72d63cceed0b6f30",
                "77BC47F65C87C1E749B74FDEA6B54661",
                "FCFAF45592DFC65D8A1F5C45DCA293DA",
            ),
            (
                "4ebb689d874ad6417905c0edaa3f90ec",
                "935e49fcdcbb47581955",
                "489C79EA052FDEFA902A833F26CF127C",
                "5E4C241E21915A4807052942AF14ACCD",
            ),
            (
                "67fb134dd79c7d77f52a5dcef23de6fd",
                "b824ffb137a4ef87e07a",
                "B71421BA11CFD54AD6C4D257925A53B2",
                "AD310AEF61B06B2A6C8330C6145B7FEE",
            ),
        ];
        for (k, rs, ks, ksp) in cases {
            assert_eq!(ta11(&h(k), &h(rs)), h::<16>(ks), "TA11 k={k}");
            assert_eq!(ta21(&h(k), &h(rs)), h::<16>(ksp), "TA21 k={k}");
        }
    }

    #[test]
    fn ta12_matches_reference_vectors() {
        // Generated with the reference implementation's ta12_ta22().
        let cases = [
            (
                "9c8451a35695d33c3094371202485453",
                "00000000000000000000",
                "fd12ea2e",
                "dd5425ea9066f2b4b1d3",
            ),
            (
                "77bc47f65c87c1e749b74fdea6b54661",
                "565a72d63cceed0b6f30",
                "6484e85e",
                "d6fc1a70bf5987eb32e3",
            ),
        ];
        for (ks, rand, res, dck) in cases {
            let r = ta12(&h(ks), &h(rand));
            assert_eq!(r.res, h::<4>(res));
            assert_eq!(r.dck, h::<10>(dck));
        }
    }

}
