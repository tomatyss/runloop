#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::{IpcClient, IpcServer, connect_ipc_client, spawn_ipc_server};

#[cfg(not(unix))]
pub(crate) mod unix {
    use crate::{BusError, Message, PublisherKind, Server};
    use std::path::Path;
    use std::sync::Arc;

    pub(crate) struct IpcServer;

    pub(crate) fn spawn_ipc_server(
        _path: &Path,
        _server: Arc<Server>,
    ) -> std::io::Result<Option<IpcServer>> {
        Ok(None)
    }

    pub(crate) async fn connect_ipc_client(
        path: &Path,
        _kind: PublisherKind,
    ) -> Result<IpcClient, BusError> {
        Err(BusError::NotFound(path.to_path_buf()))
    }

    #[derive(Clone)]
    pub(crate) struct IpcClient;

    impl IpcClient {
        pub(crate) async fn publish(
            &self,
            _topic: &str,
            _message: Message,
        ) -> Result<(), BusError> {
            Err(BusError::Closed)
        }

        pub(crate) async fn subscribe(
            &self,
            _topic: &str,
        ) -> Result<
            (
                tokio::sync::mpsc::Receiver<Message>,
                Box<dyn FnOnce() + Send + Sync>,
            ),
            BusError,
        > {
            Err(BusError::Closed)
        }
    }
}
