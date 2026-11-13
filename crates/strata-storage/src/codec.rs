//! Integer compression primitives used by the on-disk postings format.
//!
//! * **Bit packing**: store `n` integers using exactly `b` bits each, where `b`
//!   is the number of bits of the largest one. Postings are stored as *deltas*
//!   between consecutive doc ids (`[3, 7, 8, 20]` -> `[3, 4, 1, 12]`), which are
//!   small, so `b` is small (often 4-8 bits instead of 32).
//! * **Varint** (LEB128): 7 payload bits per byte, high bit = "more bytes
//!   follow". Used for positions and headers, where values vary a lot.
//!
//! Both are classic search-engine techniques (Lucene uses packed ints + vInts;
//! Tantivy bitpacks blocks of 128). Our versions are scalar and simple; the
//! decoder processes one value per loop iteration.

/// Number of bits needed to represent `max` (0 for 0).
pub fn bits_needed(max: u32) -> u8 {
    (32 - max.leading_zeros()) as u8
}

/// Append `values`, each stored in `bits` bits, LSB-first, to `out`.
/// Writes exactly `ceil(values.len() * bits / 8)` bytes.
///
/// # Panics
/// If a value does not fit in `bits` bits.
pub fn pack(values: &[u32], bits: u8, out: &mut Vec<u8>) {
    if bits == 0 {
        assert!(values.iter().all(|&v| v == 0));
        return;
    }
    // `acc` is a small bit buffer: we push values in at the top and flush
    // whole bytes out of the bottom.
    let mut acc: u64 = 0;
    let mut acc_bits: u32 = 0;
    for &v in values {
        assert!(
            bits == 32 || v < (1u32 << bits),
            "{v} does not fit in {bits} bits"
        );
        acc |= u64::from(v) << acc_bits;
        acc_bits += u32::from(bits);
        while acc_bits >= 8 {
            out.push(acc as u8);
            acc >>= 8;
            acc_bits -= 8;
        }
    }
    if acc_bits > 0 {
        out.push(acc as u8);
    }
}

/// Number of bytes [`pack`] writes for `n` values of `bits` bits.
pub fn packed_len(n: usize, bits: u8) -> usize {
    (n * bits as usize).div_ceil(8)
}

/// Decode `out.len()` values of `bits` bits from the start of `input`.
/// Returns the number of bytes consumed.
pub fn unpack(input: &[u8], bits: u8, out: &mut [u32]) -> usize {
    let len = packed_len(out.len(), bits);
    if bits == 0 {
        out.fill(0);
        return 0;
    }
    let mask: u64 = (1u64 << bits) - 1;
    let mut acc: u64 = 0;
    let mut acc_bits: u32 = 0;
    let mut pos = 0;
    for slot in out.iter_mut() {
        while acc_bits < u32::from(bits) {
            acc |= u64::from(input[pos]) << acc_bits;
            pos += 1;
            acc_bits += 8;
        }
        *slot = (acc & mask) as u32;
        acc >>= bits;
        acc_bits -= u32::from(bits);
    }
    debug_assert_eq!(pos, len);
    len
}

/// Append `v` as a LEB128 varint.
pub fn write_varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Read a varint starting at `*pos`, advancing `*pos` past it.
pub fn read_varint(input: &[u8], pos: &mut usize) -> u64 {
    let mut v: u64 = 0;
    let mut shift = 0;
    loop {
        let byte = input[*pos];
        *pos += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return v;
        }
        shift += 7;
    }
}

/// Little-endian fixed-width helpers over byte slices.
pub fn read_u32(input: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(input[at..at + 4].try_into().expect("4 bytes"))
}

pub fn read_u64(input: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(input[at..at + 8].try_into().expect("8 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn bits_needed_examples() {
        assert_eq!(bits_needed(0), 0);
        assert_eq!(bits_needed(1), 1);
        assert_eq!(bits_needed(2), 2);
        assert_eq!(bits_needed(255), 8);
        assert_eq!(bits_needed(256), 9);
        assert_eq!(bits_needed(u32::MAX), 32);
    }

    #[test]
    fn pack_hand_example() {
        // 3 values of 3 bits: 0b101, 0b011, 0b111 -> bits LSB-first:
        // 101 110 111 (value order) -> byte0 = 0b11_011_101, byte1 = 0b1
        let mut out = Vec::new();
        pack(&[0b101, 0b011, 0b111], 3, &mut out);
        assert_eq!(out, vec![0b1101_1101, 0b0000_0001]);
        let mut back = [0u32; 3];
        assert_eq!(unpack(&out, 3, &mut back), 2);
        assert_eq!(back, [0b101, 0b011, 0b111]);
    }

    #[test]
    fn varint_examples() {
        for (v, bytes) in [
            (0u64, vec![0u8]),
            (127, vec![0x7f]),
            (128, vec![0x80, 0x01]),
            (300, vec![0xac, 0x02]),
        ] {
            let mut out = Vec::new();
            write_varint(v, &mut out);
            assert_eq!(out, bytes);
            let mut pos = 0;
            assert_eq!(read_varint(&out, &mut pos), v);
            assert_eq!(pos, out.len());
        }
    }

    proptest! {
        #[test]
        fn pack_roundtrip(values in prop::collection::vec(any::<u32>(), 0..300), shift in 0u32..32) {
            let values: Vec<u32> = values.iter().map(|v| v >> shift).collect();
            let bits = bits_needed(values.iter().copied().max().unwrap_or(0));
            let mut out = vec![0xAA]; // pre-existing data must be preserved
            pack(&values, bits, &mut out);
            prop_assert_eq!(out.len(), 1 + packed_len(values.len(), bits));
            let mut back = vec![0u32; values.len()];
            let used = unpack(&out[1..], bits, &mut back);
            prop_assert_eq!(used, packed_len(values.len(), bits));
            prop_assert_eq!(back, values);
        }

        #[test]
        fn varint_roundtrip(values in prop::collection::vec(any::<u64>(), 0..100)) {
            let mut out = Vec::new();
            for &v in &values { write_varint(v, &mut out); }
            let mut pos = 0;
            for &v in &values { prop_assert_eq!(read_varint(&out, &mut pos), v); }
            prop_assert_eq!(pos, out.len());
        }
    }
}
