use std::collections::HashMap;
use std::io;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::{error, warn};

use crate::{BusError, Message, PublisherKind, Server};
use runloop_rmp::Header;
use runloop_rmp::header::{DEFAULT_MAX_FRAME_LEN as MAX_BODY_LEN, HEADER_LEN};

#[derive(Debug)]
pub(crate) struct IpcServer {
    path: PathBuf,
    handle: JoinHandle<()>,
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.handle.abort();
        if let Err(err) = std::fs::remove_file(&self.path) {
            if err.kind() == io::ErrorKind::NotFound {
                return;
            }
            warn!(path = ?self.path, ?err, "failed to remove ipc socket");
        }
    }
}

pub(crate) fn spawn_ipc_server(path: &Path, server: Arc<Server>) -> io::Result<Option<IpcServer>> {
    if path.as_os_str().is_empty() {
        return Ok(None);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    let path_buf = path.to_path_buf();
    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let server = server.clone();
                    tokio::spawn(async move {
                        if let Err(err) = handle_client(stream, server).await {
                            warn!(?err, "ipc client session ended with error");
                        }
                    });
                }
                Err(err) => {
                    error!(?err, "ipc listener accept failed");
                    break;
                }
            }
        }
    });
    Ok(Some(IpcServer {
        path: path_buf,
        handle,
    }))
}

pub(crate) async fn connect_ipc_client(
    path: &Path,
    kind: PublisherKind,
) -> Result<IpcClient, BusError> {
    let stream = UnixStream::connect(path)
        .await
        .map_err(|_| BusError::NotFound(path.to_path_buf()))?;
    IpcClient::new(stream, kind).await
}

#[derive(Clone)]
pub(crate) struct IpcClient {
    inner: Arc<IpcClientInner>,
}

struct IpcClientInner {
    cmd_tx: mpsc::UnboundedSender<IpcCommand>,
    subscriptions: Mutex<HashMap<u64, mpsc::Sender<Message>>>,
    next_sub_id: AtomicU64,
    next_cmd_id: AtomicU64,
    pending: Mutex<HashMap<u64, PendingAck>>,
}

enum PendingAck {
    Publish(tokio::sync::oneshot::Sender<Result<(), BusError>>),
    Subscribe {
        sub_id: u64,
        sender: tokio::sync::oneshot::Sender<Result<(), BusError>>,
    },
}

impl IpcClient {
    async fn new(stream: UnixStream, kind: PublisherKind) -> Result<Self, BusError> {
        let (reader, writer) = stream.into_split();
        let writer = Arc::new(tokio::sync::Mutex::new(writer));
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let inner = Arc::new(IpcClientInner {
            cmd_tx,
            subscriptions: Mutex::new(HashMap::new()),
            next_sub_id: AtomicU64::new(1),
            next_cmd_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
        });
        let writer_clone = writer.clone();
        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                if let Err(err) = write_frame(&writer_clone, &cmd).await {
                    warn!(?err, "ipc client writer error");
                    break;
                }
            }
        });

        let inner_events = inner.clone();
        tokio::spawn(async move {
            if let Err(err) = read_frames(reader, inner_events).await {
                warn!(?err, "ipc client reader error");
            }
        });

        let client = Self { inner };
        client.send_command(IpcCommand::Hello { kind })?;
        Ok(client)
    }

    fn next_cmd_id(&self) -> u64 {
        self.inner.next_cmd_id.fetch_add(1, Ordering::Relaxed)
    }

    fn send_command(&self, cmd: IpcCommand) -> Result<(), BusError> {
        self.inner.cmd_tx.send(cmd).map_err(|_| BusError::Closed)
    }

    pub async fn publish(&self, topic: &str, message: Message) -> Result<(), BusError> {
        let id = self.next_cmd_id();
        let (tx, rx) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .insert(id, PendingAck::Publish(tx));
        if let Err(err) = self.send_command(IpcCommand::Publish {
            id,
            topic: topic.to_string(),
            message: SerializableMessage::from(message),
        }) {
            self.inner.pending.lock().remove(&id);
            return Err(err);
        }
        match timeout(Duration::from_secs(1), rx).await {
            Ok(result) => result.unwrap_or(Err(BusError::Closed)),
            Err(_) => {
                self.inner.pending.lock().remove(&id);
                Err(BusError::Closed)
            }
        }
    }

    pub async fn subscribe(
        &self,
        topic: &str,
    ) -> Result<(mpsc::Receiver<Message>, Box<dyn FnOnce() + Send + Sync>), BusError> {
        let sub_id = self.inner.next_sub_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(64);
        self.inner.subscriptions.lock().insert(sub_id, tx);
        let id = self.next_cmd_id();
        let (ack_tx, ack_rx) = oneshot::channel();
        self.inner.pending.lock().insert(
            id,
            PendingAck::Subscribe {
                sub_id,
                sender: ack_tx,
            },
        );
        if let Err(err) = self.send_command(IpcCommand::Subscribe {
            id,
            topic: topic.to_string(),
            sub_id,
        }) {
            self.inner.pending.lock().remove(&id);
            self.inner.subscriptions.lock().remove(&sub_id);
            return Err(err);
        }
        match timeout(Duration::from_secs(1), ack_rx).await {
            Ok(ack) => match ack.unwrap_or(Err(BusError::Closed)) {
                Ok(()) => {}
                Err(err) => {
                    self.inner.remove_subscription(sub_id);
                    self.inner.pending.lock().remove(&id);
                    return Err(err);
                }
            },
            Err(_) => {
                // Timed out waiting for ack; assume server is gone.
                self.inner.remove_subscription(sub_id);
                self.inner.pending.lock().remove(&id);
                return Err(BusError::Closed);
            }
        };
        let inner = self.inner.clone();
        let dropper = Box::new(move || {
            inner.cleanup_subscription(sub_id);
        });
        Ok((rx, dropper))
    }
}

const MAX_FRAME_LEN: usize = HEADER_LEN as usize + MAX_BODY_LEN as usize + 4;

async fn read_frames(
    mut reader: tokio::net::unix::OwnedReadHalf,
    inner: Arc<IpcClientInner>,
) -> io::Result<()> {
    let result: io::Result<()> = async {
        loop {
            let mut len_buf = [0u8; 4];
            if let Err(err) = reader.read_exact(&mut len_buf).await {
                if err.kind() == io::ErrorKind::UnexpectedEof {
                    break;
                }
                return Err(err);
            }
            let len = u32::from_be_bytes(len_buf) as usize;
            if len > MAX_FRAME_LEN {
                return Err(io::Error::other("ipc frame too large"));
            }
            let mut payload = vec![0u8; len];
            if let Err(err) = reader.read_exact(&mut payload).await {
                if err.kind() == io::ErrorKind::UnexpectedEof {
                    break;
                }
                return Err(err);
            }
            match decode::<IpcEvent>(&payload)? {
                IpcEvent::HelloAck => {}
                IpcEvent::Message { sub_id, message } => {
                    inner.forward_message(sub_id, message);
                }
                IpcEvent::PublishAck { id, result } => {
                    if let Some(PendingAck::Publish(tx)) = inner.pending.lock().remove(&id) {
                        let _ = tx.send(result);
                    }
                }
                IpcEvent::SubscribeAck { id, result } => {
                    if let Some(PendingAck::Subscribe { sub_id, sender }) =
                        inner.pending.lock().remove(&id)
                    {
                        if result.is_err() {
                            inner.remove_subscription(sub_id);
                        }
                        let _ = sender.send(result);
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    inner.teardown();
    result
}

async fn write_frame(
    writer: &Arc<tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>>,
    cmd: &IpcCommand,
) -> io::Result<()> {
    let payload = encode(cmd)?;
    let len = (payload.len() as u32).to_be_bytes();
    let mut guard = writer.lock().await;
    guard.write_all(&len).await?;
    guard.write_all(&payload).await
}

async fn handle_client(stream: UnixStream, server: Arc<Server>) -> io::Result<()> {
    let (reader, writer) = stream.into_split();
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let mut kind = PublisherKind::Agent;
    let mut subscriptions: HashMap<u64, oneshot::Sender<()>> = HashMap::new();
    let mut reader = reader;

    loop {
        let mut len_buf = [0u8; 4];
        reader.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_FRAME_LEN {
            return Err(io::Error::other("ipc frame too large"));
        }
        let mut payload = vec![0u8; len];
        reader.read_exact(&mut payload).await?;
        let cmd: IpcCommand = decode(&payload)?;
        match cmd {
            IpcCommand::Hello { kind: client_kind } => {
                // For security, remote clients are always treated as agents.
                if !matches!(client_kind, PublisherKind::Agent) {
                    warn!(
                        "ipc client requested {:?} publisher kind; defaulting to Agent",
                        client_kind
                    );
                }
                kind = PublisherKind::Agent;
                let ack = IpcEvent::HelloAck;
                write_event(&writer, &ack).await?;
            }
            IpcCommand::Publish { id, topic, message } => {
                let msg = Message::new(message.header.clone(), Bytes::from(message.body))
                    .map_err(|_| io::Error::other("invalid message"))?;
                let result = server.publish(&topic, msg, true, kind).await;
                let event = IpcEvent::PublishAck { id, result };
                write_event(&writer, &event).await?;
            }
            IpcCommand::Subscribe { id, topic, sub_id } => match server.subscribe(&topic).await {
                Ok(mut sub) => {
                    let writer_clone = writer.clone();
                    let (stop_tx, mut stop_rx) = oneshot::channel();
                    tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                msg = sub.next() => {
                                    match msg {
                                        Some(message) => {
                                            let serial = SerializableMessage::from(message);
                                            if write_event(&writer_clone, &IpcEvent::Message { sub_id, message: serial }).await.is_err() {
                                                break;
                                            }
                                        }
                                        None => break,
                                    }
                                }
                                _ = &mut stop_rx => break,
                            }
                        }
                    });
                    subscriptions.insert(sub_id, stop_tx);
                    write_event(&writer, &IpcEvent::SubscribeAck { id, result: Ok(()) }).await?;
                }
                Err(err) => {
                    write_event(
                        &writer,
                        &IpcEvent::SubscribeAck {
                            id,
                            result: Err(err),
                        },
                    )
                    .await?;
                }
            },
            IpcCommand::Unsubscribe { sub_id } => {
                if let Some(stop_tx) = subscriptions.remove(&sub_id) {
                    let _ = stop_tx.send(());
                }
            }
        }
    }
}

impl IpcClientInner {
    fn forward_message(self: &Arc<Self>, sub_id: u64, serial: SerializableMessage) {
        let tx_opt = {
            let map = self.subscriptions.lock();
            map.get(&sub_id).cloned()
        };
        if let Some(tx) = tx_opt {
            let msg: Message = serial.into();
            match tx.try_send(msg.clone()) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    let tx_clone = tx.clone();
                    let inner = Arc::clone(self);
                    tokio::spawn(async move {
                        if tx_clone.send(msg).await.is_err() {
                            inner.cleanup_subscription(sub_id);
                        }
                    });
                }
                Err(TrySendError::Closed(_)) => {
                    self.remove_subscription(sub_id);
                }
            }
        }
    }

    fn cleanup_subscription(self: &Arc<Self>, sub_id: u64) {
        if self.subscriptions.lock().remove(&sub_id).is_some() {
            let _ = self.cmd_tx.send(IpcCommand::Unsubscribe { sub_id });
        }
    }

    fn remove_subscription(&self, sub_id: u64) {
        self.subscriptions.lock().remove(&sub_id);
    }

    fn teardown(self: &Arc<Self>) {
        let pending = {
            let mut guard = self.pending.lock();
            mem::take(&mut *guard)
        };
        for (_, ack) in pending {
            match ack {
                PendingAck::Publish(tx) => {
                    let _ = tx.send(Err(BusError::Closed));
                }
                PendingAck::Subscribe { sender, .. } => {
                    let _ = sender.send(Err(BusError::Closed));
                }
            }
        }
        let subscriptions = {
            let mut guard = self.subscriptions.lock();
            mem::take(&mut *guard)
        };
        drop(subscriptions);
    }
}

async fn write_event(
    writer: &Arc<tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>>,
    event: &IpcEvent,
) -> io::Result<()> {
    let payload = encode(event)?;
    let len = (payload.len() as u32).to_be_bytes();
    let mut guard = writer.lock().await;
    guard.write_all(&len).await?;
    guard.write_all(&payload).await
}

fn to_io_error(err: serde_json::Error) -> io::Error {
    io::Error::other(err)
}

fn encode<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(to_io_error)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    serde_json::from_slice(bytes).map_err(to_io_error)
}

#[derive(Serialize, Deserialize)]
enum IpcCommand {
    Hello {
        kind: PublisherKind,
    },
    Publish {
        id: u64,
        topic: String,
        message: SerializableMessage,
    },
    Subscribe {
        id: u64,
        topic: String,
        sub_id: u64,
    },
    Unsubscribe {
        sub_id: u64,
    },
}

#[derive(Serialize, Deserialize)]
enum IpcEvent {
    HelloAck,
    PublishAck {
        id: u64,
        result: Result<(), BusError>,
    },
    SubscribeAck {
        id: u64,
        result: Result<(), BusError>,
    },
    Message {
        sub_id: u64,
        message: SerializableMessage,
    },
}

#[derive(Serialize, Deserialize, Clone)]
struct SerializableMessage {
    header: Header,
    body: Vec<u8>,
}

impl From<Message> for SerializableMessage {
    fn from(message: Message) -> Self {
        Self {
            header: message.header,
            body: message.body.to_vec(),
        }
    }
}

impl From<SerializableMessage> for Message {
    fn from(serial: SerializableMessage) -> Self {
        Message {
            header: serial.header,
            body: Bytes::from(serial.body),
        }
    }
}
