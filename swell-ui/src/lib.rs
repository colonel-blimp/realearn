#![feature(trait_alias)]
mod view_manager;
use view_manager::*;

mod window;
pub use window::*;

mod menu;
pub use menu::*;

mod view;
pub use view::*;

mod units;
pub use units::*;

mod types;
pub use types::*;

mod string_types;
pub use string_types::*;

pub mod menu_tree;
