pub mod test_utils;
mod traits;

use slot_clock::SlotClock;
use ssz::ssz_encode;
use std::sync::Arc;
use types::{BeaconBlock, ChainSpec, Hash256, ProposalSignedData, PublicKey};

pub use self::traits::{
    BeaconNode, BeaconNodeError, DutiesReader, DutiesReaderError, PublishOutcome, Signer,
};

#[derive(Debug, PartialEq)]
pub enum PollOutcome {
    /// A new block was produced.
    BlockProduced(u64),
    /// A block was not produced as it would have been slashable.
    SlashableBlockNotProduced(u64),
    /// The validator duties did not require a block to be produced.
    BlockProductionNotRequired(u64),
    /// The duties for the present epoch were not found.
    ProducerDutiesUnknown(u64),
    /// The slot has already been processed, execution was skipped.
    SlotAlreadyProcessed(u64),
    /// The Beacon Node was unable to produce a block at that slot.
    BeaconNodeUnableToProduceBlock(u64),
    /// The signer failed to sign the message.
    SignerRejection(u64),
    /// The public key for this validator is not an active validator.
    ValidatorIsUnknown(u64),
}

#[derive(Debug, PartialEq)]
pub enum Error {
    SlotClockError,
    SlotUnknowable,
    EpochMapPoisoned,
    SlotClockPoisoned,
    EpochLengthIsZero,
    BeaconNodeError(BeaconNodeError),
}

/// A polling state machine which performs block production duties, based upon some epoch duties
/// (`EpochDutiesMap`) and a concept of time (`SlotClock`).
///
/// Ensures that messages are not slashable.
///
/// Relies upon an external service to keep the `EpochDutiesMap` updated.
pub struct BlockProducer<T: SlotClock, U: BeaconNode, V: DutiesReader, W: Signer> {
    pub last_processed_slot: Option<u64>,
    pubkey: PublicKey,
    spec: Arc<ChainSpec>,
    epoch_map: Arc<V>,
    slot_clock: Arc<T>,
    beacon_node: Arc<U>,
    signer: Arc<W>,
}

impl<T: SlotClock, U: BeaconNode, V: DutiesReader, W: Signer> BlockProducer<T, U, V, W> {
    /// Returns a new instance where `last_processed_slot == 0`.
    pub fn new(
        spec: Arc<ChainSpec>,
        pubkey: PublicKey,
        epoch_map: Arc<V>,
        slot_clock: Arc<T>,
        beacon_node: Arc<U>,
        signer: Arc<W>,
    ) -> Self {
        Self {
            last_processed_slot: None,
            pubkey,
            spec,
            epoch_map,
            slot_clock,
            beacon_node,
            signer,
        }
    }
}

impl<T: SlotClock, U: BeaconNode, V: DutiesReader, W: Signer> BlockProducer<T, U, V, W> {
    /// "Poll" to see if the validator is required to take any action.
    ///
    /// The slot clock will be read and any new actions undertaken.
    pub fn poll(&mut self) -> Result<PollOutcome, Error> {
        let slot = self
            .slot_clock
            .present_slot()
            .map_err(|_| Error::SlotClockError)?
            .ok_or(Error::SlotUnknowable)?;

        // If this is a new slot.
        if !self.is_processed_slot(slot) {
            let is_block_production_slot = match self.epoch_map.is_block_production_slot(slot) {
                Ok(result) => result,
                Err(DutiesReaderError::UnknownEpoch) => {
                    return Ok(PollOutcome::ProducerDutiesUnknown(slot));
                }
                Err(DutiesReaderError::UnknownValidator) => {
                    return Ok(PollOutcome::ValidatorIsUnknown(slot));
                }
                Err(DutiesReaderError::EpochLengthIsZero) => return Err(Error::EpochLengthIsZero),
                Err(DutiesReaderError::Poisoned) => return Err(Error::EpochMapPoisoned),
            };

            if is_block_production_slot {
                self.last_processed_slot = Some(slot);

                self.produce_block(slot)
            } else {
                Ok(PollOutcome::BlockProductionNotRequired(slot))
            }
        } else {
            Ok(PollOutcome::SlotAlreadyProcessed(slot))
        }
    }

    fn is_processed_slot(&self, slot: u64) -> bool {
        match self.last_processed_slot {
            Some(processed_slot) if processed_slot >= slot => true,
            _ => false,
        }
    }

    /// Produce a block at some slot.
    ///
    /// Assumes that a block is required at this slot (does not check the duties).
    ///
    /// Ensures the message is not slashable.
    ///
    /// !!! UNSAFE !!!
    ///
    /// The slash-protection code is not yet implemented. There is zero protection against
    /// slashing.
    fn produce_block(&mut self, slot: u64) -> Result<PollOutcome, Error> {
        let randao_reveal = {
            let producer_nonce = self.beacon_node.proposer_nonce(&self.pubkey)?;

            // TODO: add domain, etc to this message.
            let message = ssz_encode(&producer_nonce);

            match self.signer.bls_sign(&message) {
                None => return Ok(PollOutcome::SignerRejection(slot)),
                Some(signature) => signature,
            }
        };

        if let Some(block) = self
            .beacon_node
            .produce_beacon_block(slot, &randao_reveal)?
        {
            if self.safe_to_produce(&block) {
                if let Some(block) = self.sign_block(block) {
                    self.beacon_node.publish_beacon_block(block)?;
                    Ok(PollOutcome::BlockProduced(slot))
                } else {
                    Ok(PollOutcome::SignerRejection(slot))
                }
            } else {
                Ok(PollOutcome::SlashableBlockNotProduced(slot))
            }
        } else {
            Ok(PollOutcome::BeaconNodeUnableToProduceBlock(slot))
        }
    }

    /// Consumes a block, returning that block signed by the validators private key.
    ///
    /// Important: this function will not check to ensure the block is not slashable. This must be
    /// done upstream.
    fn sign_block(&mut self, mut block: BeaconBlock) -> Option<BeaconBlock> {
        self.store_produce(&block);

        match self.signer.bls_sign(&block.proposal_root(&self.spec)[..]) {
            None => None,
            Some(signature) => {
                block.signature = signature;
                Some(block)
            }
        }
    }

    /// Returns `true` if signing a block is safe (non-slashable).
    ///
    /// !!! UNSAFE !!!
    ///
    /// Important: this function is presently stubbed-out. It provides ZERO SAFETY.
    fn safe_to_produce(&self, _block: &BeaconBlock) -> bool {
        // TODO: ensure the producer doesn't produce slashable blocks.
        // https://github.com/sigp/lighthouse/issues/160
        true
    }

    /// Record that a block was produced so that slashable votes may not be made in the future.
    ///
    /// !!! UNSAFE !!!
    ///
    /// Important: this function is presently stubbed-out. It provides ZERO SAFETY.
    fn store_produce(&mut self, _block: &BeaconBlock) {
        // TODO: record this block production to prevent future slashings.
        // https://github.com/sigp/lighthouse/issues/160
    }
}

impl From<BeaconNodeError> for Error {
    fn from(e: BeaconNodeError) -> Error {
        Error::BeaconNodeError(e)
    }
}

#[cfg(test)]
mod tests {
    use super::test_utils::{TestBeaconNode, TestEpochMap, TestSigner};
    use super::*;
    use slot_clock::TestingSlotClock;
    use types::{
        test_utils::{SeedableRng, TestRandom, XorShiftRng},
        Keypair,
    };

    // TODO: implement more thorough testing.
    // https://github.com/sigp/lighthouse/issues/160
    //
    // These tests should serve as a good example for future tests.

    #[test]
    pub fn polling() {
        let mut rng = XorShiftRng::from_seed([42; 16]);

        let spec = Arc::new(ChainSpec::foundation());
        let slot_clock = Arc::new(TestingSlotClock::new(0));
        let beacon_node = Arc::new(TestBeaconNode::default());
        let signer = Arc::new(TestSigner::new(Keypair::random()));

        let mut epoch_map = TestEpochMap::new(spec.epoch_length);
        let produce_slot = 100;
        let produce_epoch = produce_slot / spec.epoch_length;
        epoch_map.map.insert(produce_epoch, produce_slot);
        let epoch_map = Arc::new(epoch_map);
        let keypair = Keypair::random();

        let mut block_producer = BlockProducer::new(
            spec.clone(),
            keypair.pk.clone(),
            epoch_map.clone(),
            slot_clock.clone(),
            beacon_node.clone(),
            signer.clone(),
        );

        // Configure responses from the BeaconNode.
        beacon_node.set_next_produce_result(Ok(Some(BeaconBlock::random_for_test(&mut rng))));
        beacon_node.set_next_publish_result(Ok(PublishOutcome::ValidBlock));
        beacon_node.set_next_nonce_result(Ok(0));

        // One slot before production slot...
        slot_clock.set_slot(produce_slot - 1);
        assert_eq!(
            block_producer.poll(),
            Ok(PollOutcome::BlockProductionNotRequired(produce_slot - 1))
        );

        // On the produce slot...
        slot_clock.set_slot(produce_slot);
        assert_eq!(
            block_producer.poll(),
            Ok(PollOutcome::BlockProduced(produce_slot))
        );

        // Trying the same produce slot again...
        slot_clock.set_slot(produce_slot);
        assert_eq!(
            block_producer.poll(),
            Ok(PollOutcome::SlotAlreadyProcessed(produce_slot))
        );

        // One slot after the produce slot...
        slot_clock.set_slot(produce_slot + 1);
        assert_eq!(
            block_producer.poll(),
            Ok(PollOutcome::BlockProductionNotRequired(produce_slot + 1))
        );

        // In an epoch without known duties...
        let slot = (produce_epoch + 1) * spec.epoch_length;
        slot_clock.set_slot(slot);
        assert_eq!(
            block_producer.poll(),
            Ok(PollOutcome::ProducerDutiesUnknown(slot))
        );
    }
}