use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct DecoderContractFingerprint([u8; 32]);

include!(concat!(env!("OUT_DIR"), "/decoder_contracts.rs"));

impl DecoderContractFingerprint {
    pub(crate) const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub(crate) enum DecoderVariant {
    #[default]
    Primary,
    CodeBuddyJsonl,
    CodeBuddyExtension,
    CodeBuddyHost,
    CopilotBuiltIn,
    CopilotExplicitRoot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct DecoderVersion {
    pub decoder_id: DecoderId,
    pub(super) contract: DecoderContractFingerprint,
    pub(super) variant: DecoderVariant,
}

impl DecoderVersion {
    pub(crate) const fn current(decoder_id: DecoderId) -> Self {
        Self {
            decoder_id,
            contract: decoder_id.contract_fingerprint(),
            variant: DecoderVariant::Primary,
        }
    }

    pub(crate) const fn with_variant(mut self, variant: DecoderVariant) -> Self {
        self.variant = variant;
        self
    }

    pub(crate) const fn contract(self) -> DecoderContractFingerprint {
        self.contract
    }

    pub(crate) const fn variant(self) -> DecoderVariant {
        self.variant
    }

    #[cfg(test)]
    pub(crate) const fn for_test_contract_marker(decoder_id: DecoderId, marker: u32) -> Self {
        let mut bytes = decoder_id.contract_fingerprint().0;
        let marker = marker.to_le_bytes();
        bytes[28] = marker[0];
        bytes[29] = marker[1];
        bytes[30] = marker[2];
        bytes[31] = marker[3];
        Self {
            decoder_id,
            contract: DecoderContractFingerprint(bytes),
            variant: DecoderVariant::Primary,
        }
    }
}
