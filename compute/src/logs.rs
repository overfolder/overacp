use std::pin::Pin;

use bytes::Bytes;
use futures::Stream;

use crate::error::ProviderError;

pub type LogStream = Pin<Box<dyn Stream<Item = Result<Bytes, ProviderError>> + Send>>;
