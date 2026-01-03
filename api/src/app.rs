use axum::routing::{get, post};
use axum::Router;
use fhir_core::{Error, FhirVersion};
use fhir_store::ResourceStore;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

use crate::handlers::{create, health, method_not_allowed, not_found, read, update, vread};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn ResourceStore>,
    pub version: FhirVersion,
    pub dependencies: Arc<Vec<Dependency>>,
}

#[derive(Clone)]
pub struct Dependency {
    pub name: &'static str,
    pub check: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}

#[derive(Clone)]
pub struct Service {
    state: AppState,
}

impl Service {
    pub fn new(
        store: Arc<dyn ResourceStore>,
        version: FhirVersion,
        dependencies: Vec<Dependency>,
    ) -> Service {
        Service {
            state: AppState {
                store,
                version,
                dependencies: Arc::new(dependencies),
            },
        }
    }

    pub fn router(&self) -> Router<()> {
        Router::new()
            .route("/health", get(health))
            .route("/{type}/{id}", get(read).put(update))
            .route("/{type}/{id}/_history/{vid}", get(vread))
            .route("/{type}", post(create))
            .fallback(not_found)
            .method_not_allowed_fallback(method_not_allowed)
            .with_state(self.state.clone())
    }

    pub async fn bind(&self, addr: SocketAddr) -> Result<Bound, Error> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|error| Error::Internal(format!("cannot bind {addr}: {error}")))?;
        Ok(Bound {
            listener,
            router: self.router(),
        })
    }

    pub async fn serve(&self, addr: SocketAddr) -> Result<(), Error> {
        self.bind(addr).await?.serve().await
    }
}

pub struct Bound {
    listener: TcpListener,
    router: Router<()>,
}

impl Bound {
    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        self.listener
            .local_addr()
            .map_err(|error| Error::Internal(format!("cannot read the bound address: {error}")))
    }

    pub async fn serve(self) -> Result<(), Error> {
        axum::serve(self.listener, self.router)
            .await
            .map_err(|error| Error::Internal(format!("server error: {error}")))
    }
}