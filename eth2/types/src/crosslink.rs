use crate::test_utils::TestRandom;
use crate::{Hash256, Slot};
use rand::RngCore;
use serde_derive::Serialize;
use ssz::{hash, Decodable, DecodeError, Encodable, SszStream, TreeHash};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Crosslink {
    pub slot: Slot,
    pub shard_block_root: Hash256,
}

impl Crosslink {
    /// Generates a new instance where `dynasty` and `hash` are both zero.
    pub fn zero() -> Self {
        Self {
            slot: Slot::from(0_u64),
            shard_block_root: Hash256::zero(),
        }
    }
}

impl Encodable for Crosslink {
    fn ssz_append(&self, s: &mut SszStream) {
        s.append(&self.slot);
        s.append(&self.shard_block_root);
    }
}

impl Decodable for Crosslink {
    fn ssz_decode(bytes: &[u8], i: usize) -> Result<(Self, usize), DecodeError> {
        let (slot, i) = <_>::ssz_decode(bytes, i)?;
        let (shard_block_root, i) = <_>::ssz_decode(bytes, i)?;

        Ok((
            Self {
                slot,
                shard_block_root,
            },
            i,
        ))
    }
}

impl TreeHash for Crosslink {
    fn hash_tree_root(&self) -> Vec<u8> {
        let mut result: Vec<u8> = vec![];
        result.append(&mut self.slot.hash_tree_root());
        result.append(&mut self.shard_block_root.hash_tree_root());
        hash(&result)
    }
}

impl<T: RngCore> TestRandom<T> for Crosslink {
    fn random_for_test(rng: &mut T) -> Self {
        Self {
            slot: <_>::random_for_test(rng),
            shard_block_root: <_>::random_for_test(rng),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{SeedableRng, TestRandom, XorShiftRng};
    use ssz::ssz_encode;

    #[test]
    pub fn test_ssz_round_trip() {
        let mut rng = XorShiftRng::from_seed([42; 16]);
        let original = Crosslink::random_for_test(&mut rng);

        let bytes = ssz_encode(&original);
        let (decoded, _) = <_>::ssz_decode(&bytes, 0).unwrap();

        assert_eq!(original, decoded);
    }

    #[test]
    pub fn test_hash_tree_root() {
        let mut rng = XorShiftRng::from_seed([42; 16]);
        let original = Crosslink::random_for_test(&mut rng);

        let result = original.hash_tree_root();

        assert_eq!(result.len(), 32);
        // TODO: Add further tests
        // https://github.com/sigp/lighthouse/issues/170
    }
}
