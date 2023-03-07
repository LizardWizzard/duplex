use std::io;
use std::pin::Pin;
use std::task::ready;
use std::task::Context;
use std::task::Poll;

use futures::Future;
use futures::Sink;
use futures::{Stream, StreamExt};
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;

use pin_project::pin_project;
use tokio::pin;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tokio_stream::wrappers::ReceiverStream;
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

    /// A local user (server) is no longer interested in receiving messages.
    #[error("ServerHangup")]
    ServerHangup,

    /// The inbound stream has been shut down.
    #[error("ClientHangup")]
    ClientHangup,
}

pub enum HalfShutdownBehavior {
    ShutdownBoth,
    KeepOne,
}

#[pin_project]
pub struct Mux<S, M, C> {
    #[pin]
    framed: Framed<S, C>,

    #[pin]
    sender: PollSender<M>,

    receiver: ReceiverStream<M>,

    half_shutdown_behavior: HalfShutdownBehavior,

    send_alive: bool,
    recv_alive: bool,
}

impl<S, M, C> Mux<S, M, C>
where
    M: Send + 'static,
    S: AsyncRead + AsyncWrite,
    C: Encoder<M, Error = io::Error> + Decoder<Item = M, Error = io::Error>,
{
    pub fn new(
        stream: S,
        codec: C,
        sender: Sender<M>,
        receiver: Receiver<M>,
        half_shutdown_behavior: HalfShutdownBehavior,
    ) -> Self {
        let receiver = ReceiverStream::new(receiver);
        let sender = PollSender::new(sender);
        let framed = codec.framed(stream);
        Self {
            framed,
            sender,
            receiver,
            half_shutdown_behavior,
            send_alive: true,
            recv_alive: true,
        }
    }
}

impl<S, M, C> Future for Mux<S, M, C>
where
    M: Send + 'static,
    S: AsyncRead + AsyncWrite,
    C: Encoder<M, Error = io::Error> + Decoder<Item = M, Error = io::Error>,
{
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        inspect!(self.poll_main(cx), "poll")
    }
}

#[derive(thiserror::Error, Debug)]
enum CopyError<StreamError, SinkError> {
    #[error("stream hangup")]
    Stream(Option<StreamError>),

    #[error("sink hangup")]
    Sink(SinkError),
}

fn poll_copy<StreamError, SinkError, M>(
    stream: Pin<&mut impl Stream<Item = Result<M, StreamError>>>,
    mut sink: Pin<&mut impl Sink<M, Error = SinkError>>,
    cx: &mut Context<'_>,
) -> Poll<Result<(), CopyError<StreamError, SinkError>>> {
    // Flush a buffered message before fetching a new one.
    let res = inspect!(sink.as_mut().poll_flush(cx), "sink.poll_flush");
    ready!(res).map_err(CopyError::Sink)?;

    // Now it's time to prepare for the `start_send` below.
    let res = inspect!(sink.as_mut().poll_ready(cx), "sink.poll_ready");
    ready!(res).map_err(CopyError::Sink)?;

    let res = inspect!(stream.poll_next(cx), "stream.poll_next");
    let frame = match ready!(res) {
        None => return Poll::Ready(Err(CopyError::Stream(None))),
        Some(frame) => frame.map_err(|e| CopyError::Stream(Some(e)))?,
    };

    tracing::info!("sink.start_send");
    sink.start_send(frame).map_err(CopyError::Sink)?;

    Poll::Ready(Ok(()))
}

enum Who {
    Send,
    Recv,
}

impl<S, M, C> Mux<S, M, C>
where
    M: Send + 'static,
    S: AsyncRead + AsyncWrite,
    C: Encoder<M, Error = io::Error> + Decoder<Item = M, Error = io::Error>,
{
    fn validate_shutdown(self: Pin<&mut Self>, e: Error, who: Who) -> Result<(), Error> {
        if !matches!(e, Error::ServerHangup) {
            return Err(e);
        }

        let this = self.project();

        match this.half_shutdown_behavior {
            HalfShutdownBehavior::ShutdownBoth => return Err(e),
            HalfShutdownBehavior::KeepOne => match who {
                Who::Send => {
                    // server hanged during our attepmt to send to it
                    // if other part is already dead move on to shutdown the Mux
                    if !*this.recv_alive {
                        return Err(e);
                    }
                    // The other half is fine
                    // mark our side as dead
                    *this.send_alive = false;
                }
                Who::Recv => {
                    // server hanged during our attempt to receive from it
                    // if other part is already dead move on to shutdown the Mux
                    if !*this.send_alive {
                        return Err(e);
                    }
                    // The other half is fine
                    // mark our side as dead
                    *this.recv_alive = false;
                }
            },
        }

        Ok(())
    }

    fn poll_send(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<<Self as Future>::Output> {
        let this = self.project();

        poll_copy(this.framed, this.sender, cx).map_err(|e| match e {
            CopyError::Stream(Some(e)) => Error::from(e),
            CopyError::Stream(None) => Error::ClientHangup,
            CopyError::Sink(_) => Error::ServerHangup,
        })
    }

    fn poll_recv(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<<Self as Future>::Output> {
        let this = self.project();

        // Channel's receiver returns plain `M` so we have to wrap it.
        let receiver = this.receiver.map(Ok::<M, std::convert::Infallible>);
        pin!(receiver);

        poll_copy(receiver, this.framed, cx).map_err(|e| match e {
            CopyError::Stream(_) => Error::ServerHangup,
            CopyError::Sink(_) => Error::ClientHangup,
        })
    }

    fn poll_main(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<<Self as Future>::Output> {
        let mut send_pending = false;
        let mut recv_pending = false;

        loop {
            tracing::info!(send_pending, recv_pending, "iteration");
            if send_pending && recv_pending {
                return Poll::Pending;
            }

            if !send_pending && self.as_mut().send_alive {
                match inspect!(self.as_mut().poll_send(cx), "poll_send") {
                    Poll::Ready(Err(e)) => self.as_mut().validate_shutdown(e, Who::Send)?,
                    Poll::Ready(Ok(())) => { /* send succeeded */ }
                    Poll::Pending => send_pending = true,
                }
            }

            if !recv_pending && self.recv_alive {
                match inspect!(self.as_mut().poll_recv(cx), "poll_recv") {
                    Poll::Ready(Err(e)) => self.as_mut().validate_shutdown(e, Who::Recv)?,
                    Poll::Ready(Ok(())) => { /* receive succeeded */ }
                    Poll::Pending => recv_pending = true,
                }
            }
        }
    }
}
