//! The text model behind gitem's editor: a rope buffer that gives back the bytes it was given,
//! selections, edits with their inverses, and undo. No UI and no git, so it can be tested
//! fast and hard.
//!
//! Every position is a char index into the rope. Lines and columns are worked out from it
//! when needed.
pub mod brackets;
pub mod buffer;
pub mod edit;
pub mod encoding;
pub mod history;
pub mod indent;
pub mod lines;
pub mod search;
pub mod selection;

pub use buffer::{Buffer, LineEnding};
pub use edit::{Edit, Transaction};
pub use encoding::{Encoding, Unsupported};
pub use history::History;
pub use selection::{Selection, SelectionSet};
