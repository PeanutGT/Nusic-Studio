//! Python's `random.Random(text).choice(seq)`, bit for bit.
//!
//! v3.7 picks a style's key with `random.Random(f"{style}|{description}|{seed}")`, so the
//! dashboard preview and the generated take agree. To keep the same picks, this reproduces
//! CPython: string seeds (version 2) become the integer `bytes + sha512(bytes)` (big-endian),
//! fed to MT19937's `init_by_array` as 32-bit words, least significant first.

use sha2::{Digest, Sha512};

const N: usize = 624;
const M: usize = 397;

pub struct PyRandom {
    mt: [u32; N],
    index: usize,
}

impl PyRandom {
    pub fn from_str_seed(text: &str) -> Self {
        let mut bytes = text.as_bytes().to_vec();
        bytes.extend_from_slice(&Sha512::digest(text.as_bytes()));
        // Big-endian integer -> 32-bit words, least significant first.
        bytes.reverse();
        let mut key: Vec<u32> = bytes
            .chunks(4)
            .map(|c| c.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (8 * i))))
            .collect();
        while key.len() > 1 && key.last() == Some(&0) {
            key.pop();
        }
        let mut r = Self { mt: [0; N], index: N };
        r.init_by_array(&key);
        r
    }

    fn init_genrand(&mut self, s: u32) {
        self.mt[0] = s;
        for i in 1..N {
            let prev = self.mt[i - 1];
            self.mt[i] = 1_812_433_253u32.wrapping_mul(prev ^ (prev >> 30)).wrapping_add(i as u32);
        }
        self.index = N;
    }

    fn init_by_array(&mut self, key: &[u32]) {
        self.init_genrand(19_650_218);
        let (mut i, mut j) = (1usize, 0usize);
        for _ in 0..N.max(key.len()) {
            let prev = self.mt[i - 1];
            self.mt[i] = (self.mt[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1_664_525))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= N {
                self.mt[0] = self.mt[N - 1];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
        }
        for _ in 0..N - 1 {
            let prev = self.mt[i - 1];
            self.mt[i] = (self.mt[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1_566_083_941)).wrapping_sub(i as u32);
            i += 1;
            if i >= N {
                self.mt[0] = self.mt[N - 1];
                i = 1;
            }
        }
        self.mt[0] = 0x8000_0000;
    }

    fn next_u32(&mut self) -> u32 {
        if self.index >= N {
            for k in 0..N {
                let y = (self.mt[k] & 0x8000_0000) | (self.mt[(k + 1) % N] & 0x7fff_ffff);
                let mut v = self.mt[(k + M) % N] ^ (y >> 1);
                if y & 1 != 0 {
                    v ^= 0x9908_b0df;
                }
                self.mt[k] = v;
            }
            self.index = 0;
        }
        let mut y = self.mt[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }

    /// `random.getrandbits(k)` for 1 <= k <= 32.
    fn getrandbits(&mut self, k: u32) -> u32 {
        self.next_u32() >> (32 - k)
    }

    /// `random._randbelow(n)`: rejection sampling on `n.bit_length()` bits.
    fn randbelow(&mut self, n: usize) -> usize {
        let k = usize::BITS - n.leading_zeros();
        loop {
            let r = self.getrandbits(k) as usize;
            if r < n {
                return r;
            }
        }
    }

    /// `random.choice(seq)`. `None` for an empty slice (Python raises).
    pub fn choice<'a, T>(&mut self, seq: &'a [T]) -> Option<&'a T> {
        if seq.is_empty() { None } else { Some(&seq[self.randbelow(seq.len())]) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_cpython_known_pick() {
        // python -c "import random; print(random.Random('abc').choice(range(1000)))" -> 790
        let r: Vec<u32> = (0..1000).collect();
        assert_eq!(*PyRandom::from_str_seed("abc").choice(&r).unwrap(), 790);
    }

    #[test]
    fn empty_choice_is_none() {
        let empty: [u8; 0] = [];
        assert!(PyRandom::from_str_seed("x").choice(&empty).is_none());
    }
}
