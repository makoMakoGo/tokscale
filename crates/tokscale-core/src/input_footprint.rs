use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use crate::ClientId;

/// Confirmed local input bytes, partitioned by the client that owns each
/// input. This is the sole authority for both per-client space and the
/// Overview total; the total is always derived.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct InputFootprint(BTreeMap<ClientId, u64>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputFootprintOverflow;

impl std::fmt::Display for InputFootprintOverflow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("input data size exceeds u64::MAX")
    }
}

impl std::error::Error for InputFootprintOverflow {}

impl InputFootprint {
    pub fn for_clients(clients: impl IntoIterator<Item = ClientId>) -> Self {
        Self(clients.into_iter().map(|client| (client, 0)).collect())
    }

    pub fn from_client_bytes(
        entries: impl IntoIterator<Item = (ClientId, u64)>,
    ) -> Result<Self, InputFootprintOverflow> {
        let mut footprint = Self::default();
        for (client, bytes) in entries {
            footprint.set_bytes(client, bytes)?;
        }
        Ok(footprint)
    }

    pub fn set_bytes(
        &mut self,
        client: ClientId,
        bytes: u64,
    ) -> Result<(), InputFootprintOverflow> {
        let previous = self.bytes_for(client);
        let remaining = self
            .total_bytes()?
            .checked_sub(previous)
            .expect("client bytes must be included in the footprint total");
        remaining.checked_add(bytes).ok_or(InputFootprintOverflow)?;
        self.0.insert(client, bytes);
        Ok(())
    }

    pub fn add_bytes(
        &mut self,
        client: ClientId,
        bytes: u64,
    ) -> Result<(), InputFootprintOverflow> {
        let total = self
            .bytes_for(client)
            .checked_add(bytes)
            .ok_or(InputFootprintOverflow)?;
        self.set_bytes(client, total)
    }

    pub fn bytes_for(&self, client: ClientId) -> u64 {
        self.0.get(&client).copied().unwrap_or(0)
    }

    pub fn contains_client(&self, client: ClientId) -> bool {
        self.0.contains_key(&client)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (ClientId, u64)> + '_ {
        self.0.iter().map(|(&client, &bytes)| (client, bytes))
    }

    pub fn total_bytes(&self) -> Result<u64, InputFootprintOverflow> {
        self.0.values().copied().try_fold(0_u64, |total, bytes| {
            total.checked_add(bytes).ok_or(InputFootprintOverflow)
        })
    }
}

impl<'de> Deserialize<'de> for InputFootprint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let footprint = Self(BTreeMap::deserialize(deserializer)?);
        footprint.total_bytes().map_err(serde::de::Error::custom)?;
        Ok(footprint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_is_derived_from_typed_client_entries() {
        let footprint =
            InputFootprint::from_client_bytes([(ClientId::Amp, 13), (ClientId::CodeBuddy, 8)])
                .unwrap();

        assert_eq!(footprint.bytes_for(ClientId::Amp), 13);
        assert_eq!(footprint.bytes_for(ClientId::Claude), 0);
        assert_eq!(footprint.total_bytes().unwrap(), 21);
    }

    #[test]
    fn serde_uses_canonical_client_ids_as_map_keys() {
        let footprint =
            InputFootprint::from_client_bytes([(ClientId::Claude, 4_096), (ClientId::Codex, 7)])
                .unwrap();

        assert_eq!(
            footprint
                .iter()
                .map(|(client, _)| client.as_str())
                .collect::<Vec<_>>(),
            ["claude", "codex"]
        );
        let value = serde_json::to_value(&footprint).unwrap();
        assert_eq!(value, serde_json::json!({"claude": 4096, "codex": 7}));
        assert_eq!(
            serde_json::from_value::<InputFootprint>(value).unwrap(),
            footprint
        );
    }

    #[test]
    fn serde_rejects_unknown_client_keys() {
        let error = serde_json::from_value::<InputFootprint>(serde_json::json!({
            "not-a-client": 1
        }))
        .unwrap_err();

        assert!(error.to_string().contains("unknown local client"));
    }

    #[test]
    fn serde_rejects_an_overflowing_total() {
        let error = serde_json::from_value::<InputFootprint>(serde_json::json!({
            "amp": u64::MAX,
            "codex": 1
        }))
        .unwrap_err();

        assert!(error.to_string().contains("exceeds u64::MAX"));
    }

    #[test]
    fn failed_add_does_not_create_an_invalid_footprint() {
        let mut footprint = InputFootprint::from_client_bytes([(ClientId::Amp, u64::MAX)]).unwrap();

        assert_eq!(
            footprint.add_bytes(ClientId::Codex, 1),
            Err(InputFootprintOverflow)
        );
        assert_eq!(footprint.bytes_for(ClientId::Codex), 0);
        assert_eq!(footprint.total_bytes().unwrap(), u64::MAX);
    }
}
