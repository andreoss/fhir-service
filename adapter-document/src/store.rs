use crate::fault::{classified, retried, Policy};
use crate::record::{document_of, envelope_of, next_version, version_number, Record};
use async_trait::async_trait;
use fhir_core::search::{for_type, ParamDef, ParameterSpec};
use fhir_core::{Error, ResourceEnvelope, ResourceId, VersionId};
use fhir_store::index::{rows_of, Rows};
use fhir_store::{
    system_clock, Clock, HistoryOrder, HistoryPage, HistoryQuery, HistoryScope, IndexReport,
    Namespace, PlanCache, PlanStat, ResourceStore, SearchPage, SearchQuery, StoreScope,
};
use mongodb::bson::{doc, Document};
use mongodb::options::{IndexOptions, ReturnDocument};
use mongodb::{Client, ClientSession, Collection, IndexModel};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::{Mutex, OwnedMutexGuard};

pub const RESOURCES: &str = "resource";

pub const COUNTERS: &str = "counter";

pub const STATES: &str = "parameter_index";

const SEQUENCE: &str = "sequence";

const SELECTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const PROBE: std::time::Duration = std::time::Duration::from_secs(2);

pub fn faulted(context: &str, error: mongodb::error::Error) -> Error {
    classified(context, error)
}

fn body_of(envelope: &ResourceEnvelope) -> Result<Value, Error> {
    serde_json::from_slice(envelope.raw()).map_err(|error| Error::InvalidJson(error.to_string()))
}

#[derive(Clone)]
enum Source {
    Client,
    Held(Held),
}

type Held = Arc<Mutex<Option<ClientSession>>>;

pub struct Work {
    own: Option<ClientSession>,
    held: Option<OwnedMutexGuard<Option<ClientSession>>>,
    opened: bool,
}

impl Work {
    pub(crate) fn session(&mut self) -> Result<&mut ClientSession, Error> {
        if let Some(session) = self.own.as_mut() {
            return Ok(session);
        }
        match self.held.as_mut().and_then(|guard| guard.as_mut()) {
            Some(session) => Ok(session),
            None => Err(Error::Internal("the scope is closed".to_owned())),
        }
    }

    pub(crate) async fn done(mut self) -> Result<(), Error> {
        if !self.opened {
            return Ok(());
        }
        let Some(session) = self.own.as_mut() else {
            return Ok(());
        };
        session
            .commit_transaction()
            .await
            .map_err(|error| faulted("committing a write", error))
    }
}

pub struct DocumentStore {
    client: Client,
    namespace: Namespace,
    clock: Clock,
    plans: Arc<PlanCache>,
    custom: Arc<RwLock<HashMap<String, (ParameterSpec, IndexReport)>>>,
    policy: Policy,
    source: Source,
}

impl DocumentStore {
    pub fn new(client: Client, namespace: Namespace) -> DocumentStore {
        DocumentStore {
            client,
            namespace,
            clock: system_clock(),
            plans: Arc::new(PlanCache::new()),
            custom: Arc::new(RwLock::new(HashMap::new())),
            policy: Policy::default(),
            source: Source::Client,
        }
    }

    pub async fn connect(url: &str, namespace: Namespace) -> Result<DocumentStore, Error> {
        let mut options = mongodb::options::ClientOptions::parse(url)
            .await
            .map_err(|error| Error::Config(format!("the store is not reachable: {error}")))?;
        options.server_selection_timeout = Some(SELECTION_TIMEOUT);
        let client = Client::with_options(options)
            .map_err(|error| Error::Config(format!("the store is not reachable: {error}")))?;
        Ok(DocumentStore::new(client, namespace))
    }

    pub fn with_clock(self, clock: Clock) -> DocumentStore {
        DocumentStore { clock, ..self }
    }

    pub fn with_policy(self, policy: Policy) -> DocumentStore {
        DocumentStore { policy, ..self }
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    pub fn database(&self) -> mongodb::Database {
        self.client.database(self.namespace.as_str())
    }

    async fn ping(&self) -> Result<(), Error> {
        let asked = tokio::time::timeout(PROBE, async {
            self.database()
                .run_command(mongodb::bson::doc! { "ping": 1 })
                .await
        })
        .await;
        match asked {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(Error::Internal(format!(
                "the store engine did not answer: {error}"
            ))),
            Err(_) => Err(Error::Internal(
                "the store engine did not answer in time".to_owned(),
            )),
        }
    }

    pub fn resources(&self) -> Collection<Document> {
        self.database().collection(RESOURCES)
    }

    fn counters(&self) -> Collection<Document> {
        self.database().collection(COUNTERS)
    }

    pub async fn initialise(&self) -> Result<usize, Error> {
        let database = self.client.database(self.namespace.as_str());
        for name in [RESOURCES, COUNTERS, STATES, crate::change::CHANGES] {
            match database.create_collection(name).await {
                Ok(()) => {}
                Err(error) if crate::fault::classify(&error) == crate::fault::Fault::Permanent => {}
                Err(error) => return Err(faulted("preparing a collection", error)),
            }
        }
        let unique = IndexOptions::builder().unique(true).build();
        let current = IndexOptions::builder()
            .unique(true)
            .partial_filter_expression(doc! {"is_current": true})
            .build();
        let models = vec![
            IndexModel::builder()
                .keys(doc! {"resource_id": 1, "version_number": 1})
                .options(unique.clone())
                .build(),
            IndexModel::builder()
                .keys(doc! {"resource_id": 1})
                .options(current)
                .build(),
            IndexModel::builder()
                .keys(doc! {SEQUENCE: 1})
                .options(unique)
                .build(),
            IndexModel::builder()
                .keys(doc! {"resource_type": 1, "is_current": 1})
                .build(),
            IndexModel::builder()
                .keys(doc! {"partition": 1, SEQUENCE: 1})
                .build(),
            IndexModel::builder()
                .keys(doc! {"token.param": 1, "token.code": 1})
                .build(),
            IndexModel::builder()
                .keys(doc! {"text.param": 1, "text.folded": 1})
                .build(),
            IndexModel::builder()
                .keys(doc! {"reference.param": 1, "reference.pointer": 1})
                .build(),
            IndexModel::builder()
                .keys(doc! {"updated_key": 1})
                .build(),
        ];
        let made = models.len();
        self.resources()
            .create_indexes(models)
            .await
            .map_err(|error| faulted("preparing an index", error))?;
        let recorded = vec![
            IndexModel::builder()
                .keys(doc! {SEQUENCE: 1})
                .options(IndexOptions::builder().unique(true).build())
                .build(),
            IndexModel::builder()
                .keys(doc! {"partition": 1, SEQUENCE: 1})
                .build(),
        ];
        let kept = recorded.len();
        self.database()
            .collection::<Document>(crate::change::CHANGES)
            .create_indexes(recorded)
            .await
            .map_err(|error| faulted("preparing an index", error))?;
        Ok(made + kept)
    }

    pub fn plans(&self) -> Vec<PlanStat> {
        self.plans.stats()
    }

    pub(crate) fn cache(&self) -> &PlanCache {
        &self.plans
    }

    pub(crate) fn defs(&self, envelope: &ResourceEnvelope) -> Vec<Arc<ParamDef>> {
        let mut defs = for_type(envelope.resource_type());
        if let Ok(custom) = self.custom.read() {
            for (spec, _) in custom.values() {
                if spec.base.contains(&envelope.resource_type()) {
                    defs.push(Arc::clone(&spec.def));
                }
            }
        }
        defs
    }

    pub(crate) fn remember(&self, spec: ParameterSpec, report: IndexReport) {
        if let Ok(mut custom) = self.custom.write() {
            custom.insert(spec.url.clone(), (spec, report));
        }
    }

    pub(crate) fn forget(&self, url: &str) {
        if let Ok(mut custom) = self.custom.write() {
            custom.remove(url);
        }
    }

    fn states(&self) -> Collection<Document> {
        self.database().collection(STATES)
    }

    pub(crate) async fn record_index(&self, report: &IndexReport) -> Result<(), Error> {
        let failures: Vec<Document> = report
            .failures
            .iter()
            .map(|failure| {
                mongodb::bson::doc! {
                    "resource": failure.resource.clone(),
                    "reason": failure.reason.clone(),
                }
            })
            .collect();
        let held = mongodb::bson::doc! {
            "$set": {
                "backfilled": report.backfilled,
                "indexed": report.indexed as i64,
                "values": report.values as i64,
                "overflow": report.overflow as i64,
                "failures": failures,
            }
        };
        self.states()
            .update_one(mongodb::bson::doc! { "_id": report.url.clone() }, held)
            .upsert(true)
            .await
            .map_err(|error| faulted("recording an index state", error))?;
        Ok(())
    }

    pub(crate) async fn recorded(&self, url: &str) -> Result<Option<IndexReport>, Error> {
        let found = self
            .states()
            .find_one(mongodb::bson::doc! { "_id": url })
            .await
            .map_err(|error| faulted("reading an index state", error))?;
        let Some(held) = found else {
            return Ok(None);
        };
        let count = |name: &str| held.get_i64(name).unwrap_or_default().max(0) as usize;
        let failures = held
            .get_array("failures")
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let held = item.as_document()?;
                        Some(fhir_store::IndexFailure {
                            resource: held.get_str("resource").ok()?.to_owned(),
                            reason: held.get_str("reason").ok()?.to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(IndexReport {
            url: url.to_owned(),
            backfilled: held.get_bool("backfilled").unwrap_or_default(),
            indexed: count("indexed"),
            values: count("values"),
            overflow: count("overflow"),
            failures,
        }))
    }

    pub(crate) async fn forget_record(&self, url: &str) -> Result<(), Error> {
        self.states()
            .delete_one(mongodb::bson::doc! { "_id": url })
            .await
            .map_err(|error| faulted("dropping an index state", error))?;
        Ok(())
    }

    fn held(&self) -> bool {
        matches!(self.source, Source::Held(_))
    }

    fn sharing(&self, source: Source) -> DocumentStore {
        DocumentStore {
            client: self.client.clone(),
            namespace: self.namespace.clone(),
            clock: Arc::clone(&self.clock),
            plans: Arc::clone(&self.plans),
            custom: Arc::clone(&self.custom),
            policy: self.policy,
            source,
        }
    }

    pub(crate) async fn reading(&self) -> Result<Work, Error> {
        match &self.source {
            Source::Held(held) => Ok(Work {
                own: None,
                held: Some(Arc::clone(held).lock_owned().await),
                opened: false,
            }),
            Source::Client => Ok(Work {
                own: Some(
                    self.client
                        .start_session()
                        .await
                        .map_err(|error| faulted("opening a session", error))?,
                ),
                held: None,
                opened: false,
            }),
        }
    }

    pub(crate) async fn writing(&self) -> Result<Work, Error> {
        match &self.source {
            Source::Held(held) => Ok(Work {
                own: None,
                held: Some(Arc::clone(held).lock_owned().await),
                opened: false,
            }),
            Source::Client => {
                let policy = self.policy;
                let mut session =
                    retried(&policy, "opening a session", || async {
                        self.client.start_session().await
                    })
                    .await?;
                session
                    .start_transaction()
                    .await
                    .map_err(|error| faulted("starting a write", error))?;
                Ok(Work {
                    own: Some(session),
                    held: None,
                    opened: true,
                })
            }
        }
    }

    pub(crate) async fn listed(
        &self,
        pipeline: Vec<Document>,
        context: &str,
    ) -> Result<Vec<Document>, Error> {
        self.drawn(RESOURCES, pipeline, context).await
    }

    pub(crate) async fn drawn(
        &self,
        collection: &str,
        pipeline: Vec<Document>,
        context: &str,
    ) -> Result<Vec<Document>, Error> {
        let mut work = self.reading().await?;
        let session = work.session()?;
        let mut cursor = self
            .database()
            .collection::<Document>(collection)
            .aggregate(pipeline)
            .session(&mut *session)
            .await
            .map_err(|error| faulted(context, error))?;
        let mut found = Vec::new();
        while cursor
            .advance(&mut *session)
            .await
            .map_err(|error| faulted(context, error))?
        {
            found.push(
                cursor
                    .deserialize_current()
                    .map_err(|error| faulted(context, error))?,
            );
        }
        Ok(found)
    }

    pub(crate) async fn perhaps(
        &self,
        filter: Document,
        context: &str,
    ) -> Result<Option<Document>, Error> {
        let mut work = self.reading().await?;
        let session = work.session()?;
        self.resources()
            .find_one(filter)
            .session(&mut *session)
            .await
            .map_err(|error| faulted(context, error))
    }

    pub(crate) async fn counted(&self, filter: Document, context: &str) -> Result<u64, Error> {
        let mut work = self.reading().await?;
        let session = work.session()?;
        self.resources()
            .count_documents(filter)
            .session(&mut *session)
            .await
            .map_err(|error| faulted(context, error))
    }

    async fn current_in(
        &self,
        session: &mut ClientSession,
        id: &ResourceId,
    ) -> Result<Option<Record>, Error> {
        let found = self
            .resources()
            .find_one(doc! {"resource_id": id.as_str(), "is_current": true})
            .session(&mut *session)
            .await
            .map_err(|error| faulted("reading the current version", error))?;
        found.as_ref().map(Record::of).transpose()
    }

    async fn next_sequence(&self, session: &mut ClientSession) -> Result<i64, Error> {
        let found = self
            .counters()
            .find_one_and_update(doc! {"_id": SEQUENCE}, doc! {"$inc": {"value": 1_i64}})
            .upsert(true)
            .return_document(ReturnDocument::After)
            .session(&mut *session)
            .await
            .map_err(|error| faulted("drawing the next write", error))?;
        match found.as_ref().and_then(|held| held.get_i64("value").ok()) {
            Some(value) => Ok(value),
            None => Err(Error::Internal(
                "the order of writes could not be drawn".to_owned(),
            )),
        }
    }

    pub(crate) async fn append(
        &self,
        session: &mut ClientSession,
        previous: Option<&Record>,
        envelope: &ResourceEnvelope,
    ) -> Result<i64, Error> {
        if let Some(previous) = previous {
            self.resources()
                .update_one(
                    doc! {
                        "resource_id": previous.id.as_str(),
                        "version_number": version_number(&previous.version)?,
                    },
                    doc! {"$set": {"is_current": false}},
                )
                .session(&mut *session)
                .await
                .map_err(|error| faulted("superseding a version", error))?;
        }
        let sequence = self.next_sequence(session).await?;
        let rows = match envelope.is_deleted() {
            true => Rows::default(),
            false => {
                let body = body_of(envelope)?;
                rows_of(envelope, &body, &self.defs(envelope))
            }
        };
        let held = document_of(envelope, &rows, sequence, true);
        self.resources()
            .insert_one(held)
            .session(&mut *session)
            .await
            .map_err(|error| faulted("writing a version", error))?;
        self.record(session, envelope, sequence).await?;
        Ok(sequence)
    }

    async fn scoped(
        &self,
        scope: &HistoryScope,
        query: &HistoryQuery,
    ) -> Result<(Vec<Document>, u64), Error> {
        let mut filter = Document::new();
        match scope {
            HistoryScope::System => {}
            HistoryScope::Type(kind) => {
                filter.insert("resource_type", kind.as_str());
            }
            HistoryScope::Instance(_, id) => {
                filter.insert("resource_id", id.as_str());
            }
        }
        let mut bounds = Document::new();
        if let Some(period) = query.since {
            bounds.insert("$gte", crate::record::low_key(&period));
        }
        if let Some(period) = query.before {
            bounds.insert("$lt", crate::record::low_key(&period));
        }
        if let Some(period) = query.at {
            bounds.insert("$gte", crate::record::low_key(&period));
            bounds.insert("$lte", crate::record::high_key(&period));
        }
        if !bounds.is_empty() {
            filter.insert("updated_key", bounds);
        }
        let total = self.counted(filter.clone(), "counting history").await?;
        let direction = match query.order {
            HistoryOrder::Newest => -1,
            HistoryOrder::Oldest => 1,
        };
        let mut pipeline = vec![
            doc! {"$match": filter},
            doc! {"$sort": {
                "updated_key": direction,
                "resource_id": direction,
                "version_number": direction,
            }},
        ];
        if query.offset > 0 {
            pipeline.push(doc! {"$skip": query.offset.min(i64::MAX as usize) as i64});
        }
        if query.count < usize::MAX {
            pipeline.push(doc! {"$limit": query.count.min(i64::MAX as usize) as i64});
        }
        let found = self.listed(pipeline, "reading history").await?;
        Ok((found, total))
    }
}

#[async_trait]
impl ResourceStore for DocumentStore {
    async fn create(&self, envelope: ResourceEnvelope) -> Result<ResourceEnvelope, Error> {
        let mut work = self.writing().await?;
        let session = work.session()?;
        if self.current_in(session, envelope.id()).await?.is_some() {
            return Err(Error::Duplicate(format!(
                "id {:?} already exists",
                envelope.id().as_str()
            )));
        }
        let first: VersionId = "1".parse()?;
        let stored = envelope.stored_with(first, (self.clock)())?;
        let session = work.session()?;
        self.append(session, None, &stored).await?;
        work.done().await?;
        Ok(stored)
    }

    async fn read(&self, id: &ResourceId) -> Result<ResourceEnvelope, Error> {
        let found = self
            .perhaps(
                doc! {"resource_id": id.as_str(), "is_current": true},
                "reading a resource",
            )
            .await?;
        match found {
            Some(held) => envelope_of(&held),
            None => Err(Error::NotFound),
        }
    }

    async fn vread(&self, id: &ResourceId, version: &VersionId) -> Result<ResourceEnvelope, Error> {
        let number = version_number(version)?;
        let found = self
            .perhaps(
                doc! {"resource_id": id.as_str(), "version_number": number},
                "reading a version",
            )
            .await?;
        match found {
            Some(held) => envelope_of(&held),
            None => Err(Error::NotFound),
        }
    }

    async fn update(
        &self,
        envelope: ResourceEnvelope,
        expected_version: Option<&VersionId>,
    ) -> Result<ResourceEnvelope, Error> {
        let mut work = self.writing().await?;
        let session = work.session()?;
        let Some(current) = self.current_in(session, envelope.id()).await? else {
            return Err(Error::NotFound);
        };
        if let Some(expected) = expected_version {
            if &current.version != expected {
                return Err(Error::VersionConflict);
            }
        }
        if current.resource_type != envelope.resource_type() {
            return Err(Error::InvalidEnvelope(format!(
                "resource type mismatch: expected {:?} found {:?}",
                current.resource_type.as_str(),
                envelope.resource_type().as_str()
            )));
        }
        if !current.deleted {
            let held = current.envelope()?;
            if envelope.content_eq(&held) {
                return Ok(held);
            }
        }
        let stored = envelope.stored_with(next_version(&current.version)?, (self.clock)())?;
        let session = work.session()?;
        self.append(session, Some(&current), &stored).await?;
        work.done().await?;
        Ok(stored)
    }

    async fn search(&self, query: &SearchQuery) -> Result<SearchPage, Error> {
        crate::query::run(self, query).await
    }

    async fn delete(&self, id: &ResourceId) -> Result<ResourceEnvelope, Error> {
        let mut work = self.writing().await?;
        let session = work.session()?;
        let current = self.current_in(session, id).await?.ok_or(Error::NotFound)?;
        if current.deleted {
            return Err(Error::Deleted);
        }
        let marker = ResourceEnvelope::deleted_marker(
            current.spec,
            current.resource_type,
            id.clone(),
            next_version(&current.version)?,
            (self.clock)(),
        );
        let session = work.session()?;
        self.append(session, Some(&current), &marker).await?;
        work.done().await?;
        Ok(marker)
    }

    async fn hard_delete(&self, id: &ResourceId) -> Result<(), Error> {
        let mut work = self.writing().await?;
        let session = work.session()?;
        let removed = self
            .resources()
            .delete_many(doc! {"resource_id": id.as_str()})
            .session(&mut *session)
            .await
            .map_err(|error| faulted("removing a resource", error))?;
        work.done().await?;
        match removed.deleted_count {
            0 => Err(Error::NotFound),
            _ => Ok(()),
        }
    }

    async fn purge_history(&self, id: &ResourceId) -> Result<usize, Error> {
        let mut work = self.writing().await?;
        let session = work.session()?;
        if self.current_in(session, id).await?.is_none() {
            return Err(Error::NotFound);
        }
        let session = work.session()?;
        let removed = self
            .resources()
            .delete_many(doc! {"resource_id": id.as_str(), "is_current": false})
            .session(&mut *session)
            .await
            .map_err(|error| faulted("purging history", error))?;
        work.done().await?;
        Ok(removed.deleted_count as usize)
    }

    async fn history(
        &self,
        scope: &HistoryScope,
        query: &HistoryQuery,
    ) -> Result<HistoryPage, Error> {
        if let HistoryScope::Instance(resource_type, id) = scope {
            let found = self
                .perhaps(doc! {"resource_id": id.as_str()}, "reading a resource")
                .await?
                .ok_or(Error::NotFound)?;
            if found.get_str("resource_type") != Ok(resource_type.as_str()) {
                return Err(Error::NotFound);
            }
        }
        let (found, total) = self.scoped(scope, query).await?;
        let entries = found
            .iter()
            .map(envelope_of)
            .collect::<Result<Vec<ResourceEnvelope>, Error>>()?;
        Ok(HistoryPage {
            entries,
            total: total as usize,
            offset: query.offset,
        })
    }

    async fn index_parameter(&self, spec: &ParameterSpec) -> Result<IndexReport, Error> {
        let report = IndexReport::empty(&spec.url);
        self.record_index(&report).await?;
        self.remember(spec.clone(), report.clone());
        Ok(report)
    }

    async fn drop_parameter(&self, url: &str) -> Result<(), Error> {
        crate::query::drop_index(self, url).await?;
        self.forget_record(url).await?;
        self.forget(url);
        Ok(())
    }

    async fn adopt_parameter(&self, spec: &ParameterSpec) -> Result<(), Error> {
        let report = self
            .recorded(&spec.url)
            .await?
            .unwrap_or_else(|| IndexReport::empty(&spec.url));
        self.remember(spec.clone(), report);
        Ok(())
    }

    async fn reindex(&self, specs: &[ParameterSpec]) -> Result<Vec<IndexReport>, Error> {
        crate::query::reindex(self, specs).await
    }

    async fn reindex_resource(
        &self,
        specs: &[ParameterSpec],
        id: &ResourceId,
    ) -> Result<Vec<IndexReport>, Error> {
        crate::query::reindex_resource(self, specs, id).await
    }

    async fn index_report(&self, url: &str) -> Result<Option<IndexReport>, Error> {
        self.recorded(url).await
    }

    async fn begin(&self) -> Result<Arc<dyn StoreScope>, Error> {
        if self.held() {
            return Err(Error::Internal("a scope is already open".to_owned()));
        }
        let mut session = self
            .client
            .start_session()
            .await
            .map_err(|error| faulted("starting a scope", error))?;
        session
            .start_transaction()
            .await
            .map_err(|error| faulted("starting a scope", error))?;
        let held: Held = Arc::new(Mutex::new(Some(session)));
        Ok(Arc::new(DocumentScope {
            store: Arc::new(self.sharing(Source::Held(Arc::clone(&held)))),
            held,
        }))
    }

    async fn health(&self) -> Result<(), Error> {
        self.ping().await
    }
}

pub struct DocumentScope {
    store: Arc<DocumentStore>,
    held: Held,
}

impl DocumentScope {
    async fn settle(&self, keep: bool) -> Result<(), Error> {
        let Some(mut session) = self.held.lock().await.take() else {
            return Ok(());
        };
        match keep {
            true => session
                .commit_transaction()
                .await
                .map_err(|error| faulted("committing a scope", error)),
            false => session
                .abort_transaction()
                .await
                .map_err(|error| faulted("rolling a scope back", error)),
        }
    }
}

#[async_trait]
impl StoreScope for DocumentScope {
    fn store(&self) -> Arc<dyn ResourceStore> {
        Arc::clone(&self.store) as Arc<dyn ResourceStore>
    }

    async fn commit(&self) -> Result<(), Error> {
        self.settle(true).await
    }

    async fn rollback(&self) -> Result<(), Error> {
        self.settle(false).await
    }
}
