mod support;

use fhir_store::ResourceStore;
use fhir_store_contract::fixture::{envelope, id, patient};
use sqlx::{PgPool, Row};

fn wordy(name: &str) -> fhir_core::ResourceEnvelope {
    let note = "the same words repeated over and over again ".repeat(60);
    envelope(
        "Patient",
        name,
        &format!(r#""active":true,"text":{{"div":"{note}"}}"#),
    )
}

async fn stored(pool: &PgPool, namespace: &str, id: &str) -> (i64, String) {
    let statement = format!(
        "select octet_length(body) as size, body_encoding from {namespace}.resource
         where resource_id = $1 and is_current"
    );
    let row = sqlx::query(&statement)
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("the version is stored");
    (
        row.get::<i32, _>("size") as i64,
        row.get::<String, _>("body_encoding"),
    )
}

#[tokio::test]
async fn a_wordy_body_is_held_smaller_than_it_was_written() {
    let Some((store, pool, namespace)) = support::fresh("packed").await else {
        return;
    };
    let written = store.create(wordy("w1")).await.unwrap();
    let (size, encoding) = stored(&pool, namespace.as_str(), "w1").await;
    assert_eq!(encoding, "sealed");
    assert!(size < written.raw().len() as i64 / 4, "{size}");
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn a_body_read_back_is_the_body_that_was_written() {
    let Some((store, pool, namespace)) = support::fresh("lossless").await else {
        return;
    };
    let unicode = envelope(
        "Patient",
        "u1",
        r#""active":true,"name":[{"family":"张","given":["三","Ann-Marie"]}],"text":{"div":"😀 café"}"#,
    );
    let written = store.create(unicode).await.unwrap();
    let read = store
        .read(&fhir_core::ResourceKey::new(
            "Patient".parse().unwrap(),
            id("u1"),
        ))
        .await
        .unwrap();
    assert_eq!(read.raw(), written.raw());
    assert_eq!(read.to_json(), written.to_json());

    let wide = store.create(wordy("u2")).await.unwrap();
    assert_eq!(
        store
            .read(&fhir_core::ResourceKey::new(
                "Patient".parse().unwrap(),
                id("u2")
            ))
            .await
            .unwrap()
            .raw(),
        wide.raw()
    );
    let updated = store.update(wordy("u2"), None).await.unwrap();
    assert_eq!(updated.version_id().as_str(), "1");
    assert_eq!(
        store
            .vread(
                &fhir_core::ResourceKey::new("Patient".parse().unwrap(), id("u2")),
                &fhir_store_contract::fixture::version("1")
            )
            .await
            .unwrap()
            .raw(),
        wide.raw()
    );
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn a_body_is_never_held_larger_than_it_was_written() {
    let Some((store, pool, namespace)) = support::fresh("plain").await else {
        return;
    };
    let written = store
        .create(envelope("Patient", "s1", r#""active":true"#))
        .await
        .unwrap();
    let (size, encoding) = stored(&pool, namespace.as_str(), "s1").await;
    assert!(
        ["plain", "sealed"].contains(&encoding.as_str()),
        "{encoding}"
    );
    assert!(size <= written.raw().len() as i64, "{size}");
    assert_eq!(
        store
            .read(&fhir_core::ResourceKey::new(
                "Patient".parse().unwrap(),
                id("s1")
            ))
            .await
            .unwrap()
            .raw(),
        written.raw()
    );
    support::drop_namespace(&pool, &namespace).await;
}

#[tokio::test]
async fn a_request_needing_metadata_alone_never_unpacks_the_body() {
    let Some((store, pool, namespace)) = support::fresh("lazy").await else {
        return;
    };
    store.create(patient("z1", "Stone", true)).await.unwrap();
    store.create(patient("z2", "Rivers", true)).await.unwrap();
    let damage = format!(
        "update {}.resource set body = decode('ffff', 'hex'), body_encoding = 'packed'
         where is_current",
        namespace.as_str()
    );
    sqlx::query(&damage).execute(&pool).await.unwrap();

    store
        .delete(&fhir_core::ResourceKey::new(
            "Patient".parse().unwrap(),
            id("z1"),
        ))
        .await
        .unwrap();
    assert_eq!(
        store
            .purge_history(&fhir_core::ResourceKey::new(
                "Patient".parse().unwrap(),
                id("z1")
            ))
            .await
            .unwrap(),
        1
    );
    assert!(store.create(patient("z2", "Rivers", true)).await.is_err());
    store
        .hard_delete(&fhir_core::ResourceKey::new(
            "Patient".parse().unwrap(),
            id("z2"),
        ))
        .await
        .unwrap();
    assert!(store
        .read(&fhir_core::ResourceKey::new(
            "Patient".parse().unwrap(),
            id("z2")
        ))
        .await
        .is_err());
    support::drop_namespace(&pool, &namespace).await;
}
