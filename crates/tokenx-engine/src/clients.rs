pub use crate::client_catalog::{ClientId, ClientIdentity, CLIENT_IDENTITIES};

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
    fn pi_and_omp_have_separate_identity_facts() {
        assert_eq!(ClientId::Pi.as_str(), "pi");
        assert_eq!(ClientId::Omp.as_str(), "omp");
        assert_eq!(ClientId::Pi.display_name(), "Pi");
        assert_eq!(ClientId::Omp.display_name(), "OMP");
    }

    #[test]
    fn codex_uses_the_product_name_for_display() {
        assert_eq!(ClientId::Codex.display_name(), "Codex");
    }

    #[test]
    fn grok_uses_the_brand_name_for_display() {
        assert_eq!(ClientId::Grok.display_name(), "Grok");
    }

    #[test]
    fn hermes_uses_the_brand_name_for_display() {
        assert_eq!(ClientId::Hermes.display_name(), "Hermes");
    }
}
