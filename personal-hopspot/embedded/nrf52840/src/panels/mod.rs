//! Display controllers, kept separate from the boards that happen to use them.
//!
//! Public so that the configuration variants a single board never constructs do not trip the
//! dead-code lint under `-D warnings`.

pub mod st7789;
