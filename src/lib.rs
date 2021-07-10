mod eventual;
pub use eventual::{Eventual, EventualReader, EventualWriter};
pub mod error;
pub use error::Closed;
mod combinators;
pub use combinators::*;

// This is a convenience trait to make it easy to pass either an Eventual or an
// EventualReader into functions.
// TODO: Implement
pub trait IntoReader {
    type Output;
    fn into_reader(self) -> EventualReader<Self::Output>;
}

pub trait Value: 'static + Send + Clone + Eq {}
impl<T> Value for T where T: 'static + Send + Clone + Eq {}
