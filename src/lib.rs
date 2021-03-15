mod components;
mod dom;
mod fctx;
mod internal;

pub use fctx::Fctx;

pub use internal::{panel, text, Context, Element, ComponentFunc};

#[cfg(test)]
mod tests;
