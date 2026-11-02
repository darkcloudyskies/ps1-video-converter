//! What the app does: open discs, show frames and run a queue of exports. This has
//! no UI toolkit in it, so `psxvc-gui` stays a thin layer and all of this can be
//! tested anywhere.

pub mod disc;
pub mod queue;
