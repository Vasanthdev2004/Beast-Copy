

// Hot-path state is distributed across the specialized engines (WalletTracker, PositionTracker)
// using DashMap. This module can serve as the unified wrapper if needed in the future.
pub struct MemoryStore {}

impl MemoryStore {
    pub fn new() -> Self {
        Self {}
    }
}
