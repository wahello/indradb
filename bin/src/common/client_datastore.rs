use capnp_rpc::{RpcSystem, twoparty};
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp::Error as CapnpError;
use autogen;
use futures::Future;
use indradb;
use serde_json::value::Value as JsonValue;
use std::thread::sleep;
use std::time::Duration;
use uuid::Uuid;
use std::net::ToSocketAddrs;
use tokio_core::reactor::Core;
use tokio_core::net::TcpStream;
use tokio_io::AsyncRead;
use converters;

pub struct ClientDatastore {
    port: u16
}

impl ClientDatastore {
    pub fn new(port: u16) -> Self {
        Self { port }
    }
}

impl indradb::Datastore<ClientTransaction> for ClientDatastore {
    fn transaction(&self) -> Result<ClientTransaction, indradb::Error> {
        Ok(ClientTransaction::new(self.port))
    }
}

pub struct ClientTransaction {
    port: u16
}

impl ClientTransaction {
    fn new(port: u16) -> Self {
        ClientTransaction { port }
    }
}

impl ClientTransaction {
    fn execute<F, G>(&self, f: F) -> Result<G, indradb::Error>
    where F: FnOnce(autogen::transaction::Client) -> Box<Future<Item=G, Error=CapnpError>> {
        let mut core = Core::new().unwrap();
        let handle = core.handle();
        let addr = format!("127.0.0.1:{}", self.port).to_socket_addrs().unwrap().next().unwrap();

        for _ in 0..5 {
            if let Ok(stream) = core.run(TcpStream::connect(&addr, &handle)) {
                stream.set_nodelay(true).unwrap();
                let (reader, writer) = stream.split();
                let rpc_network = Box::new(twoparty::VatNetwork::new(reader, writer, Side::Client, Default::default()));
                let mut rpc_system = RpcSystem::new(rpc_network, None);
                let client: autogen::service::Client = rpc_system.bootstrap(Side::Server);
                handle.spawn(rpc_system.map_err(|_e| ()));

                let req = client.ping_request();
                let res = core.run(req.send().promise).unwrap();
                
                if res.get().unwrap().get_ready() {
                    let trans = client.transaction_request().send().pipeline.get_transaction();
                    let future = f(trans);
                    return core.run(future).map_err(|err| format!("{:?}", err).into());
                }
            }

            sleep(Duration::from_secs(1));
        }

        panic!("Could not connect to the server after a few seconds");
    }
}

impl indradb::Transaction for ClientTransaction {
    fn create_vertex(&self, v: &indradb::Vertex) -> Result<bool, indradb::Error> {
        self.execute(move |trans| {
            let mut req = trans.create_vertex_request();
            converters::from_vertex(v, req.get().init_vertex());

            let f = req.send().promise.and_then(move |res| {
                Ok(res.get()?.get_result())
            });

            Box::new(f)
        })
    }

    fn create_vertex_from_type(&self, t: indradb::Type) -> Result<Uuid, indradb::Error> {
        self.execute(move |trans| {
            let mut req = trans.create_vertex_from_type_request();
            req.get().set_t(&t.0);

            let f = req.send().promise.and_then(move |res| {
                let bytes = res.get()?.get_result()?;
                Ok(Uuid::from_bytes(bytes).unwrap())
            });

            Box::new(f)
        })
    }

    fn get_vertices(&self, q: &indradb::VertexQuery) -> Result<Vec<indradb::Vertex>, indradb::Error> {
        self.execute(move |trans| {
            let mut req = trans.get_vertices_request();
            converters::from_vertex_query(&q, req.get().init_q());

            let f = req.send().promise.and_then(move |res| {
                let list = res.get()?.get_result()?;
                let list: Result<Vec<indradb::Vertex>, CapnpError> = list.into_iter().map(|reader| converters::to_vertex(&reader)).collect();
                list
            });

            Box::new(f)
        })
    }

    fn delete_vertices(&self, q: &indradb::VertexQuery) -> Result<(), indradb::Error> {
        self.execute(move |trans| {
            let mut req = trans.delete_vertices_request();
            converters::from_vertex_query(&q, req.get().init_q());

            let f = req.send().promise.and_then(move |res| {
                res.get()?;
                Ok(())
            });

            Box::new(f)
        })
    }

    fn get_vertex_count(&self) -> Result<u64, indradb::Error> {
        self.execute(move |trans| {
            let req = trans.get_vertex_count_request();

            let f = req.send().promise.and_then(move |res| {
                Ok(res.get()?.get_result())
            });

            Box::new(f)
        })
    }

    fn create_edge(&self, e: &indradb::EdgeKey) -> Result<bool, indradb::Error> {
        self.execute(move |trans| {
            let mut req = trans.create_edge_request();
            converters::from_edge_key(e, req.get().init_key());

            let f = req.send().promise.and_then(move |res| {
                Ok(res.get()?.get_result())
            });

            Box::new(f)
        })
    }

    fn get_edges(&self, q: &indradb::EdgeQuery) -> Result<Vec<indradb::Edge>, indradb::Error> {
        self.execute(move |trans| {
            let mut req = trans.get_edges_request();
            converters::from_edge_query(&q, req.get().init_q());

            let f = req.send().promise.and_then(move |res| {
                let list = res.get()?.get_result()?;
                let list: Result<Vec<indradb::Edge>, CapnpError> = list.into_iter().map(|reader| converters::to_edge(&reader)).collect();
                list
            });

            Box::new(f)
        })
    }

    fn delete_edges(&self, q: &indradb::EdgeQuery) -> Result<(), indradb::Error> {
        self.execute(move |trans| {
            let mut req = trans.delete_edges_request();
            converters::from_edge_query(&q, req.get().init_q());

            let f = req.send().promise.and_then(move |res| {
                res.get()?;
                Ok(())
            });

            Box::new(f)
        })
    }

    fn get_edge_count(
        &self,
        id: Uuid,
        type_filter: Option<&indradb::Type>,
        direction: indradb::EdgeDirection,
    ) -> Result<u64, indradb::Error> {
        self.execute(move |trans| {
            let mut req = trans.get_edge_count_request();
            req.get().set_id(id.as_bytes());
            
            if let Some(type_filter) = type_filter {
                req.get().set_type_filter(&type_filter.0);
            }

            req.get().set_direction(converters::from_edge_direction(direction));

            let f = req.send().promise.and_then(move |res| {
                Ok(res.get()?.get_result())
            });

            Box::new(f)
        })
    }

    fn get_vertex_metadata(
        &self,
        q: &indradb::VertexQuery,
        name: &str,
    ) -> Result<Vec<indradb::VertexMetadata>, indradb::Error> {
        self.execute(move |trans| {
            let mut req = trans.get_vertex_metadata_request();
            converters::from_vertex_query(&q, req.get().init_q());
            req.get().set_name(name);

            let f = req.send().promise.and_then(move |res| {
                let list = res.get()?.get_result()?;
                let list: Result<Vec<indradb::VertexMetadata>, CapnpError> = list.into_iter().map(|reader| converters::to_vertex_metadata(&reader)).collect();
                list
            });

            Box::new(f)
        })
    }

    fn set_vertex_metadata(
        &self,
        q: &indradb::VertexQuery,
        name: &str,
        value: &JsonValue,
    ) -> Result<(), indradb::Error> {
        self.execute(move |trans| {
            let mut req = trans.set_vertex_metadata_request();
            converters::from_vertex_query(&q, req.get().init_q());
            req.get().set_name(name);
            req.get().set_value(&value.to_string());

            let f = req.send().promise.and_then(move |res| {
                res.get()?;
                Ok(())
            });

            Box::new(f)
        })
    }

    fn delete_vertex_metadata(&self, q: &indradb::VertexQuery, name: &str) -> Result<(), indradb::Error> {
        self.execute(move |trans| {
            let mut req = trans.delete_vertex_metadata_request();
            converters::from_vertex_query(&q, req.get().init_q());
            req.get().set_name(name);

            let f = req.send().promise.and_then(move |res| {
                res.get()?;
                Ok(())
            });

            Box::new(f)
        })
    }

    fn get_edge_metadata(
        &self,
        q: &indradb::EdgeQuery,
        name: &str,
    ) -> Result<Vec<indradb::EdgeMetadata>, indradb::Error> {
        self.execute(move |trans| {
            let mut req = trans.get_edge_metadata_request();
            converters::from_edge_query(&q, req.get().init_q());
            req.get().set_name(name);

            let f = req.send().promise.and_then(move |res| {
                let list = res.get()?.get_result()?;
                let list: Result<Vec<indradb::EdgeMetadata>, CapnpError> = list.into_iter().map(|reader| converters::to_edge_metadata(&reader)).collect();
                list
            });

            Box::new(f)
        })
    }

    fn set_edge_metadata(&self, q: &indradb::EdgeQuery, name: &str, value: &JsonValue) -> Result<(), indradb::Error> {
        self.execute(move |trans| {
            let mut req = trans.set_edge_metadata_request();
            converters::from_edge_query(&q, req.get().init_q());
            req.get().set_name(name);
            req.get().set_value(&value.to_string());

            let f = req.send().promise.and_then(move |res| {
                res.get()?;
                Ok(())
            });

            Box::new(f)
        })
    }

    fn delete_edge_metadata(&self, q: &indradb::EdgeQuery, name: &str) -> Result<(), indradb::Error> {
        self.execute(move |trans| {
            let mut req = trans.delete_edge_metadata_request();
            converters::from_edge_query(&q, req.get().init_q());
            req.get().set_name(name);

            let f = req.send().promise.and_then(move |res| {
                res.get()?;
                Ok(())
            });

            Box::new(f)
        })
    }
}
