//! Ed25519 signatures (RFC 8032) and SHA-512, std-only.
//!
//! Used by the package system (`pkg.rs`) to sign and verify content-hash
//! strings. Field arithmetic is five 51-bit limbs mod 2^255-19, the group is
//! extended twisted Edwards coordinates, and scalars are reduced with plain
//! schoolbook big integers — sized for interactive signing and verification.

// ---------------------------------------------------------------------------
// SHA-512 (FIPS 180-4)
// ---------------------------------------------------------------------------

const K512: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

struct Sha512 {
    h: [u64; 8],
    buf: [u8; 128],
    buf_len: usize,
    total: u128,
}

impl Sha512 {
    fn new() -> Sha512 {
        Sha512 {
            h: [
                0x6a09e667f3bcc908,
                0xbb67ae8584caa73b,
                0x3c6ef372fe94f82b,
                0xa54ff53a5f1d36f1,
                0x510e527fade682d1,
                0x9b05688c2b3e6c1f,
                0x1f83d9abfb41bd6b,
                0x5be0cd19137e2179,
            ],
            buf: [0; 128],
            buf_len: 0,
            total: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u128;
        if self.buf_len > 0 {
            let take = (128 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 128 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
        while data.len() >= 128 {
            let mut block = [0u8; 128];
            block.copy_from_slice(&data[..128]);
            self.compress(&block);
            data = &data[128..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    fn finalize(mut self) -> [u8; 64] {
        let bits = self.total * 8;
        // 0x80, zeros to 112 mod 128, then the 128-bit big-endian bit length.
        let mut pad = [0u8; 128];
        pad[0] = 0x80;
        let zeros = (111 + 128 - self.buf_len) % 128;
        self.update(&pad[..1 + zeros]);
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 64];
        for i in 0..8 {
            out[8 * i..8 * i + 8].copy_from_slice(&self.h[i].to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 128]) {
        let mut w = [0u64; 80];
        for i in 0..16 {
            w[i] = u64::from_be_bytes(block[8 * i..8 * i + 8].try_into().unwrap());
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = self.h;
        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K512[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (i, v) in [a, b, c, d, e, f, g, hh].into_iter().enumerate() {
            self.h[i] = self.h[i].wrapping_add(v);
        }
    }
}

pub fn sha512(data: &[u8]) -> [u8; 64] {
    let mut st = Sha512::new();
    st.update(data);
    st.finalize()
}

// ---------------------------------------------------------------------------
// Field arithmetic mod p = 2^255 - 19 (five 51-bit limbs, little-endian)
// ---------------------------------------------------------------------------

const MASK51: u64 = (1 << 51) - 1;

#[derive(Clone, Copy)]
struct Fe([u64; 5]);

impl Fe {
    const ZERO: Fe = Fe([0, 0, 0, 0, 0]);
    const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    fn carry(&self) -> Fe {
        let mut t = self.0;
        for i in 0..4 {
            let c = t[i] >> 51;
            t[i + 1] += c;
            t[i] &= MASK51;
        }
        let c = t[4] >> 51;
        t[0] += 19 * c;
        t[4] &= MASK51;
        let c = t[0] >> 51;
        t[1] += c;
        t[0] &= MASK51;
        Fe(t)
    }

    fn from_bytes(s: &[u8; 32]) -> Fe {
        let ld = |i: usize| u64::from_le_bytes(s[i..i + 8].try_into().unwrap());
        Fe([
            ld(0) & MASK51,
            (ld(6) >> 3) & MASK51,
            (ld(12) >> 6) & MASK51,
            (ld(19) >> 1) & MASK51,
            (ld(24) >> 12) & MASK51,
        ])
    }

    // Canonical little-endian bytes of the representative in [0, p).
    fn to_bytes(&self) -> [u8; 32] {
        let mut t = self.carry().0;
        // q = 1 iff the value is >= p. Adding 19*q makes the 2^255 bit
        // overflow; dropping that bit below subtracts exactly one p
        // (2^255 = 19 mod p), cancelling the +19*q.
        let mut q = (t[0] + 19) >> 51;
        q = (t[1] + q) >> 51;
        q = (t[2] + q) >> 51;
        q = (t[3] + q) >> 51;
        q = (t[4] + q) >> 51;
        t[0] += 19 * q;
        let mut c;
        c = t[0] >> 51;
        t[1] += c;
        t[0] &= MASK51;
        c = t[1] >> 51;
        t[2] += c;
        t[1] &= MASK51;
        c = t[2] >> 51;
        t[3] += c;
        t[2] &= MASK51;
        c = t[3] >> 51;
        t[4] += c;
        t[3] &= MASK51;
        t[4] &= MASK51;
        let mut s = [0u8; 32];
        s[0..8].copy_from_slice(&(t[0] | (t[1] << 51)).to_le_bytes());
        s[8..16].copy_from_slice(&((t[1] >> 13) | (t[2] << 38)).to_le_bytes());
        s[16..24].copy_from_slice(&((t[2] >> 26) | (t[3] << 25)).to_le_bytes());
        s[24..32].copy_from_slice(&((t[3] >> 39) | (t[4] << 12)).to_le_bytes());
        s
    }
}

fn fe_mul(a: &Fe, b: &Fe) -> Fe {
    let (a, b) = (&a.0, &b.0);
    let f19 = |x: u64| (x as u128) * 19;
    let g = |i: usize| b[i] as u128;
    // 2^255 = 19 mod p folds the high product terms back into limbs 0..4.
    let mut r: [u128; 5] = [
        (a[0] as u128) * g(0) + f19(a[1]) * g(4) + f19(a[2]) * g(3) + f19(a[3]) * g(2)
            + f19(a[4]) * g(1),
        (a[0] as u128) * g(1) + (a[1] as u128) * g(0) + f19(a[2]) * g(4) + f19(a[3]) * g(3)
            + f19(a[4]) * g(2),
        (a[0] as u128) * g(2) + (a[1] as u128) * g(1) + (a[2] as u128) * g(0)
            + f19(a[3]) * g(4) + f19(a[4]) * g(3),
        (a[0] as u128) * g(3) + (a[1] as u128) * g(2) + (a[2] as u128) * g(1)
            + (a[3] as u128) * g(0) + f19(a[4]) * g(4),
        (a[0] as u128) * g(4) + (a[1] as u128) * g(3) + (a[2] as u128) * g(2)
            + (a[3] as u128) * g(1) + (a[4] as u128) * g(0),
    ];
    let mut t = [0u64; 5];
    for i in 0..4 {
        let c = r[i] >> 51;
        r[i + 1] += c;
        t[i] = r[i] as u64 & MASK51;
    }
    t[4] = r[4] as u64 & MASK51;
    t[0] += ((r[4] >> 51) as u64) * 19;
    let c = t[0] >> 51;
    t[1] += c;
    t[0] &= MASK51;
    Fe(t)
}

fn fe_sq(a: &Fe) -> Fe {
    fe_mul(a, a)
}

fn fe_add(a: &Fe, b: &Fe) -> Fe {
    let mut t = [0u64; 5];
    for i in 0..5 {
        t[i] = a.0[i] + b.0[i];
    }
    Fe(t).carry()
}

// Biases with 2p so every limb stays non-negative under the limb bound.
fn fe_sub(a: &Fe, b: &Fe) -> Fe {
    const P2: [u64; 5] = [
        4503599627370458,
        4503599627370494,
        4503599627370494,
        4503599627370494,
        4503599627370494,
    ];
    let mut t = [0u64; 5];
    for i in 0..5 {
        t[i] = a.0[i] + P2[i] - b.0[i];
    }
    Fe(t).carry()
}

fn fe_neg(a: &Fe) -> Fe {
    fe_sub(&Fe::ZERO, a)
}

fn fe_eq(a: &Fe, b: &Fe) -> bool {
    a.to_bytes() == b.to_bytes()
}

fn fe_pow(z: &Fe, e: &[u64; 4]) -> Fe {
    let mut r = Fe::ONE;
    for i in (0..256).rev() {
        r = fe_sq(&r);
        if (e[i / 64] >> (i % 64)) & 1 == 1 {
            r = fe_mul(&r, z);
        }
    }
    r
}

fn fe_invert(z: &Fe) -> Fe {
    // p - 2 = 2^255 - 21 as little-endian 256-bit words
    const P_MINUS_2: [u64; 4] = [
        0xffffffffffffffeb,
        0xffffffffffffffff,
        0xffffffffffffffff,
        0x7fffffffffffffff,
    ];
    fe_pow(z, &P_MINUS_2)
}

const D: Fe = Fe([
    929955233495203,
    466365720129213,
    1662059464998953,
    2033849074728123,
    1442794654840575,
]);
const D2: Fe = Fe([
    1859910466990425,
    932731440258426,
    1072319116312658,
    1815898335770999,
    633789495995903,
]);
const SQRTM1: Fe = Fe([
    1718705420411056,
    234908883556509,
    2233514472574048,
    2117202627021982,
    765476049583133,
]);
// (p+3)/8 = 2^252 - 2, the square-root candidate exponent for p = 5 mod 8,
// as little-endian 256-bit words.
const SQRT_EXP: [u64; 4] = [
    0xfffffffffffffffe,
    0xffffffffffffffff,
    0xffffffffffffffff,
    0x0fffffffffffffff,
];

// ---------------------------------------------------------------------------
// Group: extended twisted Edwards coordinates, x = X/Z, y = Y/Z, xy = T/Z
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Ge {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

const BASE: Ge = Ge {
    x: Fe([
        1738742601995546,
        1146398526822698,
        2070867633025821,
        562264141797630,
        587772402128613,
    ]),
    y: Fe([
        1801439850948184,
        1351079888211148,
        450359962737049,
        900719925474099,
        1801439850948198,
    ]),
    z: Fe([1, 0, 0, 0, 0]),
    t: Fe([
        1841354044333475,
        16398895984059,
        755974180946558,
        900171276175154,
        1821297809914039,
    ]),
};

// add-2008-hwcd-3 (a = -1, k = 2d); unified, complete for d a non-square.
fn ge_add(p: &Ge, q: &Ge) -> Ge {
    let a = fe_mul(&fe_sub(&p.y, &p.x), &fe_sub(&q.y, &q.x));
    let b = fe_mul(&fe_add(&p.y, &p.x), &fe_add(&q.y, &q.x));
    let c = fe_mul(&fe_mul(&p.t, &D2), &q.t);
    let d = fe_add(&fe_mul(&p.z, &q.z), &fe_mul(&p.z, &q.z));
    let e = fe_sub(&b, &a);
    let f = fe_sub(&d, &c);
    let g = fe_add(&d, &c);
    let h = fe_add(&b, &a);
    Ge {
        x: fe_mul(&e, &f),
        y: fe_mul(&g, &h),
        z: fe_mul(&f, &g),
        t: fe_mul(&e, &h),
    }
}

// dbl-2008-hwcd with a = -1 (D = -A).
fn ge_double(p: &Ge) -> Ge {
    let a = fe_sq(&p.x);
    let b = fe_sq(&p.y);
    let c = fe_add(&fe_sq(&p.z), &fe_sq(&p.z));
    let d = fe_neg(&a);
    let e = fe_sub(&fe_sub(&fe_sq(&fe_add(&p.x, &p.y)), &a), &b);
    let g = fe_add(&d, &b);
    let f = fe_sub(&g, &c);
    let h = fe_sub(&d, &b);
    Ge {
        x: fe_mul(&e, &f),
        y: fe_mul(&g, &h),
        z: fe_mul(&f, &g),
        t: fe_mul(&e, &h),
    }
}

fn ge_scalar_mul(p: &Ge, s: &[u8; 32]) -> Ge {
    let mut r = Ge {
        x: Fe::ZERO,
        y: Fe::ONE,
        z: Fe::ONE,
        t: Fe::ZERO,
    };
    for i in (0..256).rev() {
        r = ge_double(&r);
        if (s[i / 8] >> (i % 8)) & 1 == 1 {
            r = ge_add(&r, p);
        }
    }
    r
}

fn ge_eq(p: &Ge, q: &Ge) -> bool {
    fe_mul(&p.x, &q.z).to_bytes() == fe_mul(&q.x, &p.z).to_bytes()
        && fe_mul(&p.y, &q.z).to_bytes() == fe_mul(&q.y, &p.z).to_bytes()
}

fn ge_to_bytes(p: &Ge) -> [u8; 32] {
    let zinv = fe_invert(&p.z);
    let x = fe_mul(&p.x, &zinv).to_bytes();
    let mut out = fe_mul(&p.y, &zinv).to_bytes();
    out[31] |= (x[0] & 1) << 7;
    out
}

// RFC 8032 section 5.1.3: recover x from y and the sign bit.
fn ge_decompress(bytes: &[u8; 32]) -> Option<Ge> {
    let sign = (bytes[31] >> 7) & 1;
    let mut yb = *bytes;
    yb[31] &= 0x7f;
    let y = Fe::from_bytes(&yb);
    if y.to_bytes() != yb {
        return None; // y >= p
    }
    // x^2 = (y^2 - 1) / (d*y^2 + 1)
    let y2 = fe_mul(&y, &y);
    let u = fe_sub(&y2, &Fe::ONE);
    let v = fe_add(&fe_mul(&D, &y2), &Fe::ONE);
    let z = fe_mul(&u, &fe_invert(&v));
    let r = fe_pow(&z, &SQRT_EXP);
    let r2 = fe_mul(&r, &r);
    let x = if fe_eq(&r2, &z) {
        r
    } else if fe_eq(&fe_add(&r2, &z), &Fe::ZERO) {
        fe_mul(&r, &SQRTM1)
    } else {
        return None;
    };
    if fe_eq(&x, &Fe::ZERO) && sign == 1 {
        return None;
    }
    let x = if (x.to_bytes()[0] & 1) != sign {
        fe_neg(&x)
    } else {
        x
    };
    Some(Ge {
        x,
        y,
        z: Fe::ONE,
        t: fe_mul(&x, &y),
    })
}

// ---------------------------------------------------------------------------
// Scalars mod L = 2^252 + 27742317777372353535851937790883648493 (u64 words)
// ---------------------------------------------------------------------------

const L: [u64; 8] = [
    0x5812631a5cf5d3ed,
    0x14def9dea2f79cd6,
    0x0000000000000000,
    0x1000000000000000,
    0,
    0,
    0,
    0,
];

fn words_geq(a: &[u64; 8], b: &[u64; 8]) -> bool {
    for i in (0..8).rev() {
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}

fn words_sub(a: &mut [u64; 8], b: &[u64; 8]) {
    let mut borrow = 0u64;
    for i in 0..8 {
        let (x, b1) = a[i].overflowing_sub(b[i]);
        let (x, b2) = x.overflowing_sub(borrow);
        a[i] = x;
        borrow = (b1 as u64) | (b2 as u64);
    }
}

fn l_shifted(s: usize) -> [u64; 8] {
    let wsh = s / 64;
    let bsh = s % 64;
    let mut r = [0u64; 8];
    if wsh >= 8 {
        return r;
    }
    if bsh == 0 {
        r[wsh..].copy_from_slice(&L[..8 - wsh]);
    } else {
        for i in (wsh..8).rev() {
            let hi = if i > wsh {
                L[i - wsh - 1] >> (64 - bsh)
            } else {
                0
            };
            r[i] = (L[i - wsh] << bsh) | hi;
        }
    }
    r
}

// Shifted-subtract reduction: v < 2^512 in, v < L out.
fn reduce_mod_l(v: &mut [u64; 8]) {
    for s in (0..=259).rev() {
        let t = l_shifted(s);
        if words_geq(v, &t) {
            words_sub(v, &t);
        }
    }
}

// SHA-512 output (little-endian 512-bit integer) to a scalar in [0, L).
fn sc_reduce(h: &[u8; 64]) -> [u8; 32] {
    let mut v = [0u64; 8];
    for i in 0..8 {
        v[i] = u64::from_le_bytes(h[8 * i..8 * i + 8].try_into().unwrap());
    }
    reduce_mod_l(&mut v);
    let mut out = [0u8; 32];
    for i in 0..4 {
        out[8 * i..8 * i + 8].copy_from_slice(&v[i].to_le_bytes());
    }
    out
}

// (a*b + c) mod L for 32-byte little-endian a, b, c.
fn sc_muladd(a: &[u8; 32], b: &[u8; 32], c: &[u8; 32]) -> [u8; 32] {
    let load4 = |s: &[u8; 32]| -> [u64; 4] {
        let mut w = [0u64; 4];
        for i in 0..4 {
            w[i] = u64::from_le_bytes(s[8 * i..8 * i + 8].try_into().unwrap());
        }
        w
    };
    let aw = load4(a);
    let bw = load4(b);
    let mut r = [0u64; 8];
    for i in 0..4 {
        let mut carry: u128 = 0;
        for j in 0..4 {
            let t = (aw[i] as u128) * (bw[j] as u128) + (r[i + j] as u128) + carry;
            r[i + j] = t as u64;
            carry = t >> 64;
        }
        let t = (r[i + 4] as u128) + carry;
        r[i + 4] = t as u64;
        debug_assert!(t >> 64 == 0); // a*b < 2^508 fits 8 words
    }
    let cw = load4(c);
    let mut carry: u128 = 0;
    for i in 0..8 {
        let t = (r[i] as u128) + (if i < 4 { cw[i] as u128 } else { 0 }) + carry;
        r[i] = t as u64;
        carry = t >> 64;
    }
    debug_assert!(carry == 0); // a*b + c < 2^512
    reduce_mod_l(&mut r);
    let mut out = [0u8; 32];
    for i in 0..4 {
        out[8 * i..8 * i + 8].copy_from_slice(&r[i].to_le_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// Ed25519 (RFC 8032 section 5.1)
// ---------------------------------------------------------------------------

fn expand_seed(seed: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let h = sha512(seed);
    let mut a = [0u8; 32];
    a.copy_from_slice(&h[..32]);
    a[0] &= 248;
    a[31] &= 127;
    a[31] |= 64;
    let mut prefix = [0u8; 32];
    prefix.copy_from_slice(&h[32..]);
    (a, prefix)
}

/// Derive the compressed public key from a 32-byte seed.
pub fn public_from_seed(seed: &[u8; 32]) -> [u8; 32] {
    let (a, _) = expand_seed(seed);
    ge_to_bytes(&ge_scalar_mul(&BASE, &a))
}

/// Ed25519 signature over `msg` with a 32-byte seed (RFC 8032).
pub fn sign(seed: &[u8; 32], msg: &[u8]) -> [u8; 64] {
    let (a, prefix) = expand_seed(seed);
    let public = ge_to_bytes(&ge_scalar_mul(&BASE, &a));
    let mut hr = Sha512::new();
    hr.update(&prefix);
    hr.update(msg);
    let r = sc_reduce(&hr.finalize());
    let rb = ge_to_bytes(&ge_scalar_mul(&BASE, &r));
    let mut hk = Sha512::new();
    hk.update(&rb);
    hk.update(&public);
    hk.update(msg);
    let k = sc_reduce(&hk.finalize());
    let s = sc_muladd(&k, &a, &r);
    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&rb);
    sig[32..].copy_from_slice(&s);
    sig
}

/// Verify `sig` over `msg` against the 32-byte compressed public key.
pub fn verify(public: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    let mut rb = [0u8; 32];
    rb.copy_from_slice(&sig[..32]);
    let mut sb = [0u8; 32];
    sb.copy_from_slice(&sig[32..]);
    let a = match ge_decompress(public) {
        Some(p) => p,
        None => return false,
    };
    let r = match ge_decompress(&rb) {
        Some(p) => p,
        None => return false,
    };
    let mut hk = Sha512::new();
    hk.update(&rb);
    hk.update(public);
    hk.update(msg);
    let k = sc_reduce(&hk.finalize());
    // Non-cofactor check: [S]B == R + [k]A.
    let lhs = ge_scalar_mul(&BASE, &sb);
    let rhs = ge_add(&ge_scalar_mul(&a, &k), &r);
    ge_eq(&lhs, &rhs)
}

/// Parse 64 lowercase/uppercase hex chars into 32 bytes; None on bad input.
pub fn from_hex32(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let nibble = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = (nibble(b[2 * i])? << 4) | nibble(b[2 * i + 1])?;
    }
    Some(out)
}

/// Lowercase hex of 32/64 bytes.
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        s.as_bytes()
            .chunks(2)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
            .collect()
    }

    fn hex32(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        out.copy_from_slice(&unhex(s));
        out
    }

    #[test]
    fn sha512_vectors() {
        assert_eq!(
            to_hex(&sha512(b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(
            to_hex(&sha512(b"")),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
             47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
    }

    #[test]
    fn rfc8032_test1_empty_message() {
        let seed = hex32("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let expect_pub = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
        let expect_sig = "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155\
                          5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b";
        let public = public_from_seed(&seed);
        assert_eq!(to_hex(&public), expect_pub);
        let sig = sign(&seed, b"");
        assert_eq!(to_hex(&sig), expect_sig);
        assert!(verify(&public, b"", &sig));
        assert!(!verify(&public, b"x", &sig));
        let mut wrong_key = public;
        wrong_key[0] ^= 1;
        assert!(!verify(&wrong_key, b"", &sig));
    }

    #[test]
    fn rfc8032_test2_one_byte_message() {
        let seed = hex32("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb");
        let expect_pub = "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c";
        let expect_sig = "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da\
                          085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00";
        let public = public_from_seed(&seed);
        assert_eq!(to_hex(&public), expect_pub);
        let sig = sign(&seed, &[0x72]);
        assert_eq!(to_hex(&sig), expect_sig);
        assert!(verify(&public, &[0x72], &sig));
        assert!(!verify(&public, &[0x73], &sig));
        let mut tampered_r = sig;
        tampered_r[0] ^= 0x08;
        assert!(!verify(&public, &[0x72], &tampered_r));
        let mut tampered_s = sig;
        tampered_s[63] ^= 0x01;
        assert!(!verify(&public, &[0x72], &tampered_s));
    }

    #[test]
    fn rfc8032_test3_two_byte_message() {
        let seed = hex32("c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7");
        let expect_pub = "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025";
        let expect_sig = "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac\
                          18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a";
        let public = public_from_seed(&seed);
        assert_eq!(to_hex(&public), expect_pub);
        let sig = sign(&seed, &[0xaf, 0x82]);
        assert_eq!(to_hex(&sig), expect_sig);
        assert!(verify(&public, &[0xaf, 0x82], &sig));
    }

    #[test]
    fn roundtrip_71_byte_message() {
        let seed = core::array::from_fn(|i| (i as u8).wrapping_mul(7) ^ 0x5d);
        let other_seed = core::array::from_fn(|i| (i as u8).wrapping_mul(11) ^ 0x2b);
        let msg: Vec<u8> = (0..71u8).map(|i| b'a' + (i % 26)).collect();
        let public = public_from_seed(&seed);
        let sig = sign(&seed, &msg);
        assert_eq!(sig.len(), 64);
        assert!(verify(&public, &msg, &sig));
        assert!(!verify(&public_from_seed(&other_seed), &msg, &sig));
        assert!(!verify(&[0xff; 32], &msg, &sig)); // y >= p is not a valid key
    }

    #[test]
    fn field_canonicalization() {
        // Regression for the to_bytes q=1 path: limbs representing exactly p
        // (or p + k) must canonicalize to 0 (or k), not to 19 + k.
        let p_limbs = Fe([
            2251799813685229,
            2251799813685247,
            2251799813685247,
            2251799813685247,
            2251799813685247,
        ]);
        assert_eq!(p_limbs.to_bytes(), [0u8; 32]);
        for k in 1u64..64 {
            let mut t = p_limbs.0;
            t[0] += k;
            let b = Fe(t).to_bytes();
            assert_eq!(b[0], k as u8);
            assert_eq!(&b[1..], &[0u8; 31][..]);
        }
        assert_eq!(fe_add(&p_limbs, &p_limbs).to_bytes(), [0u8; 32]);
    }

    #[test]
    fn randomized_sign_verify_roundtrip() {
        let mut x = 0x243f6a8885a308d3u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for len in [0usize, 1, 31, 32, 33, 64, 71, 128, 111, 112, 113, 1000] {
            let seed = core::array::from_fn(|_| (next() & 0xff) as u8);
            let msg: Vec<u8> = (0..len).map(|_| (next() & 0xff) as u8).collect();
            let public = public_from_seed(&seed);
            let sig = sign(&seed, &msg);
            assert!(verify(&public, &msg, &sig), "verify failed at len {len}");
            if len > 0 {
                let mut tampered = msg.clone();
                tampered[0] ^= 1;
                assert!(
                    !verify(&public, &tampered, &sig),
                    "tampered verify passed at len {len}"
                );
            }
            let mut bad_sig = sig;
            bad_sig[next() as usize % 64] ^= 1 << (next() % 8);
            assert!(
                !verify(&public, &msg, &bad_sig),
                "bad sig passed at len {len}"
            );
        }
    }

    #[test]
    fn hex_roundtrip() {
        let bytes = core::array::from_fn(|i| (i as u8).wrapping_mul(8).wrapping_add(0x11));
        let hex = to_hex(&bytes);
        assert_eq!(hex.len(), 64);
        assert!(hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(from_hex32(&hex), Some(bytes));
        let upper = hex.to_ascii_uppercase();
        assert_eq!(from_hex32(&upper), Some(bytes));
        assert_eq!(from_hex32(""), None);
        assert_eq!(from_hex32("00"), None);
        assert_eq!(from_hex32(&hex[..63]), None);
        assert_eq!(from_hex32(&format!("{}0", hex)), None);
        let mut bad = hex.into_bytes();
        bad[0] = b'g';
        assert_eq!(from_hex32(std::str::from_utf8(&bad).unwrap()), None);
    }
}
