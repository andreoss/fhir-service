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
use crate::handlers::{
    compartment_definition, compartment_definitions, compartment_search, conditional_delete,
    conditional_patch, conditional_update, create, delete_instance, health, instance_history,
    method_not_allowed, not_found, parameter_refresh, parameter_reindex, parameter_status,
    parameter_status_form, parameter_status_of, parameter_status_query, parameter_status_update,
    patch_instance, purge_history, read, search_system, search_system_form, search_type,
    search_type_form, system_history, type_history, update, vread,
};
use crate::smart::configuration;

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
    pub trail: Option<Arc<crate::trail::StoredTrail>>,
    pub telemetry: Arc<fhir_telemetry::Telemetry>,
    pub scrape: Arc<fhir_telemetry::Scrape>,
    pub authorization: Option<Arc<crate::smart::Authorization>>,
    pub guard: Option<Arc<crate::access::Guard>>,
    pub polling: Arc<crate::polling::Polling>,
    pub versioning: Arc<crate::versioning::Versioning>,
    pub profiles: crate::profile::OnWrite,
    pub roles: Arc<crate::roles::Roles>,
    pub throttle: crate::throttle::Throttle,
    pub capabilities: crate::capabilities::Capabilities,

    pub purge_keeps: Arc<Vec<String>>,
    pub busy: crate::readiness::Busy,
    pub artifacts: Arc<crate::binary::Artifacts>,
    pub allowed_profiles: Arc<crate::profile::AllowedProfiles>,
    pub administration: crate::administration::Administration,
    pub tenancy: crate::tenancy::Tenancy,
    pub policies: crate::policy::Policies,
    pub security_headers: crate::headers::SecurityHeaders,

    pub alarm: Option<Arc<dyn fhir_telemetry::Alarm>>,
    pub traces: Option<Arc<dyn fhir_telemetry::Traces>>,
    pub reset: crate::reset::Resettable,
    pub unchanged: crate::unchanged::Unchanged,

    pub default_format: crate::representation::MediaType,
    pub paging: crate::paging::Paging,
    pub limits: crate::limits::Limits,
    pub forwarding: crate::address::Forwarding,
    pub references: crate::references::References,
    pub restricted: crate::restricted::Restricted,
}

pub type Asked = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;

#[derive(Clone)]
pub struct Dependency {
    pub name: &'static str,
    pub check: Arc<dyn Fn() -> Asked + Send + Sync>,
}

impl Dependency {
    pub fn of_store(name: &'static str, store: Arc<dyn ResourceStore>) -> Dependency {
        Dependency {
            name,
            check: Arc::new(move || {
                let store = Arc::clone(&store);
                Box::pin(async move { store.health().await.map_err(|error| error.to_string()) })
            }),
        }
    }

    pub fn of_queue(name: &'static str, jobs: Arc<dyn fhir_store::JobStore>) -> Dependency {
        Dependency {
            name,
            check: Arc::new(move || {
                let jobs = Arc::clone(&jobs);
                Box::pin(async move { jobs.health().await.map_err(|error| error.to_string()) })
            }),
        }
    }

    pub fn of_outputs(name: &'static str, outputs: Arc<dyn fhir_store::BulkStore>) -> Dependency {
        Dependency {
            name,
            check: Arc::new(move || {
                let outputs = Arc::clone(&outputs);
                Box::pin(async move { outputs.health().await.map_err(|error| error.to_string()) })
            }),
        }
    }
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
        let terminology = Arc::new(crate::terminology::StoredTerminology::new(
            Arc::clone(&store),
            version,
        ));
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
                trail: None,
                scrape: Arc::new(fhir_telemetry::Scrape::closed()),
                telemetry: Arc::new(fhir_telemetry::Telemetry::new(
                    Arc::new(fhir_telemetry::Stream),
                    fhir_store::system_ticker(),
                )),
                authorization: None,
                guard: None,
                polling: Arc::new(crate::polling::Polling::new(fhir_store::system_ticker())),
                versioning: Arc::new(crate::versioning::Versioning::default()),
                profiles: crate::profile::OnWrite::default(),
                roles: Arc::new(crate::roles::Roles::default()),
                throttle: crate::throttle::Throttle::unbounded(),
                capabilities: crate::capabilities::Capabilities::default(),
                purge_keeps: Arc::new(
                    crate::erase::KEPT_ON_PURGE
                        .iter()
                        .map(|name| (*name).to_owned())
                        .collect(),
                ),
                busy: crate::readiness::Busy::new(),
                artifacts: Arc::new(crate::binary::Artifacts::default()),
                allowed_profiles: Arc::new(crate::profile::AllowedProfiles::default()),
                administration: crate::administration::Administration::off(),
                tenancy: crate::tenancy::Tenancy::off(),
                policies: crate::policy::Policies::off(),
                security_headers: crate::headers::SecurityHeaders::default(),
                alarm: None,
                traces: None,
                reset: crate::reset::Resettable::never(),
                unchanged: crate::unchanged::Unchanged::silent(),
                default_format: crate::representation::MediaType::DEFAULT,
                paging: crate::paging::Paging::default(),
                limits: crate::limits::Limits::default(),
                forwarding: crate::address::Forwarding::untrusted(),
                references: crate::references::References::as_written(),
                restricted: crate::restricted::Restricted::everything(),
            },
        }
    }

    pub fn with_versioning(self, versioning: crate::versioning::Versioning) -> Service {
        Service {
            state: AppState {
                versioning: Arc::new(versioning),
                ..self.state
            },
        }
    }

    pub fn with_profile_validation(self, profiles: crate::profile::OnWrite) -> Service {
        Service {
            state: AppState {
                profiles,
                ..self.state
            },
        }
    }

    pub fn with_roles(self, roles: crate::roles::Roles) -> Service {
        Service {
            state: AppState {
                roles: Arc::new(roles),
                ..self.state
            },
        }
    }

    pub fn with_throttle(self, throttle: crate::throttle::Throttle) -> Service {
        Service {
            state: AppState {
                throttle,
                ..self.state
            },
        }
    }

    pub fn keeping_on_purge(self, kept: Vec<String>) -> Service {
        Service {
            state: AppState {
                purge_keeps: Arc::new(kept),
                ..self.state
            },
        }
    }

    pub fn accepting_profiles(self, allowed: crate::profile::AllowedProfiles) -> Service {
        Service {
            state: AppState {
                allowed_profiles: Arc::new(allowed),
                ..self.state
            },
        }
    }

    pub fn paging(self, paging: crate::paging::Paging) -> Service {
        Service {
            state: AppState {
                paging,
                ..self.state
            },
        }
    }

    pub fn serving(self, restricted: crate::restricted::Restricted) -> Service {
        Service {
            state: AppState {
                restricted,
                ..self.state
            },
        }
    }

    pub fn normalising(self, references: crate::references::References) -> Service {
        Service {
            state: AppState {
                references,
                ..self.state
            },
        }
    }

    pub fn behind_proxy(self, forwarding: crate::address::Forwarding) -> Service {
        Service {
            state: AppState {
                forwarding,
                ..self.state
            },
        }
    }

    pub fn bounded_by(self, limits: crate::limits::Limits) -> Service {
        Service {
            state: AppState {
                limits,
                ..self.state
            },
        }
    }

    pub fn answering(self, default_format: crate::representation::MediaType) -> Service {
        Service {
            state: AppState {
                default_format,
                ..self.state
            },
        }
    }

    pub fn skipping_unchanged(self, unchanged: crate::unchanged::Unchanged) -> Service {
        Service {
            state: AppState {
                unchanged,
                ..self.state
            },
        }
    }

    pub fn resettable(self, reset: crate::reset::Resettable) -> Service {
        Service {
            state: AppState {
                reset,
                ..self.state
            },
        }
    }

    pub fn with_security_headers(
        self,
        security_headers: crate::headers::SecurityHeaders,
    ) -> Service {
        Service {
            state: AppState {
                security_headers,
                ..self.state
            },
        }
    }

    pub fn with_policies(self, policies: crate::policy::Policies) -> Service {
        Service {
            state: AppState {
                policies,
                ..self.state
            },
        }
    }

    pub fn with_tenancy(self, tenancy: crate::tenancy::Tenancy) -> Service {
        Service {
            state: AppState {
                tenancy,
                ..self.state
            },
        }
    }

    pub fn with_administration(
        self,
        administration: crate::administration::Administration,
    ) -> Service {
        Service {
            state: AppState {
                administration,
                ..self.state
            },
        }
    }

    pub fn holding_artifacts(self, artifacts: crate::binary::Artifacts) -> Service {
        Service {
            state: AppState {
                artifacts: Arc::new(artifacts),
                ..self.state
            },
        }
    }

    pub fn busy(&self) -> crate::readiness::Busy {
        self.state.busy.clone()
    }

    pub fn with_capabilities(self, capabilities: crate::capabilities::Capabilities) -> Service {
        Service {
            state: AppState {
                capabilities,
                ..self.state
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

    pub fn with_polling(self, polling: crate::polling::Polling) -> Service {
        Service {
            state: AppState {
                polling: Arc::new(polling),
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

    pub fn reporting(self, telemetry: Arc<fhir_telemetry::Telemetry>) -> Service {
        Service {
            state: AppState {
                telemetry,
                ..self.state
            },
        }
    }

    pub fn telemetry(&self) -> Arc<fhir_telemetry::Telemetry> {
        Arc::clone(&self.state.telemetry)
    }

    pub fn alarming(self, alarm: Arc<dyn fhir_telemetry::Alarm>) -> Service {
        self.measuring(Some(alarm), None)
    }

    pub fn tracing(self, traces: Arc<dyn fhir_telemetry::Traces>) -> Service {
        self.measuring(None, Some(traces))
    }

    fn measuring(
        self,
        alarm: Option<Arc<dyn fhir_telemetry::Alarm>>,
        traces: Option<Arc<dyn fhir_telemetry::Traces>>,
    ) -> Service {
        let alarm = alarm.or_else(|| self.state.alarm.clone());
        let traces = traces.or_else(|| self.state.traces.clone());
        let mut telemetry = fhir_telemetry::Telemetry::new(
            Arc::new(fhir_telemetry::Stream),
            fhir_store::system_ticker(),
        );
        if let Some(held) = alarm.clone() {
            telemetry = telemetry.alarming(held);
        }
        if let Some(held) = traces.clone() {
            telemetry = telemetry.tracing(held);
        }
        Service {
            state: AppState {
                telemetry: Arc::new(telemetry),
                alarm,
                traces,
                ..self.state
            },
        }
    }

    pub fn scraped(self, scrape: fhir_telemetry::Scrape) -> Service {
        Service {
            state: AppState {
                scrape: Arc::new(scrape),
                ..self.state
            },
        }
    }

    pub fn recording(self, trail: Arc<crate::trail::StoredTrail>) -> Service {
        Service {
            state: AppState {
                audit: Arc::clone(&trail) as Arc<dyn fhir_store::Audit>,
                trail: Some(trail),
                ..self.state
            },
        }
    }

    pub fn recording_to(self, audit: Arc<dyn fhir_store::Audit>) -> Service {
        Service {
            state: AppState {
                audit,
                trail: None,
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

        crate::profile::register_types(&service.state.store).await?;
        service.refresh().await?;
        Ok(service)
    }

    pub async fn refresh(&self) -> Result<u64, Error> {
        crate::parameter::refresh(&self.state).await
    }

    pub fn registry(&self) -> Arc<Registry> {
        Arc::clone(&self.state.registry)
    }

    pub fn interactions(&self) -> Arc<dyn fhir_store::Interactions> {
        Arc::new(crate::interaction::Searches::new(
            Arc::clone(&self.state.store),
            self.state.version,
            Arc::clone(&self.state.registry),
            Arc::clone(&self.state.terminology),
        ))
    }

    pub fn router(&self) -> Router<()> {
        let held = layered(routes().with_state(self.state.clone()), &self.state);
        match self.state.administration.is_on() {
            true => {
                Router::new()
                    .fallback_service(held)
                    .layer(axum::middleware::from_fn_with_state(
                        self.state.clone(),
                        crate::administration::doors,
                    ))
            }
            false => held,
        }
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
    let held = AppState {
        store,
        ..state.clone()
    };
    layered(routes().with_state(held.clone()), &held)
}

fn layered(router: Router<()>, state: &AppState) -> Router<()> {
    router
        .layer(axum::middleware::from_fn(crate::cors::shared))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::headers::written,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::limits::bounded,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::references::lengthening,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::tenancy::unlabelling,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::throttle::bounded,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::measure::measured,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::representation::negotiated,
        ))
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
        entry("/Binary", &[Verb::Post], post(crate::binary::write)),
        entry(
            "/Binary/{id}",
            &[Verb::Get, Verb::Put],
            get(crate::binary::read).put(crate::binary::write_at),
        ),
        entry("/$liveness", READ, get(crate::readiness::liveness)),
        entry("/$readiness", READ, get(crate::readiness::readiness)),
        entry("/metadata", READ, get(capability)),
        entry("/openapi.json", READ, get(crate::handlers::description)),
        entry("/$reset", WRITE, post(crate::reset::reset)),
        entry("/$fhirUser-lookup", WRITE, post(crate::fhiruser::lookup)),
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
            get(parameter_status)
                .post(parameter_status_query)
                .put(parameter_status_update),
        ),
        entry(
            "/AuditEvent/$verify",
            BOTH,
            get(crate::trail::verified).post(crate::trail::verified),
        ),
        entry(
            "/AuditEvent/$export-trail",
            BOTH,
            get(crate::trail::exported).post(crate::trail::exported),
        ),
        entry("/AuditEvent/$retain", WRITE, post(crate::trail::retained)),
        entry(
            "/SearchParameter/$status/_search",
            &[Verb::Post],
            post(parameter_status_form),
        ),
        entry(
            "/SearchParameter/{id}/$status",
            READ,
            get(parameter_status_of),
        ),
        entry("/SearchParameter/$reindex", WRITE, post(parameter_reindex)),
        entry("/SearchParameter/$refresh", WRITE, post(parameter_refresh)),
        entry("/CompartmentDefinition", READ, get(compartment_definitions)),
        entry("/OperationDefinition", READ, get(operation_definitions)),
        entry(
            "/OperationDefinition/{code}",
            READ,
            get(operation_definition),
        ),
        entry(
            "/CompartmentDefinition/{id}",
            READ,
            get(compartment_definition),
        ),
        entry("/{type}/{id}/{target}", READ, get(compartment_search)),
        entry(
            "/{type}/{id}",
            &[Verb::Get, Verb::Put, Verb::Delete, Verb::Patch],
            get(read)
                .put(update)
                .delete(delete_instance)
                .patch(patch_instance),
        ),
        entry("/_search", &[Verb::Post], post(search_system_form)),
        entry("/{type}/_search", &[Verb::Post], post(search_type_form)),
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
            "/$convert",
            &[Verb::Post],
            post(crate::operation::represented),
        ),
        entry(
            "/StructureDefinition/$snapshot",
            &[Verb::Post],
            post(crate::operation::snapshot),
        ),
        entry(
            "/ValueSet/$validate-code",
            BOTH,
            get(crate::coding::value_set_validate_code)
                .post(crate::coding::value_set_validate_code),
        ),
        entry(
            "/ValueSet/{id}/$validate-code",
            BOTH,
            get(crate::coding::value_set_validate_code_instance)
                .post(crate::coding::value_set_validate_code_instance),
        ),
        entry(
            "/CodeSystem/$validate-code",
            BOTH,
            get(crate::coding::code_system_validate_code)
                .post(crate::coding::code_system_validate_code),
        ),
        entry(
            "/CodeSystem/{id}/$validate-code",
            BOTH,
            get(crate::coding::code_system_validate_code_instance)
                .post(crate::coding::code_system_validate_code_instance),
        ),
        entry(
            "/CodeSystem/$lookup",
            BOTH,
            get(crate::coding::lookup).post(crate::coding::lookup),
        ),
        entry(
            "/CodeSystem/$subsumes",
            BOTH,
            get(crate::coding::subsumes).post(crate::coding::subsumes),
        ),
        entry(
            "/CodeSystem/$find-matches",
            BOTH,
            get(crate::coding::find_matches).post(crate::coding::find_matches),
        ),
        entry(
            "/CodeSystem/$compose",
            BOTH,
            get(crate::coding::find_matches).post(crate::coding::find_matches),
        ),
        entry(
            "/ConceptMap/$translate",
            BOTH,
            get(crate::coding::translate).post(crate::coding::translate),
        ),
        entry(
            "/ConceptMap/{id}/$translate",
            BOTH,
            get(crate::coding::translate_instance).post(crate::coding::translate_instance),
        ),
        entry("/$closure", &[Verb::Post], post(crate::coding::closure)),
        entry(
            "/Observation/$lastn",
            BOTH,
            get(crate::operation::last_n).post(crate::operation::last_n),
        ),
        entry(
            "/{type}/{id}/$erase",
            &[Verb::Post],
            post(crate::erase::erase_instance),
        ),
        entry(
            "/{type}/{id}/_history/{vid}/$erase",
            &[Verb::Post],
            post(crate::erase::erase_version),
        ),
        entry(
            "/Patient/{id}/$purge",
            &[Verb::Post],
            post(crate::erase::purge),
        ),
        entry(
            "/Composition/$document",
            &[Verb::Post],
            post(crate::operation::document_type),
        ),
        entry(
            "/Composition/{id}/$document",
            BOTH,
            get(crate::operation::document).post(crate::operation::document),
        ),
        entry("/$meta", READ, get(crate::meta::read_system)),
        entry("/{type}/$meta", READ, get(crate::meta::read_type)),
        entry(
            "/{type}/{id}/$meta",
            BOTH,
            get(crate::meta::read_instance).post(crate::meta::read_instance),
        ),
        entry("/{type}/{id}/$meta-add", WRITE, post(crate::meta::add)),
        entry(
            "/{type}/{id}/$meta-delete",
            WRITE,
            post(crate::meta::remove),
        ),
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
        entry(
            "/$convert-data",
            WRITE,
            post(crate::operation::convert_data),
        ),
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
        .route(crate::scrape::METRICS, get(crate::scrape::metrics))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
}

pub struct Bound {
    listener: TcpListener,
    router: Router<()>,
}

impl Bound {
    pub async fn holding(addr: SocketAddr, router: Router<()>) -> Result<Bound, Error> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|error| Error::Internal(format!("cannot bind {addr}: {error}")))?;
        Ok(Bound { listener, router })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        self.listener
            .local_addr()
            .map_err(|error| Error::Internal(format!("cannot read the bound address: {error}")))
    }

    pub async fn serve(self) -> Result<(), Error> {
        axum::serve(self.listener, Bound::connected(self.router))
            .await
            .map_err(|error| Error::Internal(format!("server error: {error}")))
    }

    fn connected(
        router: Router<()>,
    ) -> axum::extract::connect_info::IntoMakeServiceWithConnectInfo<Router<()>, SocketAddr> {
        router.into_make_service_with_connect_info::<SocketAddr>()
    }

    pub async fn serve_until<S>(self, stop: S) -> Result<(), Error>
    where
        S: std::future::Future<Output = ()> + Send + 'static,
    {
        axum::serve(self.listener, Bound::connected(self.router))
            .with_graceful_shutdown(stop)
            .await
            .map_err(|error| Error::Internal(format!("server error: {error}")))
    }
}
