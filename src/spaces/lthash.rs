use blake3::Hasher as Blake3Hasher;
use sha2::{Digest, Sha256};

const NUM_LANES: usize = 1024;
const STATE_BYTES: usize = NUM_LANES * 2; // 2048

pub struct LtHashState {
    lanes: [u16; NUM_LANES],
}

impl Default for LtHashState {
    fn default() -> Self {
        Self::new()
    }
}

impl LtHashState {
    pub fn new() -> Self {
        LtHashState {
            lanes: [0u16; NUM_LANES],
        }
    }

    pub fn add(&mut self, element: &[u8]) {
        let expanded = expand_element(element);
        for (i, val) in expanded.iter().enumerate().take(NUM_LANES) {
            self.lanes[i] = self.lanes[i].wrapping_add(*val);
        }
    }

    pub fn remove(&mut self, element: &[u8]) {
        let expanded = expand_element(element);
        for (i, val) in expanded.iter().enumerate().take(NUM_LANES) {
            self.lanes[i] = self.lanes[i].wrapping_sub(*val);
        }
    }

    pub fn hash(&self) -> [u8; 32] {
        Sha256::digest(self.as_bytes()).into()
    }

    pub fn as_bytes(&self) -> [u8; STATE_BYTES] {
        let mut bytes = [0u8; STATE_BYTES];
        for i in 0..NUM_LANES {
            let le = self.lanes[i].to_le_bytes();
            bytes[i * 2] = le[0];
            bytes[i * 2 + 1] = le[1];
        }
        bytes
    }

    pub fn from_bytes(bytes: [u8; STATE_BYTES]) -> Self {
        let mut lanes = [0u16; NUM_LANES];
        for i in 0..NUM_LANES {
            lanes[i] = u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]);
        }
        LtHashState { lanes }
    }
}

fn expand_element(element: &[u8]) -> [u16; NUM_LANES] {
    let mut hasher = Blake3Hasher::new();
    hasher.update(element);
    let mut xof = hasher.finalize_xof();
    let mut buf = [0u8; STATE_BYTES];
    xof.fill(&mut buf);

    let mut lanes = [0u16; NUM_LANES];
    for i in 0..NUM_LANES {
        lanes[i] = u16::from_le_bytes([buf[i * 2], buf[i * 2 + 1]]);
    }
    lanes
}

pub fn record_element(collection: &str, rkey: &str, cid: &str) -> Vec<u8> {
    format!("{collection}/{rkey}/{cid}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_is_all_zeroes() {
        let state = LtHashState::new();
        assert_eq!(state.as_bytes(), [0u8; 2048]);
        let expected: [u8; 32] = sha2::Sha256::digest([0u8; 2048]).into();
        assert_eq!(state.hash(), expected);
    }

    #[test]
    fn add_then_remove_returns_to_empty() {
        let mut state = LtHashState::new();
        let element = record_element("com.example.post", "3k2abc", "bafyreiabc123");
        state.add(&element);
        assert_ne!(state.as_bytes(), [0u8; 2048]);
        state.remove(&element);
        assert_eq!(state.as_bytes(), [0u8; 2048]);
    }

    #[test]
    fn order_independent() {
        let elem_a = record_element("com.example.post", "aaa", "bafyreiaaa");
        let elem_b = record_element("com.example.post", "bbb", "bafyreibbb");

        let mut state1 = LtHashState::new();
        state1.add(&elem_a);
        state1.add(&elem_b);

        let mut state2 = LtHashState::new();
        state2.add(&elem_b);
        state2.add(&elem_a);

        assert_eq!(state1.hash(), state2.hash());
        assert_eq!(state1.as_bytes(), state2.as_bytes());
    }

    #[test]
    fn different_records_different_hashes() {
        let elem_a = record_element("com.example.post", "aaa", "bafyreiaaa");
        let elem_b = record_element("com.example.post", "bbb", "bafyreibbb");

        let mut state_a = LtHashState::new();
        state_a.add(&elem_a);

        let mut state_b = LtHashState::new();
        state_b.add(&elem_b);

        assert_ne!(state_a.hash(), state_b.hash());
    }

    #[test]
    fn record_element_format() {
        let elem = record_element("com.example.post", "3k2abc", "bafyreiabc");
        assert_eq!(elem, b"com.example.post/3k2abc/bafyreiabc");
    }

    #[test]
    fn from_bytes_roundtrip() {
        let mut state = LtHashState::new();
        let elem = record_element("com.example.post", "3k2abc", "bafyreiabc");
        state.add(&elem);
        let bytes = state.as_bytes();
        let restored = LtHashState::from_bytes(bytes);
        assert_eq!(state.hash(), restored.hash());
    }

    #[test]
    fn wrapping_arithmetic() {
        let mut state = LtHashState::new();
        let elem = record_element("test", "key", "cid");
        // Adding the same element 65536 times should wrap back to zero
        for _ in 0..65536 {
            state.add(&elem);
        }
        assert_eq!(state.as_bytes(), [0u8; 2048]);
    }

    #[test]
    fn remove_standalone() {
        let mut state = LtHashState::new();
        let elem = record_element("app.bsky.feed.post", "abc123", "bafydata");
        state.add(&elem);
        assert_ne!(state.as_bytes(), LtHashState::new().as_bytes());
        state.remove(&elem);
        assert_eq!(state.as_bytes(), LtHashState::new().as_bytes());
        assert_eq!(state.hash(), LtHashState::new().hash());
    }

    #[test]
    fn from_bytes_roundtrip_modified() {
        let mut state = LtHashState::new();
        state.add(&record_element("com.example.post", "rk1", "bafyabc"));
        let original_hash = state.hash();
        let mut bytes = state.as_bytes();
        bytes[1024] ^= 0xFF;
        let tampered = LtHashState::from_bytes(bytes);
        assert_ne!(tampered.hash(), original_hash);
    }
}
