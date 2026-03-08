mod support;

use fhir_tools::http;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

const KEY: &str = "a shared continuation key for two instances";
const URL: &str = "urn:ops:risk-band";

fn instance(namespace: &str) -> (Child, String) {
    let binary = support::service_binary().expect("the service binary is built");
    let mut child = Command::new(binary)
        .env("FHIR_BACKEND", "relational")
        .env("FHIR_DATABASE_URL", support::url())
        .env(fhir_adapter_relational::ENV_URL, support::url())
        .env(fhir_adapter_relational::ENV_NAMESPACE, namespace)
        .env("FHIR_BIND", "127.0.0.1:0")
        .env("FHIR_VERSION", "R4")
        .env("FHIR_CONTINUATION_KEY", KEY)
        .env_remove("FHIR_METRICS_CREDENTIAL")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the service starts");
    let stdout = child.stdout.take().expect("the service speaks");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("the service announces an address");
    match line.trim().rsplit_once(' ') {
        Some((_, address)) if !address.is_empty() => (child, address.to_owned()),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the service announced no address, said {line:?}");
        }
    }
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn definition() -> String {
    json!({
        "resourceType": "SearchParameter",
        "id": "risk-band",
        "url": URL,
        "name": "riskBand",
        "description": "a band a subject is placed in",
        "status": "active",
        "code": "risk-band",
        "base": ["Patient"],
        "type": "token",
        "expression": "Patient.gender"
    })
    .to_string()
}

fn subject(id: &str) -> String {
    json!({ "resourceType": "Patient", "id": id, "gender": "female" }).to_string()
}

fn call(address: &str, method: &str, path: &str, body: &str) -> (u16, Value) {
    let (status, text) = http::send(address, "localhost", method, path, body)
        .unwrap_or_else(|error| panic!("{method} {path} failed: {error}"));
    let held = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, held)
}

fn indexed_state(answer: &Value) -> Option<(String, i64)> {
    answer["parameter"].as_array()?.iter().find_map(|held| {
        let parts = held["part"].as_array()?;
        let named = |name: &str| parts.iter().find(|part| part["name"] == name);
        let url = named("url")?["valueUri"].as_str()?;
        if url != URL {
            return None;
        }
        Some((
            named("status")?["valueCode"].as_str()?.to_owned(),
            named("indexed")?["valueInteger"].as_i64()?,
        ))
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn two_instances_share_one_parameter_index_and_its_report() {
    let Some(pool) = support::engine().await else {
        return;
    };
    let namespace = support::namespace("shared");
    support::prepared(&pool, &namespace).await;
    let (first, one) = instance(namespace.as_str());
    let (second, two) = instance(namespace.as_str());

    let outcome = std::panic::catch_unwind(|| {
        assert_eq!(call(&one, "POST", "/Patient", &subject("s-one")).0, 201);
        assert_eq!(call(&one, "POST", "/SearchParameter", &definition()).0, 201);
        assert_eq!(call(&one, "POST", "/SearchParameter/$reindex", "").0, 200);

        let (status, answer) = call(&two, "POST", "/SearchParameter/$refresh", "");
        assert_eq!(status, 200);
        assert_eq!(
            indexed_state(&answer),
            Some(("searchable".to_owned(), 1)),
            "the second instance did not read the index of the first"
        );

        assert_eq!(call(&two, "POST", "/Patient", &subject("s-two")).0, 201);
        let (status, answer) = call(&two, "GET", "/Patient?risk-band=female", "");
        assert_eq!(status, 200);
        let found = answer["entry"].as_array().map(Vec::len).unwrap_or_default();
        assert_eq!(
            found, 2,
            "a write on the second instance was not indexed against the shared definition"
        );

        let (status, answer) = call(&one, "GET", "/SearchParameter/$status", "");
        assert_eq!(status, 200);
        assert_eq!(
            indexed_state(&answer).map(|(status, _)| status),
            Some("searchable".to_owned())
        );
    });

    stop(first);
    stop(second);
    support::drop_namespace(&pool, &namespace).await;
    if let Err(reason) = outcome {
        std::panic::resume_unwind(reason);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_started_on_one_instance_is_continued_by_the_other() {
    let Some(pool) = support::engine().await else {
        return;
    };
    let namespace = support::namespace("paged");
    support::prepared(&pool, &namespace).await;
    let (first, one) = instance(namespace.as_str());
    let (second, two) = instance(namespace.as_str());

    let outcome = std::panic::catch_unwind(|| {
        for ordinal in 0..4 {
            assert_eq!(
                call(&one, "POST", "/Patient", &subject(&format!("p-{ordinal}"))).0,
                201
            );
        }
        let (status, answer) = call(&one, "GET", "/Patient?_count=2", "");
        assert_eq!(status, 200);
        let next = answer["link"]
            .as_array()
            .and_then(|links| {
                links
                    .iter()
                    .find(|link| link["relation"] == "next")
                    .and_then(|link| link["url"].as_str())
            })
            .expect("a page names the next page")
            .to_owned();
        let path = next.split_once("/Patient").expect("a next link").1;
        let (status, answer) = call(&two, "GET", &format!("/Patient{path}"), "");
        assert_eq!(status, 200, "the other instance refused a shared token");
        assert_eq!(answer["entry"].as_array().map(Vec::len), Some(2));
    });

    stop(first);
    stop(second);
    support::drop_namespace(&pool, &namespace).await;
    if let Err(reason) = outcome {
        std::panic::resume_unwind(reason);
    }
}
