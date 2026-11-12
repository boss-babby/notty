//! `lumen-mcp` — headless MCP server for a Lumen workspace file (see `mcp.rs`).
fn main() {
    std::process::exit(lumen::mcp_cli(std::env::args().collect()));
}
