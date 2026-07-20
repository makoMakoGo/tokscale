pub use crate::client_catalog::{ClientId, ClientIdentity, CLIENT_IDENTITIES};
pub use crate::local_clients::{
    cline_session_data_dir_with_env_strategy, warp_sqlite_roots_with_env_strategy, LocalClientDef,
    PathRoot, LOCAL_CLIENTS,
};

#[derive(Debug)]
pub struct ClientCounts {
    counts: [i32; ClientId::COUNT],
}

impl ClientCounts {
    pub fn new() -> Self {
        Self {
            counts: [0; ClientId::COUNT],
        }
    }

    pub fn get(&self, client: ClientId) -> i32 {
        self.counts[client as usize]
    }

    pub fn set(&mut self, client: ClientId, value: i32) {
        self.counts[client as usize] = value;
    }

    pub fn add(&mut self, client: ClientId, value: i32) {
        self.counts[client as usize] += value;
    }
}

impl Default for ClientCounts {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn client_id_all_len_matches_count() {
        assert_eq!(ClientId::ALL.len(), ClientId::COUNT);
    }

    #[test]
    fn client_id_string_round_trip() {
        for client in ClientId::iter() {
            let id = client.as_str();
            assert_eq!(ClientId::from_str(id), Some(client));
        }
    }

    #[test]
    fn catalog_ids_are_unique() {
        let ids: HashSet<&str> = ClientId::iter().map(ClientId::as_str).collect();
        assert_eq!(ids.len(), ClientId::COUNT);
    }

    #[test]
    fn catalog_hotkeys_are_unique() {
        let hotkeys: Vec<char> = ClientId::iter().filter_map(ClientId::hotkey).collect();
        let unique: HashSet<char> = hotkeys.iter().copied().collect();
        assert_eq!(unique.len(), hotkeys.len());
    }

    #[test]
    fn synthetic_is_not_a_client_identity() {
        assert_eq!(ClientId::from_str("synthetic"), None);
        assert_eq!(ClientId::from_str("synthetic.new"), None);
        assert_eq!(ClientId::from_str("antigravity-cli"), None);
    }

    #[test]
    fn removed_clients_are_not_client_identities() {
        assert_eq!(ClientId::from_str("cursor"), None);
        assert_eq!(ClientId::from_str("trae"), None);
    }

    #[test]
    fn pi_and_omp_have_separate_identity_facts() {
        assert_eq!(ClientId::Pi.as_str(), "pi");
        assert_eq!(ClientId::Omp.as_str(), "omp");
        assert_eq!(ClientId::Pi.display_name(), "Pi");
        assert_eq!(ClientId::Omp.display_name(), "OMP");
    }

    #[test]
    fn grok_uses_the_short_brand_name_for_display() {
        assert_eq!(ClientId::Grok.display_name(), "Grok");
        assert_eq!(ClientId::Grok.short_name(), "Grok");
    }

    #[test]
    fn hermes_uses_the_short_brand_name_for_display() {
        assert_eq!(ClientId::Hermes.display_name(), "Hermes");
        assert_eq!(ClientId::Hermes.short_name(), "Hermes");
    }

    #[test]
    fn client_counts_get_set_add_work() {
        let mut counts = ClientCounts::new();

        assert_eq!(counts.get(ClientId::Claude), 0);
        counts.set(ClientId::Claude, 3);
        assert_eq!(counts.get(ClientId::Claude), 3);
        counts.add(ClientId::Claude, 2);
        assert_eq!(counts.get(ClientId::Claude), 5);
    }
}
