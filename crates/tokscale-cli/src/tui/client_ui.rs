use tokscale_core::ClientId;

pub fn display_name(client: ClientId) -> &'static str {
    client.short_name()
}

pub fn hotkey(client: ClientId) -> Option<char> {
    client.hotkey()
}

pub fn from_hotkey(key: char) -> Option<ClientId> {
    ClientId::iter().find(|client| client.hotkey() == Some(key))
}
