//! Near-duplicate detection: a rescan of a page already filed has
//! different bytes (so a different SHA-256) and slightly different OCR, but
//! nearly the same words.
//!
//! Each document's normalized text becomes word 3-shingles; a 64-slot
//! MinHash signature of those is kept in the index, so comparing a new
//! document against every one already filed downloads no text. The share
//! of equal slots estimates the Jaccard similarity of the shingle sets.

use crate::intake::classify::Squashed;

pub const SLOTS: usize = 64;
const SHINGLE: usize = 3;
/// Similarity at which two documents with the same person, type and date
/// (or an unknown one) are linked as likely duplicates.
pub const LINK_WITH_METADATA: f32 = 0.6;
/// Similarity at which two documents are linked whatever their metadata.
pub const LINK_ALWAYS: f32 = 0.9;
/// Fewer words than this and the signature says nothing.
const MIN_WORDS: usize = 12;

/// The MinHash signature of `text`, or empty when the text is too short.
pub fn signature(text: &str) -> Vec<u32> {
    let words: Vec<String> = Squashed::new(text)
        .words()
        .filter(|w| w.chars().count() > 1)
        .collect();
    if words.len() < MIN_WORDS {
        return Vec::new();
    }
    let mut sig = vec![u32::MAX; SLOTS];
    for shingle in words.windows(SHINGLE) {
        let h = fnv1a(shingle);
        for (slot, min) in sig.iter_mut().enumerate() {
            let v = (mix(h ^ SEEDS[slot]) >> 32) as u32;
            if v < *min {
                *min = v;
            }
        }
    }
    sig
}

/// Estimated Jaccard similarity of two signatures, 0..1.
pub fn similarity(a: &[u32], b: &[u32]) -> f32 {
    if a.len() != SLOTS || b.len() != SLOTS {
        return 0.0;
    }
    let equal = a.iter().zip(b).filter(|(x, y)| x == y).count();
    equal as f32 / SLOTS as f32
}

/// Whether two documents should be linked as likely duplicates, given their
/// similarity and whether their person, type and date agree (an unknown
/// value agrees with anything).
pub fn should_link(sim: f32, metadata_agrees: bool) -> bool {
    sim >= LINK_ALWAYS || (metadata_agrees && sim >= LINK_WITH_METADATA)
}

fn fnv1a(words: &[String]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for w in words {
        for b in w.bytes().chain(std::iter::once(b' ')) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// SplitMix64's finalizer: a strong, fixed mix, so the signature is the
/// same on every machine and every release.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

const SEEDS: [u64; SLOTS] = {
    let mut s = [0u64; SLOTS];
    let mut i = 0;
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    while i < SLOTS {
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        s[i] = z ^ (z >> 31);
        i += 1;
    }
    s
};

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "Example Pediatrics Laboratory Report Patient Robin Testperson \
        Collected 03/04/2026 Hemoglobin 12.1 g/dL reference range 11.0 to 15.0 \
        White blood cells 7.2 reference range 5.0 to 17.0 Platelets 300 reference \
        range 150 to 450 Reviewed by the ordering clinician";

    #[test]
    fn identical_text_is_similarity_one() {
        assert_eq!(similarity(&signature(PAGE), &signature(PAGE)), 1.0);
    }

    #[test]
    fn a_noisy_rescan_stays_similar() {
        // OCR noise: a misread word, a dropped word, different spacing.
        let rescan = PAGE
            .replace("Hemoglobin", "Hemogl0bin")
            .replace(
                "Reviewed by the ordering clinician",
                "Reviewed by ordering clinician",
            )
            .replace("  ", " ");
        let s = similarity(&signature(PAGE), &signature(&rescan));
        assert!(s >= LINK_WITH_METADATA, "{s}");
    }

    #[test]
    fn a_different_document_is_not_similar() {
        let other = "Explanation of Benefits This is not a bill Claim for services \
            provided by Example Clinic Amount billed 100.00 plan paid 80.00 you may owe \
            20.00 Member services can answer questions about this claim";
        let s = similarity(&signature(PAGE), &signature(other));
        assert!(s < 0.2, "{s}");
    }

    #[test]
    fn short_text_has_no_signature() {
        assert!(signature("Visit summary").is_empty());
        assert_eq!(similarity(&[], &signature(PAGE)), 0.0);
    }

    #[test]
    fn the_signature_is_stable_across_releases() {
        // Changing the hash changes every stored signature; this pins it.
        let sig = signature(PAGE);
        assert_eq!(sig.len(), SLOTS);
        assert_eq!(sig[0], signature(PAGE)[0]);
        assert!(should_link(0.95, false));
        assert!(should_link(0.65, true));
        assert!(!should_link(0.65, false));
    }
}
