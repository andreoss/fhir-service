use axum::routing::{get, post};
use axum::Router;
use fhir_core::convert::{ApprovedTemplates, Templates};
use fhir_core::search::Registry;
use fhir_core::{Error, FhirVersion};
use fhir_store::ResourceStore;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

use crate::handlers::{
    compartment_definition, compartment_definitions, compartment_search, conditional_delete,
    conditional_patch, conditional_update, create, delete_instance, health,
    instance_history, method_not_allowed, not_found, parameter_refresh, parameter_reindex,
    parameter_status,
    parameter_status_query,
    parameter_status_update, patch_instance, purge_history, read,
    search_system, search_type, system_history, type_history, update, vread,
};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn ResourceStore>,
    pub jobs: Option<Arc<dyn fhir_store::JobStore>>,
    pub outputs: Option<Arc<dyn fhir_store::BulkStore>>,
    pub version: FhirVersion,
    pub dependencies: Arc<Vec<Dependency>>,
    pub registry: Arc<Registry>,
    pub parameters: Arc<tokio::sync::Mutex<()>>,
    pub entries: Arc<tokio::sync::Semaphore>,
    pub templates: Arc<dyn Templates>,
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
                jobs: None,
                outputs: None,
                version,
                dependencies: Arc::new(dependencies),
                registry: Arc::new(Registry::new()),
                parameters: Arc::new(tokio::sync::Mutex::new(())),
                entries: Arc::new(tokio::sync::Semaphore::new(crate::bundle::ENTRIES_AT_ONCE)),
                templates: Arc::new(ApprovedTemplates::default()),
            },
        }
    }

    pub fn with_jobs(self, jobs: Arc<dyn fhir_store::JobStore>) -> Service {
        Service {
            state: AppState {
                jobs: Some(jobs),
                ..self.state
            },
        }
    }

    pub fn with_outputs(self, outputs: Arc<dyn fhir_store::BulkStore>) -> Service {
        Service {
            state: AppState {
                outputs: Some(outputs),
                ..self.state
            },
        }
    }

    pub fn with_entries(self, limit: usize) -> Service {
        Service {
            state: AppState {
                entries: Arc::new(tokio::sync::Semaphore::new(limit.max(1))),
                ..self.state
            },
        }
    }

    pub fn with_templates(self, templates: Arc<dyn Templates>) -> Service {
        Service {
            state: AppState {
                templates,
                ..self.state
            },
        }
    }

    pub async fn started(
        store: Arc<dyn ResourceStore>,
        version: FhirVersion,
        dependencies: Vec<Dependency>,
    ) -> Result<Service, Error> {
        let service = Service::new(store, version, dependencies);
        service.refresh().await?;
        Ok(service)
    }

    pub async fn refresh(&self) -> Result<u64, Error> {
        crate::parameter::refresh(&self.state).await
    }

    pub fn registry(&self) -> Arc<Registry> {
        Arc::clone(&self.state.registry)
    }

    pub fn router(&self) -> Router<()> {
        routes().with_state(self.state.clone())
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

pub(crate) fn over(state: &AppState, store: Arc<dyn ResourceStore>) -> Router<()> {
    routes().with_state(AppState {
        store,
        ..state.clone()
    })
}

fn routes() -> Router<AppState> {
    Router::new()
            .route("/", get(search_system).post(crate::bundle::process))
            .route("/health", get(health))
            .route(
                "/SearchParameter/$status",
                get(parameter_status).post(parameter_status_query).put(parameter_status_update),
            )
            .route("/SearchParameter/$reindex", post(parameter_reindex))
            .route("/SearchParameter/$refresh", post(parameter_refresh))
            .route("/CompartmentDefinition", get(compartment_definitions))
            .route("/CompartmentDefinition/{id}", get(compartment_definition))
            .route("/{type}/{id}/{target}", get(compartment_search))
            .route("/{type}/{id}", get(read).put(update).delete(delete_instance).patch(patch_instance))
            .route("/_history", get(system_history))
            .route("/{type}/_history", get(type_history))
            .route("/{type}/{id}/_history", get(instance_history))
            .route("/{type}/{id}/_history/{vid}", get(vread))
            .route("/{type}", get(search_type).post(create).put(conditional_update).delete(conditional_delete).patch(conditional_patch))
            .route("/{type}/{id}/$purge-history", post(purge_history))
            .route("/$convert-data", post(crate::operation::convert_data))
            .route("/$export", get(crate::job::submit_export).post(crate::job::submit_export))
            .route(
                "/Patient/$export",
                get(crate::job::submit_patient_export).post(crate::job::submit_patient_export),
            )
            .route(
                "/Group/{id}/$export",
                get(crate::job::submit_group_export).post(crate::job::submit_group_export),
            )
            .route("/$import", post(crate::job::submit_import))
            .route("/$bulk-delete", post(crate::job::submit_bulk_delete))
            .route(
                "/{type}/$bulk-delete",
                post(crate::job::submit_type_bulk_delete),
            )
            .route(
                "/$bulk-delete-soft-deleted",
                post(crate::job::submit_bulk_delete_soft_deleted),
            )
            .route(
                "/{type}/$bulk-delete-soft-deleted",
                post(crate::job::submit_type_bulk_delete_soft_deleted),
            )
            .route("/$bulk-update", post(crate::job::submit_bulk_update))
            .route(
                "/{type}/$bulk-update",
                post(crate::job::submit_type_bulk_update),
            )
            .route("/$reindex", post(crate::job::submit_reindex))
            .route(
                "/{type}/{id}/$reindex",
                post(crate::job::submit_resource_reindex),
            )
            .route("/_jobs/{id}", get(crate::job::poll).delete(crate::job::cancel))
            .route("/_jobs/{id}/{*name}", get(crate::job::output))
            .fallback(not_found)
            .method_not_allowed_fallback(method_not_allowed)
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