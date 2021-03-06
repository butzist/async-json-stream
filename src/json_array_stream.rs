use futures::{stream::Stream, task::Context};
use std::pin::Pin;
use std::task::Poll;
use thiserror::Error;

use super::json_depth_analyzer;

#[derive(Error, Debug)]
pub enum JsonStreamError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid syntax")]
    Parser(#[from] json_depth_analyzer::ParserError),
    #[error("invalid json")]
    Json(#[from] serde_json::error::Error),
}

pub struct JsonArrayStream<S, B>
where
    S: Stream<Item = B>,
    B: IntoIterator<Item = u8> + Sized,
{
    analyzer: json_depth_analyzer::JsonDepthAnalyzer,
    buffer: Vec<u8>,
    stream: Pin<Box<S>>,
    chunk: Option<B::IntoIter>,
    comma: bool,
    end: bool,
}

impl<S, B> Stream for JsonArrayStream<S, B>
where
    S: Stream<Item = B>,
    B: IntoIterator<Item = u8> + Sized,
{
    type Item = Result<Vec<u8>, JsonStreamError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let this = unsafe { self.get_unchecked_mut() };
        if this.end {
            return Poll::Ready(None);
        }

        loop {
            if let Some(chunk) = this.chunk.as_mut() {
                for c in chunk {
                    let initial_depth = this.analyzer.depth();

                    this.analyzer
                        .process(c)
                        .map_err(|err| JsonStreamError::from(err))?;

                    if initial_depth == 0 {
                        continue;
                    }

                    let emit = if initial_depth == 1 && c == b',' {
                        this.comma = true;
                        true
                    } else if initial_depth == 1 && (c as char).is_whitespace() {
                        false
                    } else if this.analyzer.depth() == 0 {
                        this.end = true;
                        true
                    } else {
                        this.buffer.push(c);
                        false
                    };

                    if emit {
                        if this.buffer.len() == 0 && !this.comma {
                            return Poll::Ready(None);
                        }

                        let mut empty = vec![];
                        std::mem::swap(&mut empty, &mut this.buffer);
                        return Poll::Ready(Some(Ok(empty)));
                    }
                }
                this.chunk = None;
            }

            match this.stream.as_mut().poll_next(cx) {
                Poll::Ready(None) => {
                    return Poll::Ready(Some(Err(JsonStreamError::from(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "preliminary EOF when parsing json array",
                    )))));
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
                Poll::Ready(Some(chunk)) => {
                    this.chunk = Some(chunk.into_iter());
                }
            }
        }
    }
}

pub fn stream_json_array<S, B>(stream: S) -> JsonArrayStream<S, B>
where
    S: Stream<Item = B>,
    B: IntoIterator<Item = u8> + Sized,
{
    JsonArrayStream {
        stream: Box::pin(stream),
        analyzer: json_depth_analyzer::JsonDepthAnalyzer::new(),
        buffer: vec![],
        chunk: None,
        comma: false,
        end: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::prelude::*;
    use std::error::Error;

    #[tokio::test]
    async fn empty_array() {
        let json = "[]";
        let stream = futures::stream::once(async { json.bytes() });
        let parsed: Result<Vec<_>, _> = stream_json_array(stream)
            .map_err(|err| Box::new(err) as Box<dyn Error>)
            .and_then(move |buffer| {
                future::ready(String::from_utf8(buffer).map_err(|err| Box::new(err).into()))
            })
            .try_collect()
            .await;

        assert_eq!(parsed.unwrap(), vec![] as Vec<&str>);
    }

    #[tokio::test]
    async fn single_value() {
        let json = "[12]";
        let stream = futures::stream::once(async { json.bytes() });
        let parsed: Result<Vec<_>, _> = stream_json_array(stream)
            .map_err(|err| Box::new(err) as Box<dyn Error>)
            .and_then(|buffer| {
                future::ready(String::from_utf8(buffer).map_err(|err| Box::new(err).into()))
            })
            .try_collect()
            .await;

        assert_eq!(parsed.unwrap(), vec!["12"]);
    }

    #[tokio::test]
    async fn multiple_values() {
        let json = "[\"blubb\", 42,{\"xxx\":false , \"yyy\":\"abc\"} ] ";
        let stream = futures::stream::once(async { json.bytes() });
        let parsed: Result<Vec<_>, _> = stream_json_array(stream)
            .map_err(|err| Box::new(err) as Box<dyn Error>)
            .and_then(|buffer| {
                future::ready(String::from_utf8(buffer).map_err(|err| Box::new(err).into()))
            })
            .try_collect()
            .await;

        assert_eq!(
            parsed.unwrap(),
            vec!["\"blubb\"", "42", "{\"xxx\":false , \"yyy\":\"abc\"}"]
        );
    }

    #[tokio::test]
    async fn comma_without_values() {
        let json = "[,]";
        let stream = futures::stream::once(async { json.bytes() });
        let parsed: Result<Vec<_>, _> = stream_json_array(stream)
            .map_err(|err| Box::new(err) as Box<dyn Error>)
            .and_then(|buffer| {
                future::ready(String::from_utf8(buffer).map_err(|err| Box::new(err).into()))
            })
            .try_collect()
            .await;

        assert_eq!(parsed.unwrap(), vec!["", ""]);
    }

    #[tokio::test]
    async fn dangling_comma() {
        let json = "[42 , ]";
        let stream = futures::stream::once(async { json.bytes() });
        let parsed: Result<Vec<_>, _> = stream_json_array(stream)
            .map_err(|err| Box::new(err) as Box<dyn Error>)
            .and_then(|buffer| {
                future::ready(String::from_utf8(buffer).map_err(|err| Box::new(err).into()))
            })
            .try_collect()
            .await;

        assert_eq!(parsed.unwrap(), vec!["42", ""]);
    }
}
