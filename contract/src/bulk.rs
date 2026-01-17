use fhir_core::Error;
use fhir_store::{BulkStore, JobId, Output};

fn job(raw: &str) -> JobId {
    JobId::parse(raw).expect("suite job id is valid")
}

pub async fn outputs(store: &dyn BulkStore) {
    let one = job("o1");
    assert!(store.list(&one).await.unwrap().is_empty());

    store
        .write(&one, &Output::new("Patient-1.ndjson", "Patient", 2), b"a\nb\n")
        .await
        .unwrap();
    store
        .write(&one, &Output::new("Observation-1.ndjson", "Observation", 1), b"c\n")
        .await
        .unwrap();

    let listed = store.list(&one).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].name, "Observation-1.ndjson");
    assert_eq!(listed[0].kind, "Observation");
    assert_eq!(listed[0].count, 1);
    assert_eq!(listed[0].size, 2);
    assert_eq!(listed[1].name, "Patient-1.ndjson");
    assert_eq!(listed[1].count, 2);

    assert_eq!(store.read(&one, "Patient-1.ndjson").await.unwrap(), b"a\nb\n");

    store
        .write(&one, &Output::new("Patient-1.ndjson", "Patient", 1), b"a\n")
        .await
        .unwrap();
    let rewritten = store.list(&one).await.unwrap();
    assert_eq!(rewritten.len(), 2);
    assert_eq!(store.read(&one, "Patient-1.ndjson").await.unwrap(), b"a\n");

    let other = job("o2");
    store
        .write(&other, &Output::new("Patient-1.ndjson", "Patient", 1), b"z\n")
        .await
        .unwrap();
    assert_eq!(store.list(&other).await.unwrap().len(), 1);
    assert_eq!(store.read(&other, "Patient-1.ndjson").await.unwrap(), b"z\n");

    let missing = store.read(&one, "nowhere.ndjson").await;
    assert!(matches!(missing, Err(Error::NotFound)), "{missing:?}");
    let unknown = store.read(&job("o3"), "Patient-1.ndjson").await;
    assert!(matches!(unknown, Err(Error::NotFound)), "{unknown:?}");

    assert_eq!(store.purge(&one).await.unwrap(), 2);
    assert!(store.list(&one).await.unwrap().is_empty());
    assert_eq!(store.list(&other).await.unwrap().len(), 1);
    assert_eq!(store.purge(&one).await.unwrap(), 0);
    store.health().expect("a fresh sink is healthy");
}
