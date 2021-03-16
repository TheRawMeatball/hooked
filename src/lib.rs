mod components;
mod dom;
mod fctx;
mod internal;

pub mod prelude {
    use super::*;
    pub use fctx::Fctx;
    pub use internal::{ComponentFunc, Context, Element};
    pub mod e {
        pub use super::internal::{panel, text};
    }
}

#[cfg(test)]
mod tests;
