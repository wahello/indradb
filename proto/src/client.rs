use std::convert::TryInto;
use std::error::Error as StdError;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::ConversionError;

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::transport::{Channel, Endpoint, Error as TonicTransportError};
use tonic::{Request, Status};
use uuid::Uuid;

const CHANNEL_CAPACITY: usize = 100;

/// The error returned if a client operation failed.
#[derive(Debug)]
pub enum ClientError {
    /// Conversion between an IndraDB and its protobuf equivalent failed.
    Conversion { inner: ConversionError },
    /// A gRPC error.
    Grpc { inner: Status },
    /// A transport error.
    Transport { inner: TonicTransportError },
    /// The gRPC channel has been closed.
    ChannelClosed,
}

impl StdError for ClientError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match *self {
            ClientError::Conversion { ref inner } => Some(inner),
            ClientError::Grpc { ref inner } => Some(inner),
            ClientError::Transport { ref inner } => Some(inner),
            _ => None,
        }
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            ClientError::Conversion { ref inner } => inner.fmt(f),
            ClientError::Grpc { ref inner } => write!(f, "grpc error: {}", inner),
            ClientError::Transport { ref inner } => write!(f, "transport error: {}", inner),
            ClientError::ChannelClosed => write!(f, "failed to send request: channel closed"),
        }
    }
}

impl From<ConversionError> for ClientError {
    fn from(err: ConversionError) -> Self {
        ClientError::Conversion { inner: err }
    }
}

impl From<Status> for ClientError {
    fn from(err: Status) -> Self {
        ClientError::Grpc { inner: err }
    }
}

impl From<TonicTransportError> for ClientError {
    fn from(err: TonicTransportError) -> Self {
        ClientError::Transport { inner: err }
    }
}

impl<T> From<mpsc::error::SendError<T>> for ClientError {
    fn from(_: mpsc::error::SendError<T>) -> Self {
        ClientError::ChannelClosed
    }
}

/// A higher-level client implementation.
///
/// This should be better suited than the low-level client auto-generated by
/// gRPC/tonic in virtually every case, unless you want to avoid the cost of
/// translating between protobuf types and their IndraDB equivalents. The
/// interface is designed to resemble `indradb::Database`, but async.
#[derive(Clone)]
pub struct Client(crate::ProtoClient<Channel>);

impl Client {
    /// Creates a new client.
    ///
    /// # Arguments
    /// * `endpoint`: The server endpoint.
    pub async fn new(endpoint: Endpoint) -> Result<Self, ClientError> {
        let client = crate::ProtoClient::connect(endpoint).await?;
        Ok(Client(client))
    }

    /// Pings the server.
    pub async fn ping(&mut self) -> Result<(), ClientError> {
        self.0.ping(()).await?;
        Ok(())
    }

    /// Syncs persisted content. Depending on the datastore implementation,
    /// this has different meanings - including potentially being a no-op.
    pub async fn sync(&mut self) -> Result<(), ClientError> {
        self.0.sync(()).await?;
        Ok(())
    }

    /// Creates a new vertex. Returns whether the vertex was successfully
    /// created - if this is false, it's because a vertex with the same UUID
    /// already exists.
    ///
    /// # Arguments
    /// * `vertex`: The vertex to create.
    pub async fn create_vertex(&mut self, vertex: &indradb::Vertex) -> Result<bool, ClientError> {
        let vertex: crate::Vertex = vertex.clone().into();
        let res = self.0.create_vertex(vertex).await?;
        Ok(res.into_inner().created)
    }

    /// Creates a new vertex with just a type specification. As opposed to
    /// `create_vertex`, this is used when you do not want to manually specify
    /// the vertex's UUID. Returns the new vertex's UUID.
    ///
    /// # Arguments
    /// * `t`: The type of the vertex to create.
    pub async fn create_vertex_from_type(&mut self, t: indradb::Identifier) -> Result<Uuid, ClientError> {
        let t: crate::Identifier = t.into();
        let res = self.0.create_vertex_from_type(t).await?;
        Ok(res.into_inner().try_into()?)
    }

    /// Creates a new edge. If the edge already exists, this will update it
    /// with a new update datetime. Returns whether the edge was successfully
    /// created - if this is false, it's because one of the specified vertices
    /// is missing.
    ///
    /// # Arguments
    /// * `edge`: The edge to create.
    pub async fn create_edge(&mut self, edge: &indradb::Edge) -> Result<bool, ClientError> {
        let edge: crate::Edge = edge.clone().into();
        let res = self.0.create_edge(edge).await?;
        Ok(res.into_inner().created)
    }

    /// Gets values specified by a query.
    ///
    /// # Arguments
    /// * `q`: The query to run.
    pub async fn get<Q: Into<indradb::Query>>(&mut self, q: Q) -> Result<Vec<indradb::QueryOutputValue>, ClientError> {
        let q: crate::Query = q.into().into();
        let mut output = Vec::<indradb::QueryOutputValue>::new();
        let mut res = self.0.get(q).await?.into_inner();
        while let Some(res) = res.next().await {
            output.push(res?.try_into()?);
        }
        Ok(output)
    }

    /// Deletes values specified by a query.
    ///
    /// # Arguments
    /// * `q`: The query to run.
    pub async fn delete<Q: Into<indradb::Query>>(&mut self, q: Q) -> Result<(), ClientError> {
        let q: crate::Query = q.into().into();
        self.0.delete(q).await?;
        Ok(())
    }

    /// Sets properties.
    ///
    /// # Arguments
    /// * `q`: The query to run.
    /// * `name`: The property name.
    /// * `value`: The property value.
    pub async fn set_properties<Q: Into<indradb::Query>>(
        &mut self,
        q: Q,
        name: indradb::Identifier,
        value: &indradb::Json,
    ) -> Result<(), ClientError> {
        let name: crate::Identifier = name.into();
        let value: crate::Json = value.clone().into();
        let req = Request::new(crate::SetPropertiesRequest {
            q: Some(q.into().into()),
            name: name.into(),
            value: value.clone().into(),
        });
        self.0.set_properties(req).await?;
        Ok(())
    }

    /// Bulk inserts many vertices, edges, and/or properties.
    ///
    /// Note that datastores have discretion on how to approach safeguard vs
    /// performance tradeoffs. In particular:
    /// * If the datastore is disk-backed, it may or may not flush before
    ///   returning.
    /// * The datastore might not verify for correctness; e.g., it might not
    ///   ensure that the relevant vertices exist before inserting an edge.
    /// If you want maximum protection, use the equivalent functions in
    /// transactions, which will provide more safeguards.
    ///
    /// # Arguments
    /// * `items`: The items to insert.
    pub async fn bulk_insert(&mut self, items: Vec<indradb::BulkInsertItem>) -> Result<(), ClientError> {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let last_err: Arc<Mutex<Option<ClientError>>> = Arc::new(Mutex::new(None));

        {
            let last_err = last_err.clone();
            tokio::spawn(async move {
                for item in items.into_iter() {
                    if let Err(err) = tx.send(item.into()).await {
                        *last_err.lock().unwrap() = Some(err.into());
                        return;
                    }
                }
            });
        }

        self.0.bulk_insert(Request::new(ReceiverStream::new(rx))).await?;

        let mut last_err = last_err.lock().unwrap();
        if last_err.is_some() {
            Err(last_err.take().unwrap())
        } else {
            Ok(())
        }
    }

    pub async fn index_property(&mut self, name: indradb::Identifier) -> Result<(), ClientError> {
        let request = Request::new(crate::IndexPropertyRequest {
            name: Some(name.into()),
        });
        self.0.index_property(request).await?;
        Ok(())
    }

    pub async fn execute_plugin(&mut self, name: &str, arg: indradb::Json) -> Result<indradb::Json, ClientError> {
        let req = Request::new(crate::ExecutePluginRequest {
            name: name.to_string(),
            arg: Some(arg.into()),
        });
        let res = self.0.execute_plugin(req).await?;
        match res.into_inner().value {
            Some(value) => Ok(value.try_into()?),
            None => Ok(indradb::Json::new(serde_json::Value::Null)),
        }
    }
}
