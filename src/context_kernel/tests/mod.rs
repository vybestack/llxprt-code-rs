//! Red tests for the context kernel, split by area: each submodule owns its tests
//! and the fixtures they need; the shared fixtures live in `fixtures`.

mod ir;
mod lanes_scopes;
mod legality;
mod migration;
mod reducer;
