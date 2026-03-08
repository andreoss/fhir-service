use fhir_adapter_memory::MemoryStore;
use fhir_api::{Dependency, Service};
use fhir_core::{FhirInstant, FhirVersion};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

fn service() -> Service {
    let store = MemoryStore::with_clock(Arc::new(|| {
        FhirInstant::parse("2026-09-07T04:00:00.000Z").unwrap()
    }));
    let dependencies = vec![Dependency {
        name: "memory-store",
        check: Arc::new(|| Box::pin(async { Ok(()) })),
    }];
    Service::new(Arc::new(store), FhirVersion::R4, dependencies)
}

fn answered(address: std::net::SocketAddr, path: &str) -> Option<String> {
    let mut socket = TcpStream::connect(address).ok()?;
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let request = format!("GET {path} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n");
    socket.write_all(request.as_bytes()).ok()?;
    socket.flush().ok()?;
    let mut raw = String::new();
    socket.read_to_string(&mut raw).ok()?;
    Some(raw)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_in_flight_is_answered_after_the_stop_is_asked_for() {
    let bound = service()
        .bind("127.0.0.1:0".parse().unwrap())
        .await
        .expect("a bound listener");
    let address = bound.local_addr().expect("an announced address");
    let (stop, asked) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(async move {
        bound
            .serve_until(async {
                let _ = asked.await;
            })
            .await
    });

    let first = tokio::task::spawn_blocking(move || answered(address, "/health"))
        .await
        .expect("the first request ran");
    assert!(
        first.as_deref().unwrap_or_default().contains(" 200 "),
        "{first:?}"
    );

    stop.send(()).expect("the stop is asked for");
    serving
        .await
        .expect("the server task ended")
        .expect("served");

    let after = tokio::task::spawn_blocking(move || answered(address, "/health"))
        .await
        .expect("the second attempt ran");
    let refused = after.is_none() || after.as_deref().unwrap_or_default().is_empty();
    assert!(refused, "a stopped instance accepts nothing: {after:?}");
}
