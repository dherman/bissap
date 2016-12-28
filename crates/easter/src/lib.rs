extern crate joker;
extern crate tristate;

pub mod id;
pub mod fun;
pub mod obj;
pub mod stmt {
    include!(concat!(env!("OUT_DIR"), "/stmt.rs"));
}
pub mod expr;
pub mod decl;
pub mod patt;
pub mod punc;
pub mod cover;
