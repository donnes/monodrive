//! `monodrive mcp`: a Model Context Protocol server on stdin and stdout.
//! `server` speaks the protocol, `contract` holds the shapes every reply
//! shares, and each module in `tools` decodes a tool's arguments, calls one
//! service and replies. What agents may rely on is in `docs/mcp.md`, and
//! `contract/v1.json` holds it so a test can tell a break from an addition.

mod contract;
mod server;
mod tools;

pub use contract::Profile;
pub use server::{Server, serve_stdio};
pub use tools::{TOOLS, Tool};

#[cfg(test)]
mod tests;
