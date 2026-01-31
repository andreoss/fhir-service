use axum::routing::{get, post, MethodRouter};
use axum::Router;
use fhir_core::convert::{ApprovedTemplates, Templates};
use fhir_core::search::Registry;
use fhir_core::{Error, FhirVersion};
use fhir_store::ResourceStore;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

use crate::capability::{capability, version_report};
use crate::definition::{operation_definition, operation_definitions};
use crate::smart::configuration;
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
    pub terminology: Arc<dyn fhir_store::Terminology>,
    pub audit: Arc<dyn fhir_store::Audit>,
    pub authorization: Option<Arc<crate::smart::Authorization>>,
    pub guard: Option<Arc<crate::access::Guard>>,
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
        let terminology = Arc::new(crate::terminology::StoredTerminology::new(Arc::clone(&store)));
        Service {
            state: AppState {
                store,
                terminology,
                jobs: None,
                outputs: None,
                version,
                dependencies: Arc::new(dependencies),
                registry: Arc::new(Registry::for_version(version)),
                parameters: Arc::new(tokio::sync::Mutex::new(())),
                entries: Arc::new(tokio::sync::Semaphore::new(crate::bundle::ENTRIES_AT_ONCE)),
                templates: Arc::new(ApprovedTemplates::default()),
                audit: Arc::new(fhir_store::Unrecorded),
                authorization: None,
                guard: None,
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

    pub fn with_terminology(self, terminology: Arc<dyn fhir_store::Terminology>) -> Service {
        Service {
            state: AppState {
                terminology,
                ..self.state
            },
        }
    }

    pub fn with_authorization(self, authorization: crate::smart::Authorization) -> Service {
        Service {
            state: AppState {
                authorization: Some(Arc::new(authorization)),
                ..self.state
            },
        }
    }

    pub fn recording(self, audit: Arc<dyn fhir_store::Audit>) -> Service {
        Service {
            state: AppState {
                audit,
                ..self.state
            },
        }
    }

    pub fn enforcing(self, keys: Arc<dyn crate::discovery::Keys>) -> Result<Service, Error> {
        let authorization = self
            .state
            .authorization
            .clone()
            .ok_or_else(|| Error::Config("enforcement needs an authorization".to_owned()))?;
        Ok(Service {
            state: AppState {
                guard: Some(Arc::new(crate::access::Guard::new(authorization, keys))),
                ..self.state
            },
        })
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verb {
    Get,
    Post,
    Put,
    Delete,
    Patch,
}

impl Verb {
    pub const ALL: [Verb; 5] = [Verb::Get, Verb::Post, Verb::Put, Verb::Delete, Verb::Patch];

    pub fn as_str(&self) -> &'static str {
        match self {
            Verb::Get => "GET",
            Verb::Post => "POST",
            Verb::Put => "PUT",
            Verb::Delete => "DELETE",
            Verb::Patch => "PATCH",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub path: &'static str,
    pub methods: &'static [Verb],
}

struct Entry {
    route: Route,
    router: MethodRouter<AppState>,
}

fn entry(path: &'static str, methods: &'static [Verb], router: MethodRouter<AppState>) -> Entry {
    Entry {
        route: Route { path, methods },
        router,
    }
}

pub fn served() -> Vec<Route> {
    entries().into_iter().map(|held| held.route).collect()
}

const READ: &[Verb] = &[Verb::Get];
const WRITE: &[Verb] = &[Verb::Post];
const BOTH: &[Verb] = &[Verb::Get, Verb::Post];

fn entries() -> Vec<Entry> {
    vec![
        entry("/", BOTH, get(search_system).post(crate::bundle::process)),
        entry("/health", READ, get(health)),
        entry("/metadata", READ, get(capability)),
        entry("/$versions", BOTH, get(version_report).post(version_report)),
        entry("/.well-known/smart-configuration", READ, get(configuration)),
        entry(
            crate::introspect::INTROSPECT,
            WRITE,
            post(crate::introspect::introspect),
        ),
        entry(
            "/SearchParameter/$status",
            &[Verb::Get, Verb::Post, Verb::Put],
            get(parameter_status).post(parameter_status_query).put(parameter_status_update),
        ),
        entry("/SearchParameter/$reindex", WRITE, post(parameter_reindex)),
        entry("/SearchParameter/$refresh", WRITE, post(parameter_refresh)),
        entry("/CompartmentDefinition", READ, get(compartment_definitions)),
        entry("/OperationDefinition", READ, get(operation_definitions)),
        entry("/OperationDefinition/{code}", READ, get(operation_definition)),
        entry("/CompartmentDefinition/{id}", READ, get(compartment_definition)),
        entry("/{type}/{id}/{target}", READ, get(compartment_search)),
        entry(
            "/{type}/{id}",
            &[Verb::Get, Verb::Put, Verb::Delete, Verb::Patch],
            get(read).put(update).delete(delete_instance).patch(patch_instance),
        ),
        entry("/_history", READ, get(system_history)),
        entry("/{type}/_history", READ, get(type_history)),
        entry("/{type}/{id}/_history", READ, get(instance_history)),
        entry("/{type}/{id}/_history/{vid}", READ, get(vread)),
        entry(
            "/{type}",
            &Verb::ALL,
            get(search_type)
                .post(create)
                .put(conditional_update)
                .delete(conditional_delete)
                .patch(conditional_patch),
        ),
        entry("/{type}/{id}/$purge-history", WRITE, post(purge_history)),
        entry(
            "/Patient/{id}/$everything",
            BOTH,
            get(crate::operation::everything).post(crate::operation::everything),
        ),
        entry(
            "/Patient/$member-match",
            WRITE,
            post(crate::operation::member_match),
        ),
        entry(
            "/$includes",
            BOTH,
            get(crate::operation::includes_system).post(crate::operation::includes_system),
        ),
        entry(
            "/{type}/$includes",
            BOTH,
            get(crate::operation::includes_type).post(crate::operation::includes_type),
        ),
        entry(
            "/DocumentReference/$docref",
            BOTH,
            get(crate::operation::docref_query).post(crate::operation::docref_body),
        ),
        entry(
            "/ValueSet/$expand",
            BOTH,
            get(crate::operation::expand_query).post(crate::operation::expand_body),
        ),
        entry("/$convert-data", WRITE, post(crate::operation::convert_data)),
        entry(
            "/{type}/$validate",
            BOTH,
            get(crate::operation::validate_type).post(crate::operation::validate_type),
        ),
        entry(
            "/{type}/{id}/$validate",
            BOTH,
            get(crate::operation::validate_instance).post(crate::operation::validate_instance),
        ),
        entry(
            "/$export",
            BOTH,
            get(crate::job::submit_export).post(crate::job::submit_export),
        ),
        entry(
            "/Patient/$export",
            BOTH,
            get(crate::job::submit_patient_export).post(crate::job::submit_patient_export),
        ),
        entry(
            "/Group/{id}/$export",
            BOTH,
            get(crate::job::submit_group_export).post(crate::job::submit_group_export),
        ),
        entry("/$import", WRITE, post(crate::job::submit_import)),
        entry("/$bulk-delete", WRITE, post(crate::job::submit_bulk_delete)),
        entry(
            "/{type}/$bulk-delete",
            WRITE,
            post(crate::job::submit_type_bulk_delete),
        ),
        entry(
            "/$bulk-delete-soft-deleted",
            WRITE,
            post(crate::job::submit_bulk_delete_soft_deleted),
        ),
        entry(
            "/{type}/$bulk-delete-soft-deleted",
            WRITE,
            post(crate::job::submit_type_bulk_delete_soft_deleted),
        ),
        entry("/$bulk-update", WRITE, post(crate::job::submit_bulk_update)),
        entry(
            "/{type}/$bulk-update",
            WRITE,
            post(crate::job::submit_type_bulk_update),
        ),
        entry("/$reindex", WRITE, post(crate::job::submit_reindex)),
        entry(
            "/{type}/{id}/$reindex",
            WRITE,
            post(crate::job::submit_resource_reindex),
        ),
        entry(
            "/_jobs/{id}",
            &[Verb::Get, Verb::Delete],
            get(crate::job::poll).delete(crate::job::cancel),
        ),
        entry("/_jobs/{id}/{*name}", READ, get(crate::job::output)),
    ]
}

fn routes() -> Router<AppState> {
    entries()
        .into_iter()
        .fold(Router::new(), |router, held| {
            router.route(held.route.path, held.router)
        })
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