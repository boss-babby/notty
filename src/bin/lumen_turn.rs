//! `lumen-turn` — UDP relay for Lumen collaboration through symmetric NATs. See `src/collab/turn.rs`.
fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    std::process::exit(lumen::turn_cli(std::env::args().collect()));
}
