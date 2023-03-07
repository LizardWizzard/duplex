use std::io;
use std::io::ErrorKind;
use std::pin::Pin;
use std::task::ready;
use std::task::Context;
use std::task::Poll;

use futures::sink::Buffer;
use futures::Future;
use futures::Stream;
use futures::{Sink, SinkExt};
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use pin_project::pin_project;
use tokio::pin;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tokio_util::codec::Decoder;
use tokio_util::codec::Encoder;
use tokio_util::codec::Framed;
use tokio_util::sync::PollSender;

macro_rules! inspect {
    ($cmd:expr, $name:literal) => {{
        let span = tracing::info_span!($name);
        let _guard = span.enter();

        tracing::info!("POLL");

        let res = $cmd;
        if res.is_pending() {
            tracing::info!("YIELD")
        } else {
            tracing::info!("RETURN")
        };

        res
    }};
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    // NOTE: there is no separate variant for protocol error
    //     for simplicity we merge it with io::Error
    //     this can be easily adjusted
    #[error("Io")]
    Io(#[from] std::io::Error),

    #[error("Processor died")]
    ProcessorDied,
}

#[pin_project]
pub struct Shuttle<S, M, C> {
    #[pin]
    framed: Framed<S, C>,

    #[pin]
    sender: Buffer<PollSender<M>, M>,

    #[pin]
    receiver: Receiver<M>,
}

impl<S, M, C> Shuttle<S, M, C>
where
    M: Send + 'static,
    S: AsyncRead + AsyncWrite,
    C: Encoder<M, Error = io::Error> + Decoder<Item = M, Error = io::Error>,
{
    pub fn new(stream: S, codec: C, sender: Sender<M>, receiver: Receiver<M>) -> Self {
        let sender = PollSender::new(sender).buffer(1);
        let framed = codec.framed(stream);
        Self {
            framed,
            sender,
            receiver,
        }
    }
}

impl<S, M, C> Shuttle<S, M, C>
where
    M: Send + 'static,
    S: AsyncRead + AsyncWrite,
    C: Encoder<M, Error = io::Error> + Decoder<Item = M, Error = io::Error>,
{
    fn poll_send(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<<Self as Future>::Output> {
        let mut this = self.project();

        // Flush a buffered message before fetching a new one.
        ready!(this
            .sender
            .as_mut()
            .poll_flush(cx)
            .map_err(|_| Error::ProcessorDied))?;

        let frame = match ready!(inspect!(this.framed.poll_next(cx), "framed.poll_next")) {
            Some(frame) => frame?,
            None => return Poll::Ready(Err(Error::Io(io::Error::from(ErrorKind::UnexpectedEof)))),
        };

        // We don't need to call sender's `poll_ready` because it's been flushed.
        this.sender
            .start_send(frame)
            .map_err(|_| Error::ProcessorDied)?;

        Poll::Ready(Ok(()))
    }

    fn poll_recv(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<<Self as Future>::Output> {
        let mut this = self.project();

        ready!(inspect!(
            this.framed.as_mut().poll_ready(cx),
            "framed.poll_ready"
        ))?;

        // NOTE: there can be a better strategy. We can move outer loop into this function,
        //     so we take multiple messages and flush them with single flush call
        ready!(inspect!(
            this.framed.as_mut().poll_flush(cx),
            "framed.poll_flush"
        ))?;

        let frame = match ready!(inspect!(
            this.receiver.as_mut().poll_recv(cx),
            "receiver.poll_recv"
        )) {
            Some(frame) => frame,
            None => return Poll::Ready(Err(Error::ProcessorDied)),
        };

        this.framed.as_mut().start_send(frame)?;

        Poll::Ready(Ok(()))
    }
}

impl<S, M, C> Future for Shuttle<S, M, C>
where
    M: Send + 'static,
    S: AsyncRead + AsyncWrite,
    C: Encoder<M, Error = io::Error> + Decoder<Item = M, Error = io::Error>,
{
    type Output = Result<(), Error>;

    #[tracing::instrument(skip_all)]
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        tracing::info!("POLL");
        let mut send_pending = false;
        let mut recv_pending = false;
        loop {
            tracing::info!(send_pending, recv_pending, "iteration");
            if send_pending && recv_pending {
                tracing::info!("YIELD");
                return Poll::Pending;
            }

            if !send_pending {
                match inspect!(self.as_mut().poll_send(cx), "poll_send") {
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Ready(Ok(())) => { /* send succeeded */ }
                    Poll::Pending => send_pending = true,
                }
            }

            if !recv_pending {
                match inspect!(self.as_mut().poll_recv(cx), "poll_recv") {
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Ready(Ok(())) => { /* receive succeeded */ }
                    Poll::Pending => recv_pending = true,
                }
            }
        }
    }
}
