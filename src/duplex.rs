use std::io;
use std::io::ErrorKind;
use std::pin::Pin;
use std::task::ready;
use std::task::Context;
use std::task::Poll;

use futures::Future;
use futures::Sink;
use futures::Stream;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use pin_project::pin_project;
use tokio::pin;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tokio_util::codec::Decoder;
use tokio_util::codec::Encoder;
use tokio_util::codec::Framed;

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
    #[error("protocol {0}")]
    Protocol(&'static str), // FIXME
    //
    #[error("io")]
    Io(#[from] std::io::Error),

    #[error("processor died")]
    ProcessorDied,
}

#[pin_project]
pub struct Shuttle<S, M, C> {
    #[pin]
    framed: Framed<S, C>,

    #[pin]
    sender: Sender<M>,

    #[pin]
    receiver: Receiver<M>,
}

impl<S, M, C> Shuttle<S, M, C>
where
    S: AsyncRead + AsyncWrite,
    C: Encoder<M, Error = io::Error> + Decoder<Item = M, Error = io::Error>,
{
    pub fn new(stream: S, codec: C, sender: Sender<M>, receiver: Receiver<M>) -> Self {
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
    S: AsyncRead + AsyncWrite,
    C: Encoder<M> + Decoder<Item = M>,
{
    fn poll_send(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<<Self as Future>::Output> {
        // can we write into channel?
        // acquire permit to do that
        let this = self.project();

        // TODO pukable stream analogous to https://docs.rs/futures/latest/futures/sink/struct.Buffer.html
        //     but with access to internal Buffer
        //     or emulate with poll_flush on Buffer, ready means empty buf?

        // TODO reserve_owned, or slot
        let fut = this.sender.reserve();
        pin!(fut);

        // TODO if let else
        let permit = match ready!(inspect!(fut.poll(cx), "reserve")) {
            Ok(p) => p,
            Err(_) => return Poll::Ready(Err(Error::ProcessorDied)),
        };

        // permit acquired, write to underlying framed
        let framed = this.framed;

        let frame = match ready!(inspect!(framed.poll_next(cx), "framed.poll_next")) {
            Some(frame) => frame.map_err(|_| Error::Protocol("whoopsie"))?,
            None => return Poll::Ready(Err(Error::Io(io::Error::from(ErrorKind::UnexpectedEof)))),
        };

        permit.send(frame);

        Poll::Ready(Ok(()))
    }

    fn poll_recv(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<<Self as Future>::Output> {
        let mut this = self.project();

        // TODO remove map_err?
        ready!(inspect!(
            this.framed.as_mut().poll_ready(cx),
            "framed.poll_ready"
        ))
        .map_err(|_| Error::Protocol("uh oh"))?;

        let frame = match ready!(inspect!(
            this.receiver.as_mut().poll_recv(cx),
            "receiver.poll_recv"
        )) {
            Some(frame) => frame,
            None => return Poll::Ready(Err(Error::ProcessorDied)),
        };

        this.framed
            .as_mut()
            .start_send(frame)
            .map_err(|_| Error::Protocol("uh oh"))?;

        ready!(inspect!(
            this.framed.as_mut().poll_flush(cx),
            "framed.poll_flush"
        ))
        .map_err(|_| Error::Protocol("uh oh"))?;

        Poll::Ready(Ok(()))
    }
}

impl<S, M, C> Future for Shuttle<S, M, C>
where
    S: AsyncRead + AsyncWrite,
    C: Encoder<M> + Decoder<Item = M>,
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
