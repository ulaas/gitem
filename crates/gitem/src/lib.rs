//! gitem's UI, as a library so tests can drive it headlessly.
#[cfg(test)]
gitem_testenv::isolate!();

pub mod action;
pub mod app;
pub mod chrome;
pub mod crash;
pub mod logo;
pub mod notify;
pub mod data;
pub mod forms;
pub mod i18n;
pub mod keys;
pub mod menus;
pub mod palette;
pub mod standalone;
pub mod svg;
pub mod syntax;
pub mod task;
pub mod theme;
pub mod views;
pub mod widgets;
