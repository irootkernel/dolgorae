#![cfg(target_os = "macos")]

#[path = "support/gateway_native.rs"]
mod support;

use dolgorae::protocol::public_v1 as pb;
use prost::Message as _;
use serde_json::Value;
use std::time::Duration;
use support::{Fixture, context};

async fn get_run(
    client: &mut pb::run_service_client::RunServiceClient<tonic::transport::Channel>,
    fixture: &Fixture,
    id: &str,
) -> pb::RunProjection {
    client
        .get_run(pb::GetRunRequest {
            context: context(),
            run: fixture.run_ref(id),
        })
        .await
        .unwrap()
        .into_inner()
        .run
        .unwrap()
}

fn submit(fixture: &Fixture, id: &str) {
    fixture.cli(&[
        "run",
        "--controller-file",
        fixture.controller.to_str().unwrap(),
        "submit",
        id,
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--message",
        "Execute the isolated gateway runtime scenario.",
        "--idempotency-key",
        "gateway-native-turn",
    ]);
}

fn watch(fixture: &Fixture, id: &str) -> pb::WatchRunEventsRequest {
    pb::WatchRunEventsRequest {
        context: context(),
        run: fixture.run_ref(id),
        after_cursor: "0".into(),
        projection: pb::ProjectionProfile::Operational as i32,
        projection_version: 1,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_run_restart_e2e() {
    let fixture = Fixture::new("gateway_active");
    let run_id = fixture.start_run(&[]);
    submit(&fixture, &run_id);
    let worker_path = fixture
        .state_root
        .join("runtime/runs")
        .join(format!("{run_id}.json"));
    let original_worker: Value =
        serde_json::from_slice(&std::fs::read(&worker_path).unwrap()).unwrap();
    let mut gateway = fixture.start_gateway();
    let original_instance = gateway.ready["data"]["server_instance_id"].clone();
    let channel = gateway.channel().await;
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let before = get_run(&mut runs, &fixture, &run_id).await;
    assert!(
        before.active_turn.is_some(),
        "restart scenario requires a genuinely active Turn"
    );
    let mut events = pb::observation_service_client::ObservationServiceClient::new(channel);
    let mut first_stream = events
        .watch_run_events(watch(&fixture, &run_id))
        .await
        .unwrap()
        .into_inner();
    let first = tokio::time::timeout(Duration::from_secs(5), first_stream.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    gateway.kill();
    assert!(
        gateway.socket.exists(),
        "crashed server should leave its owned socket for stale proof"
    );
    let mut replacement = fixture.start_gateway();
    assert_ne!(
        original_instance,
        replacement.ready["data"]["server_instance_id"]
    );
    let channel = replacement.channel().await;
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let after = get_run(&mut runs, &fixture, &run_id).await;
    assert_eq!(before.run_id, after.run_id);
    assert_eq!(before.thread, after.thread);
    assert_eq!(before.active_turn, after.active_turn);
    assert_eq!(before.configuration, after.configuration);
    assert_eq!(before.server_lane, after.server_lane);
    assert_eq!(before.writer_authority, after.writer_authority);
    let current_worker: Value =
        serde_json::from_slice(&std::fs::read(&worker_path).unwrap()).unwrap();
    assert_eq!(
        original_worker["identity"], current_worker["identity"],
        "gateway restart changed worker identity"
    );
    let mut events = pb::observation_service_client::ObservationServiceClient::new(channel);
    let mut resumed = events
        .watch_run_events(watch(&fixture, &run_id))
        .await
        .unwrap()
        .into_inner();
    let replayed = tokio::time::timeout(Duration::from_secs(5), resumed.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        first, replayed,
        "event replay changed identity, cursor, stamp or payload"
    );
    drop(resumed);
    replacement.terminate();
}

#[derive(Clone, PartialEq, prost::Message)]
struct RichStatus {
    #[prost(message, repeated, tag = "3")]
    details: Vec<prost_types::Any>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_run_pressure_e2e() {
    let fixture = Fixture::new("gateway_pressure");
    let crowded = fixture.start_run(&[]);
    let healthy = fixture.start_run(&[]);
    for index in 0..6 {
        let key = format!("pressure-{index}");
        fixture.cli(&[
            "run",
            "--controller-file",
            fixture.controller.to_str().unwrap(),
            "send",
            &crowded,
            "--workspace",
            fixture.workspace.to_str().unwrap(),
            "--message",
            "Generate the bounded fixture response.",
            "--idempotency-key",
            &key,
        ]);
    }
    submit(&fixture, &crowded);
    let mut gateway = fixture.start_gateway();
    let channel = gateway.channel().await;
    let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
    let before = get_run(&mut runs, &fixture, &healthy).await;
    let mut observations =
        pb::observation_service_client::ObservationServiceClient::new(channel.clone());
    let mut slow = observations
        .watch_run_events(watch(&fixture, &crowded))
        .await
        .unwrap()
        .into_inner();
    // A 1-KiB HTTP/2 receive window applies actual network backpressure; this
    // deliberately does not read the crowded Run stream while another Run is read.
    tokio::time::sleep(Duration::from_secs(7)).await;
    let after = tokio::time::timeout(
        Duration::from_secs(3),
        get_run(&mut runs, &fixture, &healthy),
    )
    .await
    .unwrap();
    assert_eq!(before.run_id, after.run_id);
    assert_eq!(before.lifecycle, after.lifecycle);
    let mut other = pb::observation_service_client::ObservationServiceClient::new(channel);
    let mut independent = other
        .watch_run_events(watch(&fixture, &healthy))
        .await
        .unwrap()
        .into_inner();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), independent.message())
            .await
            .unwrap()
            .unwrap()
            .is_some()
    );
    let error = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match slow.message().await {
                Ok(Some(_)) => {}
                Ok(None) => panic!("pressured stream ended without typed error"),
                Err(error) => break error,
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(error.code(), tonic::Code::ResourceExhausted);
    let rich = RichStatus::decode(error.details()).unwrap();
    let detail = pb::DolgoraeErrorDetail::decode(rich.details[0].value.as_slice()).unwrap();
    assert_eq!(detail.dolgorae_error_code, "SLOW_CONSUMER");
    let running = get_run(&mut runs, &fixture, &crowded).await;
    assert!(
        running.active_turn.is_some(),
        "stream pressure changed the Run itself"
    );
    drop(independent);
    gateway.terminate();
}
