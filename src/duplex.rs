use bytes::Buf;
use bytes::BytesMut;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{self, Poll};
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::io::Interest;
use tokio::net::TcpStream;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tokio_util::codec::Decoder;
use tokio_util::codec::Encoder;

#[derive(thiserror::Error, Debug)]
pub enum Error<D, E> {
    Decode(D),
    Encode(E),
    Io(std::io::Error),
    Internal(&'static str),
}

fn cvt(r: io::Result<usize>) -> io::Result<Option<usize>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            if e.kind() == io::ErrorKind::WouldBlock {
                return Ok(None);
            } else {
                return Err(e);
            }
        }
    }
}

/// Decode bytes into messages and send them via the channel.
struct SenderCtx<M, D> {
    sender: Sender<M>,
    buffer: BytesMut,
    decoder: D,
}

impl<M, D> SenderCtx<M, D>
where
    D: Decoder<Item = M>,
{
    fn new(sender: Sender<M>, decoder: D) -> Self {
        Self {
            sender,
            decoder,
            buffer: BytesMut::new(),
        }
    }

    fn handle(&mut self, stream: &TcpStream) -> Result<(), D::Error> {
        todo!()
    }
}

/// Receive messages via the channel and encode them into bytes.
struct ReceiverCtx<F, E> {
    receiver: Receiver<F>,
    buffer: BytesMut,
    encoder: E,
}

impl<M, E> ReceiverCtx<M, E>
where
    E: Encoder<M>,
{
    fn new(receiver: Receiver<M>, encoder: E) -> Self {
        Self {
            receiver,
            encoder,
            buffer: BytesMut::new(),
        }
    }

    fn handle(&mut self, stream: &TcpStream) -> Result<(), E::Error> {
        todo!()
    }
}

struct Duplex<S, M, D, E> {
    stream: S,
    sender: SenderCtx<M, D>,
    receiver: ReceiverCtx<M, E>,
}

impl<S, M, D, E> Future for Duplex<S, M, D, E>
where
    S: AsyncRead + AsyncWrite + Unpin,
    D: Decoder<Item = M, Error = io::Error>,
    E: Encoder<M, Error = io::Error>,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        todo!()
    }
}
