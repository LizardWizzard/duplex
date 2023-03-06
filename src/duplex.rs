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

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("protocol {0}")]
    Protocol(&'static str), // FIXME
    #[error("io")]
    Io(#[from] std::io::Error),
    #[error("processor died")]
    ProcessorDied,
}

#[pin_project]
pub struct Shuttle<S, M, C>
where
    S: AsyncRead + AsyncWrite,
    C: Encoder<M> + Decoder<Item = M>,
{
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
    C: Encoder<M> + Decoder<Item = M>,
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

        let fut = this.sender.reserve();
        pin!(fut);

        let permit = match ready!(fut.poll(cx)) {
            Ok(p) => p,
            Err(_) => return Poll::Ready(Err(Error::ProcessorDied)),
        };

        // permit acquired, write to underlying framed
        let framed = this.framed;

        let frame = match ready!(framed.poll_next(cx)) {
            Some(frame) => frame.map_err(|_| Error::Protocol("whoopsie"))?,
            None => return Poll::Ready(Err(Error::Io(io::Error::from(ErrorKind::UnexpectedEof)))),
        };

        permit.send(frame);

        Poll::Ready(Ok(()))
    }

    fn poll_receive(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<<Self as Future>::Output> {
        let mut this = self.project();

        ready!(this.framed.poll_ready(cx));

        let frame = match ready!(this.receiver.poll_recv(cx)) {
            Some(frame) => frame,
            None => return Poll::Ready(Err(Error::ProcessorDied)),
        };

        this.framed
            .start_send(frame)
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

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut send_pending = false;
        let mut receive_pending = false;
        loop {
            if send_pending && receive_pending {
                return Poll::Pending;
            }
            

            match self.poll_send(cx) {
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) => { /* send succeeded */ }
                Poll::Pending => send_pending = true,
            }

            match self.poll_receive(cx) {
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) => { /* receive succeeded */ }
                Poll::Pending => receive_pending = true,
            }
        }
    }
}
