
#[macro_use] extern crate batsmt_pretty;
extern crate fxhash;
#[macro_use] extern crate log;

pub mod types;
pub mod parser;
pub mod simple_ast;

pub use types::{Statement,TermBuilder,SortBuilder};
pub use parser::{parse,parse_stdin,parse_str,Error,Result};

