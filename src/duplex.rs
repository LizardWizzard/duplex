use std::io;
use std::io::ErrorKind;

use bytes::Buf;
use tokio::io::Interest;
use tokio::net::TcpStream;

use bytes::BytesMut;
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

fn bail_on_non_would_block(r: io::Result<usize>, ctx: &'static str) -> io::Result<Option<usize>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            if e.kind() == ErrorKind::WouldBlock {
                println!("would block {}", ctx);
                return Ok(None);
            } else {
                return Err(e);
            }
        }
    }
}

fn try_send<T>(sender: &Sender<T>, frame: T) -> Result<Option<T>, ()> {
    match sender.try_send(frame) {
        Ok(_) => Ok(None),
        Err(e) => match e {
            TrySendError::Full(frame) => Ok(Some(frame)),
            TrySendError::Closed(_) => Err(()),
        },
    }
}

fn try_recv<T>(receiver: &mut Receiver<T>) -> Result<Option<T>, ()> {
    match receiver.try_recv() {
        Ok(t) => Ok(Some(t)),
        Err(e) => match e {
            TryRecvError::Empty => Ok(None),
            TryRecvError::Disconnected => Err(()),
        },
    }
}

struct SenderCtx<F, D: Decoder<Item = F>> {
    sender: Sender<F>,
    decoder: D,
    decode_buf: BytesMut,
}

impl<F, D: Decoder<Item = F>> SenderCtx<F, D> {
    fn new(sender: Sender<F>, decoder: D) -> Self {
        Self {
            sender,
            decoder,
            decode_buf: BytesMut::new(),
        }
    }
}

struct ReceiverCtx<F, E: Encoder<F>> {
    receiver: Receiver<F>,
    encoder: E,
    // encode buf contains only one message we've received from processor
    // if we want to store more messages we need to take care of fairness
    encode_buf: BytesMut,
}

impl<F, E: Encoder<F>> ReceiverCtx<F, E> {
    fn new(receiver: Receiver<F>, encoder: E) -> Self {
        Self {
            receiver,
            encoder,
            encode_buf: BytesMut::new(),
        }
    }
}

pub async fn spawn_duplex<F, E, D>(
    stream: TcpStream,
    decoder: D,
    encoder: E,
    sender: Sender<F>,
    receiver: Receiver<F>,
) -> Result<(), Error<D::Error, E::Error>>
where
    E: Encoder<F>,
    E::Error: std::error::Error,
    D: Decoder<Item = F>,
    D::Error: std::error::Error,
{
    let mut sender_ctx = SenderCtx::new(sender, decoder);
    let mut receiver_ctx = ReceiverCtx::new(receiver, encoder);

    // send to processor
    let mut pending_send_slot: Option<F> = None;

    loop {
        let mut read_blocked = false;
        let mut write_blocked = false;
        let mut receive_blocked = false;

        match pending_send_slot.take() {
            Some(frame) => {
                pending_send_slot = try_send(&sender_ctx.sender, frame)
                    .map_err(|_| Error::Internal("processor is dead"))?;
            }
            None => {
                'inner: loop {
                    match bail_on_non_would_block(
                        stream.try_read_buf(&mut sender_ctx.decode_buf),
                        "try_read",
                    )
                    .map_err(|e| Error::Io(e))?
                    {
                        // we read something, feed the buf to decoder
                        Some(_) => {
                            if let Some(frame) = sender_ctx
                                .decoder
                                .decode(&mut sender_ctx.decode_buf)
                                .map_err(|e| Error::Decode(e))?
                            {
                                // we managed to decode the frame, try to send it, if channel is full store frame in slot
                                pending_send_slot = try_send(&sender_ctx.sender, frame)
                                    .map_err(|_| Error::Internal("processor is dead"))?;
                                break 'inner;
                            }
                        }
                        None => {
                            read_blocked = true;
                            break 'inner;
                        }
                    }
                }
            }
        }

        if receiver_ctx.encode_buf.is_empty() {
            // we dont have something to write into the socket
            // try to receive and write if we got something (without attempting to get next message to be fair)
            match try_recv(&mut receiver_ctx.receiver)
                .map_err(|_| Error::Internal("processor is dead"))?
            {
                Some(frame) => {
                    receiver_ctx
                        .encoder
                        .encode(frame, &mut receiver_ctx.encode_buf)
                        .map_err(|e| Error::Encode(e))?;

                    // TODO cleanup, repetitive
                    'inner: loop {
                        match bail_on_non_would_block(
                            stream.try_write(&receiver_ctx.encode_buf),
                            "try_write_1",
                        )
                        .map_err(|e| Error::Io(e))?
                        {
                            Some(written) => {
                                receiver_ctx.encode_buf.advance(written);
                                if receiver_ctx.encode_buf.is_empty() {
                                    // do not attempt to receive new message, go to other duties to be fair
                                    break 'inner;
                                }
                            }
                            None => {
                                write_blocked = true;
                                break 'inner;
                            }
                        }
                    }
                }
                None => receive_blocked = true,
            }
        } else {
            // we have something to write into the socket
            // try to write it and if we wrote everything try to get new message but dont start writing it to be fair
            'inner: loop {
                match bail_on_non_would_block(
                    stream.try_write(&receiver_ctx.encode_buf),
                    "try_write_2",
                )
                .map_err(|e| Error::Io(e))?
                {
                    Some(written) => {
                        receiver_ctx.encode_buf.advance(written);
                        if receiver_ctx.encode_buf.is_empty() {
                            match try_recv(&mut receiver_ctx.receiver)
                                .map_err(|_| Error::Internal("processor is dead"))?
                            {
                                Some(frame) => {
                                    receiver_ctx
                                        .encoder
                                        .encode(frame, &mut receiver_ctx.encode_buf)
                                        .map_err(|e| Error::Encode(e))?;
                                }
                                None => {
                                    receive_blocked = true;
                                    break 'inner;
                                }
                            }
                        }
                    }
                    None => {
                        write_blocked = true;
                        break 'inner;
                    }
                }
            }
        };

        // what we need to wait for in order to make progress?
        // if we would blocked on read, we wait for Interest::READABLE
        // if we would blocked on write, we wait for Interest::WRITABLE
        dbg!(read_blocked, write_blocked);
        let interest = {
            match (read_blocked, write_blocked) {
                (false, false) => None,
                (false, true) => Some(Interest::WRITABLE),
                (true, false) => Some(Interest::READABLE),
                (true, true) => Some(Interest::READABLE | Interest::WRITABLE),
            }
        };

        tokio::select! {
            _ = stream.ready(interest.unwrap()), if interest.is_some() => {
                println!("stream ready")
            },
            // if we have send slot we must've blocked on try_send so we can wait on sender.reserve
            _ = sender_ctx.sender.reserve(), if pending_send_slot.is_some() => {
                println!("sender ready")
            },
            // TODO if we blocked on recv we can wait on receiver.peek()
            _ = tokio::task::yield_now(), if receive_blocked => {}
        }
    }
    // no shutdown other than with Error
}

