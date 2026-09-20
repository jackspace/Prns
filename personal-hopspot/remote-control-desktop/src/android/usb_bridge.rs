use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::Notify;

pub struct AndroidUsbBridge {
    inbound: Arc<Mutex<Option<UnboundedSender<Vec<u8>>>>>,
    pending_inbound: Arc<Mutex<VecDeque<Vec<u8>>>>,
    outbound: Arc<Mutex<VecDeque<u8>>>,
    connected: Arc<AtomicBool>,
    rescan: Arc<Notify>,
}

const PENDING_INBOUND_CHUNKS: usize = 8;

impl Clone for AndroidUsbBridge {
    fn clone(&self) -> Self {
        Self {
            inbound: Arc::clone(&self.inbound),
            pending_inbound: Arc::clone(&self.pending_inbound),
            outbound: Arc::clone(&self.outbound),
            connected: Arc::clone(&self.connected),
            rescan: Arc::clone(&self.rescan),
        }
    }
}

impl AndroidUsbBridge {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inbound: Arc::new(Mutex::new(None)),
            pending_inbound: Arc::new(Mutex::new(VecDeque::new())),
            outbound: Arc::new(Mutex::new(VecDeque::new())),
            connected: Arc::new(AtomicBool::new(false)),
            rescan: Arc::new(Notify::new()),
        }
    }

    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::Release);
        self.rescan.notify_one();
    }

    pub fn push_inbound(&self, bytes: &[u8]) {
        if let Ok(guard) = self.inbound.lock() {
            if let Some(sender) = guard.as_ref() {
                let _ = sender.send(bytes.to_vec());
                return;
            }
        }
        if let Ok(mut pending) = self.pending_inbound.lock() {
            if pending.len() >= PENDING_INBOUND_CHUNKS {
                pending.pop_front();
            }
            pending.push_back(bytes.to_vec());
        }
    }

    pub fn pull_outbound(&self, out: &mut [u8]) -> usize {
        let Ok(mut queue) = self.outbound.lock() else {
            return 0;
        };
        let mut written = 0;
        for slot in out.iter_mut() {
            let Some(byte) = queue.pop_front() else {
                break;
            };
            *slot = byte;
            written += 1;
        }
        written
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn rescan(&self) -> Arc<Notify> {
        Arc::clone(&self.rescan)
    }

    #[must_use]
    pub fn open_stream(&self) -> BridgeStream {
        let (tx, rx) = unbounded_channel::<Vec<u8>>();
        if let Ok(mut pending) = self.pending_inbound.lock() {
            while let Some(chunk) = pending.pop_front() {
                let _ = tx.send(chunk);
            }
        }
        if let Ok(mut guard) = self.inbound.lock() {
            *guard = Some(tx);
        }
        BridgeStream {
            rx,
            leftover: Vec::new(),
            pos: 0,
            outbound: Arc::clone(&self.outbound),
        }
    }
}

impl Default for AndroidUsbBridge {
    fn default() -> Self {
        Self::new()
    }
}

pub struct BridgeStream {
    rx: UnboundedReceiver<Vec<u8>>,
    leftover: Vec<u8>,
    pos: usize,
    outbound: Arc<Mutex<VecDeque<u8>>>,
}

impl AsyncRead for BridgeStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.pos >= self.leftover.len() {
            match self.rx.poll_recv(cx) {
                Poll::Ready(Some(chunk)) => {
                    self.leftover = chunk;
                    self.pos = 0;
                }
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
        let available = self.leftover.len() - self.pos;
        let n = available.min(buf.remaining());
        let start = self.pos;
        buf.put_slice(&self.leftover[start..start + n]);
        self.pos += n;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for BridgeStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if let Ok(mut queue) = self.outbound.lock() {
            queue.extend(buf.iter().copied());
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
