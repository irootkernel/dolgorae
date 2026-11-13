//! Bounded foreground gRPC transport. Semantic work remains in GatewayBackend.
use crate::darwin::DarwinSystem;
use crate::gateway_socket::GatewaySocket;
use crate::machine::{FailureEnvelope, MachineError, SuccessEnvelope};
use crate::paths::DolgoraeHome;
use crate::protocol::public_v1 as pb;
use prost::Message as _;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Semaphore, watch};
use tokio_stream::Stream;
use tonic::{Request, Response, Status};
use uuid::Uuid;

const STREAM_COUNT: usize = 32;
const STREAM_BYTES: usize = 4 * 1024 * 1024;
const STREAM_STALL: Duration = Duration::from_secs(5);
const SEMANTIC_WORKERS: usize = 8;
const STREAM_WORKERS: usize = 4;
const RPC_BUDGET: Duration = Duration::from_secs(30);
const DRAIN_BUDGET: Duration = Duration::from_secs(5);
const MAX_CONNECTIONS: usize = 16;

/// A bounded, immediately available projection page. next_cursor may cross
/// filtered gaps only once the backend scanned them at its captured head.
pub struct EventPage {
    pub events: Vec<pb::RunEventEnvelope>,
    pub next_cursor: String,
    pub durable_head_cursor: String,
    pub lifecycle: i32,
    pub terminal: bool,
}

pub trait GatewayBackend: Send + Sync + 'static {
    fn modern(&self) -> Result<&dyn crate::gateway_v2::ModernGatewayBackend, MachineError> {
        Err(unavailable("public-v2"))
    }
    fn get_capabilities(
        &self,
        _request: pb::GetCapabilitiesRequest,
    ) -> Result<pb::GetCapabilitiesResponse, MachineError> {
        Err(unavailable("RuntimeService.GetCapabilities"))
    }
    fn inspect_workspace(
        &self,
        _request: pb::InspectWorkspaceRequest,
    ) -> Result<pb::InspectWorkspaceResponse, MachineError> {
        Err(unavailable("RuntimeService.InspectWorkspace"))
    }
    fn list_profiles(
        &self,
        _request: pb::ListProfilesRequest,
    ) -> Result<pb::ListProfilesResponse, MachineError> {
        Err(unavailable("RuntimeService.ListProfiles"))
    }
    fn get_profile(
        &self,
        _request: pb::GetProfileRequest,
    ) -> Result<pb::GetProfileResponse, MachineError> {
        Err(unavailable("RuntimeService.GetProfile"))
    }
    fn start_run(
        &self,
        _request: pb::StartRunRequest,
    ) -> Result<pb::StartRunResponse, MachineError> {
        Err(unavailable("RunService.StartRun"))
    }
    fn list_runs(
        &self,
        _request: pb::ListRunsRequest,
    ) -> Result<pb::ListRunsResponse, MachineError> {
        Err(unavailable("RunService.ListRuns"))
    }
    fn get_run(&self, _request: pb::GetRunRequest) -> Result<pb::GetRunResponse, MachineError> {
        Err(unavailable("RunService.GetRun"))
    }
    fn submit_turn(
        &self,
        _request: pb::SubmitTurnRequest,
    ) -> Result<pb::SubmitTurnAccepted, MachineError> {
        Err(unavailable("RunService.SubmitTurn"))
    }
    fn interrupt_turn(
        &self,
        _request: pb::InterruptTurnRequest,
    ) -> Result<pb::RunMutationResponse, MachineError> {
        Err(unavailable("RunService.InterruptTurn"))
    }
    fn pause_run(
        &self,
        _request: pb::PauseRunRequest,
    ) -> Result<pb::RunMutationResponse, MachineError> {
        Err(unavailable("RunService.PauseRun"))
    }
    fn resume_run(
        &self,
        _request: pb::ResumeRunRequest,
    ) -> Result<pb::RunMutationResponse, MachineError> {
        Err(unavailable("RunService.ResumeRun"))
    }
    fn close_run(
        &self,
        _request: pb::CloseRunRequest,
    ) -> Result<pb::RunMutationResponse, MachineError> {
        Err(unavailable("RunService.CloseRun"))
    }
    fn recover_run(
        &self,
        _request: pb::RecoverRunRequest,
    ) -> Result<pb::RunMutationResponse, MachineError> {
        Err(unavailable("RunService.RecoverRun"))
    }
    fn reconcile_run(
        &self,
        _request: pb::ReconcileRunRequest,
    ) -> Result<pb::RunMutationResponse, MachineError> {
        Err(unavailable("RunService.ReconcileRun"))
    }
    fn watch_run_events(
        &self,
        _request: pb::WatchRunEventsRequest,
    ) -> Result<EventPage, MachineError> {
        Err(unavailable("ObservationService.WatchRunEvents"))
    }
    fn list_run_timeline_items(
        &self,
        _request: pb::ListRunTimelineItemsRequest,
    ) -> Result<pb::ListRunTimelineItemsResponse, MachineError> {
        Err(unavailable("ObservationService.ListRunTimelineItems"))
    }
    fn list_pending_interactions(
        &self,
        _request: pb::ListPendingInteractionsRequest,
    ) -> Result<pb::ListPendingInteractionsResponse, MachineError> {
        Err(unavailable("InteractionService.ListPendingInteractions"))
    }
    fn get_controller_interaction(
        &self,
        _request: pb::GetControllerInteractionRequest,
    ) -> Result<pb::GetControllerInteractionResponse, MachineError> {
        Err(unavailable("InteractionService.GetControllerInteraction"))
    }
    fn resolve_interaction(
        &self,
        _request: pb::ResolveInteractionRequest,
    ) -> Result<pb::ResolveInteractionResponse, MachineError> {
        Err(unavailable("InteractionService.ResolveInteraction"))
    }
    fn get_workspace_writer_status(
        &self,
        _request: pb::GetWorkspaceWriterStatusRequest,
    ) -> Result<pb::GetWorkspaceWriterStatusResponse, MachineError> {
        Err(unavailable("WriterService.GetWorkspaceWriterStatus"))
    }
    fn acquire_writer(
        &self,
        _request: pb::AcquireWriterRequest,
    ) -> Result<pb::WriterState, MachineError> {
        Err(unavailable("WriterService.AcquireWriter"))
    }
    fn release_writer(
        &self,
        _request: pb::ReleaseWriterRequest,
    ) -> Result<pb::WriterState, MachineError> {
        Err(unavailable("WriterService.ReleaseWriter"))
    }
    fn verify_controller(
        &self,
        _request: pb::VerifyControllerRequest,
    ) -> Result<pb::VerifyControllerResponse, MachineError> {
        Err(unavailable("ControllerService.VerifyController"))
    }
    fn get_artifact(
        &self,
        _request: pb::GetArtifactRequest,
    ) -> Result<pb::GetArtifactResponse, MachineError> {
        Err(unavailable("ArtifactService.GetArtifact"))
    }
    fn read_artifact_chunk(
        &self,
        _request: pb::ReadArtifactChunkRequest,
    ) -> Result<pb::ReadArtifactChunkResponse, MachineError> {
        Err(unavailable("ArtifactService.ReadArtifactChunk"))
    }
    fn get_orchestrated_session(
        &self,
        _request: pb::GetOrchestratedSessionRequest,
    ) -> Result<pb::GetOrchestratedSessionResponse, MachineError> {
        Err(unavailable("OrchestrationService.GetOrchestratedSession"))
    }
    fn list_orchestrated_session_results(
        &self,
        _request: pb::ListOrchestratedSessionResultsRequest,
    ) -> Result<pb::ListOrchestratedSessionResultsResponse, MachineError> {
        Err(unavailable(
            "OrchestrationService.ListOrchestratedSessionResults",
        ))
    }
}

#[derive(Clone)]
pub struct GatewayTransport {
    backend: Arc<dyn GatewayBackend>,
    workers: Arc<Semaphore>,
    stream_workers: Arc<Semaphore>,
    shutdown: watch::Receiver<bool>,
}

impl GatewayTransport {
    pub fn new(backend: Arc<dyn GatewayBackend>, shutdown: watch::Receiver<bool>) -> Self {
        Self {
            backend,
            workers: Arc::new(Semaphore::new(SEMANTIC_WORKERS)),
            stream_workers: Arc::new(Semaphore::new(STREAM_WORKERS)),
            shutdown,
        }
    }

    pub(crate) async fn execute<T: Send + 'static>(
        &self,
        method: &'static str,
        deadline: tokio::time::Instant,
        work: impl FnOnce(Arc<dyn GatewayBackend>) -> Result<T, MachineError> + Send + 'static,
    ) -> Result<T, Status> {
        self.execute_in(&self.workers, method, deadline, work, error_status)
            .await
    }

    async fn execute_stream<T: Send + 'static>(
        &self,
        method: &'static str,
        deadline: tokio::time::Instant,
        work: impl FnOnce(Arc<dyn GatewayBackend>) -> Result<T, MachineError> + Send + 'static,
    ) -> Result<T, Status> {
        self.execute_in(&self.stream_workers, method, deadline, work, error_status)
            .await
    }

    pub(crate) async fn execute_v2<T: Send + 'static>(
        &self,
        method: &'static str,
        deadline: tokio::time::Instant,
        work: impl FnOnce(Arc<dyn GatewayBackend>) -> Result<T, MachineError> + Send + 'static,
    ) -> Result<T, Status> {
        self.execute_in(
            &self.workers,
            method,
            deadline,
            work,
            crate::gateway_v2::error_status,
        )
        .await
    }

    pub(crate) async fn watch_v2(
        &self,
        request: crate::protocol::public_v2::WatchRunEventsRequest,
        deadline: tokio::time::Instant,
    ) -> Result<EventStream, Status> {
        let initial = request.clone();
        let page = self
            .execute_in(
                &self.stream_workers,
                "ObservationService.WatchRunEvents",
                deadline,
                move |backend| {
                    crate::gateway_observation_v2::watch_page(backend.modern()?, initial)
                },
                crate::gateway_v2::error_status,
            )
            .await?;
        let queue = Arc::new(Mutex::new(StreamQueue {
            map_error: crate::gateway_v2::error_status,
            ..Default::default()
        }));
        tokio::spawn(produce_event_pages(
            self.clone(),
            EventRequest::V2(request),
            page,
            queue.clone(),
        ));
        Ok(EventStream { queue })
    }

    async fn execute_in<T: Send + 'static>(
        &self,
        workers: &Arc<Semaphore>,
        method: &'static str,
        deadline: tokio::time::Instant,
        work: impl FnOnce(Arc<dyn GatewayBackend>) -> Result<T, MachineError> + Send + 'static,
        map_error: fn(&MachineError, &str) -> Status,
    ) -> Result<T, Status> {
        if *self.shutdown.borrow() {
            return Err(map_error(&transport_error("SERVER_SHUTDOWN"), method));
        }
        let mut shutdown = self.shutdown.clone();
        let permit = tokio::select! {
            _ = shutdown.changed() => return Err(map_error(&transport_error("SERVER_SHUTDOWN"), method)),
            _ = tokio::time::sleep_until(deadline) => return Err(map_error(&transport_error("TRANSPORT_FAILURE"), method)),
            permit = workers.clone().acquire_owned() => permit.map_err(|_| map_error(&transport_error("INTERNAL_ERROR"), method))?,
        };
        if *self.shutdown.borrow() {
            return Err(map_error(&transport_error("SERVER_SHUTDOWN"), method));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(map_error(&transport_error("TRANSPORT_FAILURE"), method));
        }
        let backend = self.backend.clone();
        let job = tokio::task::spawn_blocking(move || {
            // Dropping a response waiter does not cancel an accepted mutation.
            // Keep its physical executor permit until the work really exits.
            let _permit = permit;
            work(backend)
        });
        tokio::time::timeout_at(deadline, job)
            .await
            .map_err(|_| map_error(&transport_error("TRANSPORT_FAILURE"), method))?
            .map_err(|_| map_error(&transport_error("INTERNAL_ERROR"), method))?
            .map_err(|error| map_error(&error, method))
    }
}

#[tonic::async_trait]
impl pb::runtime_service_server::RuntimeService for GatewayTransport {
    async fn get_capabilities(
        &self,
        request: Request<pb::GetCapabilitiesRequest>,
    ) -> Result<Response<pb::GetCapabilitiesResponse>, Status> {
        let deadline = rpc_deadline(&request, "RuntimeService.GetCapabilities")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), true)
            .map_err(|error| error_status(&error, "RuntimeService.GetCapabilities"))?;
        if request.minimum_protocol_version > 1 || request.maximum_protocol_version < 1 {
            return Err(error_status(
                &transport_error("PROTOCOL_VERSION_UNSUPPORTED"),
                "RuntimeService.GetCapabilities",
            ));
        }
        self.execute("RuntimeService.GetCapabilities", deadline, move |backend| {
            backend.get_capabilities(request)
        })
        .await
        .map(Response::new)
    }
    async fn inspect_workspace(
        &self,
        request: Request<pb::InspectWorkspaceRequest>,
    ) -> Result<Response<pb::InspectWorkspaceResponse>, Status> {
        let deadline = rpc_deadline(&request, "RuntimeService.InspectWorkspace")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RuntimeService.InspectWorkspace"))?;
        self.execute(
            "RuntimeService.InspectWorkspace",
            deadline,
            move |backend| backend.inspect_workspace(request),
        )
        .await
        .map(Response::new)
    }
    async fn list_profiles(
        &self,
        request: Request<pb::ListProfilesRequest>,
    ) -> Result<Response<pb::ListProfilesResponse>, Status> {
        let deadline = rpc_deadline(&request, "RuntimeService.ListProfiles")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RuntimeService.ListProfiles"))?;
        self.execute("RuntimeService.ListProfiles", deadline, move |backend| {
            backend.list_profiles(request)
        })
        .await
        .map(Response::new)
    }
    async fn get_profile(
        &self,
        request: Request<pb::GetProfileRequest>,
    ) -> Result<Response<pb::GetProfileResponse>, Status> {
        let deadline = rpc_deadline(&request, "RuntimeService.GetProfile")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RuntimeService.GetProfile"))?;
        self.execute("RuntimeService.GetProfile", deadline, move |backend| {
            backend.get_profile(request)
        })
        .await
        .map(Response::new)
    }
    async fn list_profile_diagnostics(
        &self,
        request: Request<pb::ListProfileDiagnosticsRequest>,
    ) -> Result<Response<pb::ListProfileDiagnosticsResponse>, Status> {
        let _deadline = rpc_deadline(&request, "RuntimeService.ListProfileDiagnostics")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RuntimeService.ListProfileDiagnostics"))?;
        Err(error_status(
            &unavailable("RuntimeService.ListProfileDiagnostics"),
            "RuntimeService.ListProfileDiagnostics",
        ))
    }
}

#[tonic::async_trait]
impl pb::run_service_server::RunService for GatewayTransport {
    async fn start_run(
        &self,
        request: Request<pb::StartRunRequest>,
    ) -> Result<Response<pb::StartRunResponse>, Status> {
        let deadline = rpc_deadline(&request, "RunService.StartRun")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RunService.StartRun"))?;
        self.execute("RunService.StartRun", deadline, move |backend| {
            backend.start_run(request)
        })
        .await
        .map(Response::new)
    }
    async fn list_runs(
        &self,
        request: Request<pb::ListRunsRequest>,
    ) -> Result<Response<pb::ListRunsResponse>, Status> {
        let deadline = rpc_deadline(&request, "RunService.ListRuns")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RunService.ListRuns"))?;
        self.execute("RunService.ListRuns", deadline, move |backend| {
            backend.list_runs(request)
        })
        .await
        .map(Response::new)
    }
    async fn get_run(
        &self,
        request: Request<pb::GetRunRequest>,
    ) -> Result<Response<pb::GetRunResponse>, Status> {
        let deadline = rpc_deadline(&request, "RunService.GetRun")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RunService.GetRun"))?;
        self.execute("RunService.GetRun", deadline, move |backend| {
            backend.get_run(request)
        })
        .await
        .map(Response::new)
    }
    async fn submit_turn(
        &self,
        request: Request<pb::SubmitTurnRequest>,
    ) -> Result<Response<pb::SubmitTurnAccepted>, Status> {
        let deadline = rpc_deadline(&request, "RunService.SubmitTurn")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RunService.SubmitTurn"))?;
        self.execute("RunService.SubmitTurn", deadline, move |backend| {
            backend.submit_turn(request)
        })
        .await
        .map(Response::new)
    }
    async fn interrupt_turn(
        &self,
        request: Request<pb::InterruptTurnRequest>,
    ) -> Result<Response<pb::RunMutationResponse>, Status> {
        let deadline = rpc_deadline(&request, "RunService.InterruptTurn")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RunService.InterruptTurn"))?;
        self.execute("RunService.InterruptTurn", deadline, move |backend| {
            backend.interrupt_turn(request)
        })
        .await
        .map(Response::new)
    }
    async fn set_default_effort(
        &self,
        request: Request<pb::SetDefaultEffortRequest>,
    ) -> Result<Response<pb::RunMutationResponse>, Status> {
        let _deadline = rpc_deadline(&request, "RunService.SetDefaultEffort")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RunService.SetDefaultEffort"))?;
        Err(error_status(
            &unavailable("RunService.SetDefaultEffort"),
            "RunService.SetDefaultEffort",
        ))
    }
    async fn pause_run(
        &self,
        request: Request<pb::PauseRunRequest>,
    ) -> Result<Response<pb::RunMutationResponse>, Status> {
        let deadline = rpc_deadline(&request, "RunService.PauseRun")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RunService.PauseRun"))?;
        self.execute("RunService.PauseRun", deadline, move |backend| {
            backend.pause_run(request)
        })
        .await
        .map(Response::new)
    }
    async fn resume_run(
        &self,
        request: Request<pb::ResumeRunRequest>,
    ) -> Result<Response<pb::RunMutationResponse>, Status> {
        let deadline = rpc_deadline(&request, "RunService.ResumeRun")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RunService.ResumeRun"))?;
        self.execute("RunService.ResumeRun", deadline, move |backend| {
            backend.resume_run(request)
        })
        .await
        .map(Response::new)
    }
    async fn close_run(
        &self,
        request: Request<pb::CloseRunRequest>,
    ) -> Result<Response<pb::RunMutationResponse>, Status> {
        let deadline = rpc_deadline(&request, "RunService.CloseRun")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RunService.CloseRun"))?;
        self.execute("RunService.CloseRun", deadline, move |backend| {
            backend.close_run(request)
        })
        .await
        .map(Response::new)
    }
    async fn delete_run(
        &self,
        request: Request<pb::DeleteRunRequest>,
    ) -> Result<Response<pb::DeleteRunResponse>, Status> {
        let _deadline = rpc_deadline(&request, "RunService.DeleteRun")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RunService.DeleteRun"))?;
        Err(error_status(
            &unavailable("RunService.DeleteRun"),
            "RunService.DeleteRun",
        ))
    }
    async fn recover_run(
        &self,
        request: Request<pb::RecoverRunRequest>,
    ) -> Result<Response<pb::RunMutationResponse>, Status> {
        let deadline = rpc_deadline(&request, "RunService.RecoverRun")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RunService.RecoverRun"))?;
        self.execute("RunService.RecoverRun", deadline, move |backend| {
            backend.recover_run(request)
        })
        .await
        .map(Response::new)
    }
    async fn reconcile_run(
        &self,
        request: Request<pb::ReconcileRunRequest>,
    ) -> Result<Response<pb::RunMutationResponse>, Status> {
        let deadline = rpc_deadline(&request, "RunService.ReconcileRun")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RunService.ReconcileRun"))?;
        self.execute("RunService.ReconcileRun", deadline, move |backend| {
            backend.reconcile_run(request)
        })
        .await
        .map(Response::new)
    }
    async fn fork_run(
        &self,
        request: Request<pb::ForkRunRequest>,
    ) -> Result<Response<pb::StartRunResponse>, Status> {
        let _deadline = rpc_deadline(&request, "RunService.ForkRun")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RunService.ForkRun"))?;
        Err(error_status(
            &unavailable("RunService.ForkRun"),
            "RunService.ForkRun",
        ))
    }
    async fn verify_run(
        &self,
        request: Request<pb::VerifyRunRequest>,
    ) -> Result<Response<pb::VerifyRunResponse>, Status> {
        let _deadline = rpc_deadline(&request, "RunService.VerifyRun")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RunService.VerifyRun"))?;
        Err(error_status(
            &unavailable("RunService.VerifyRun"),
            "RunService.VerifyRun",
        ))
    }
    async fn create_write_continuation(
        &self,
        request: Request<pb::CreateWriteContinuationRequest>,
    ) -> Result<Response<pb::CreateWriteContinuationResponse>, Status> {
        let _deadline = rpc_deadline(&request, "RunService.CreateWriteContinuation")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "RunService.CreateWriteContinuation"))?;
        Err(error_status(
            &unavailable("RunService.CreateWriteContinuation"),
            "RunService.CreateWriteContinuation",
        ))
    }
}

#[tonic::async_trait]
impl pb::observation_service_server::ObservationService for GatewayTransport {
    type WatchRunEventsStream = EventStream;
    async fn watch_run_events(
        &self,
        request: Request<pb::WatchRunEventsRequest>,
    ) -> Result<Response<Self::WatchRunEventsStream>, Status> {
        let deadline = rpc_deadline(&request, "ObservationService.WatchRunEvents")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "ObservationService.WatchRunEvents"))?;
        let initial = request.clone();
        let page = self
            .execute_stream(
                "ObservationService.WatchRunEvents",
                deadline,
                move |backend| backend.watch_run_events(initial),
            )
            .await?;
        let queue = Arc::new(Mutex::new(StreamQueue::default()));
        tokio::spawn(produce_events(self.clone(), request, page, queue.clone()));
        Ok(Response::new(EventStream { queue }))
    }
    async fn list_run_timeline_items(
        &self,
        request: Request<pb::ListRunTimelineItemsRequest>,
    ) -> Result<Response<pb::ListRunTimelineItemsResponse>, Status> {
        let deadline = rpc_deadline(&request, "ObservationService.ListRunTimelineItems")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "ObservationService.ListRunTimelineItems"))?;
        self.execute(
            "ObservationService.ListRunTimelineItems",
            deadline,
            move |backend| backend.list_run_timeline_items(request),
        )
        .await
        .map(Response::new)
    }
}

#[tonic::async_trait]
impl pb::interaction_service_server::InteractionService for GatewayTransport {
    async fn list_pending_interactions(
        &self,
        request: Request<pb::ListPendingInteractionsRequest>,
    ) -> Result<Response<pb::ListPendingInteractionsResponse>, Status> {
        let deadline = rpc_deadline(&request, "InteractionService.ListPendingInteractions")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "InteractionService.ListPendingInteractions"))?;
        self.execute(
            "InteractionService.ListPendingInteractions",
            deadline,
            move |backend| backend.list_pending_interactions(request),
        )
        .await
        .map(Response::new)
    }
    async fn get_controller_interaction(
        &self,
        request: Request<pb::GetControllerInteractionRequest>,
    ) -> Result<Response<pb::GetControllerInteractionResponse>, Status> {
        let deadline = rpc_deadline(&request, "InteractionService.GetControllerInteraction")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "InteractionService.GetControllerInteraction"))?;
        self.execute(
            "InteractionService.GetControllerInteraction",
            deadline,
            move |backend| backend.get_controller_interaction(request),
        )
        .await
        .map(Response::new)
    }
    async fn resolve_interaction(
        &self,
        request: Request<pb::ResolveInteractionRequest>,
    ) -> Result<Response<pb::ResolveInteractionResponse>, Status> {
        let deadline = rpc_deadline(&request, "InteractionService.ResolveInteraction")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "InteractionService.ResolveInteraction"))?;
        self.execute(
            "InteractionService.ResolveInteraction",
            deadline,
            move |backend| backend.resolve_interaction(request),
        )
        .await
        .map(Response::new)
    }
}

#[tonic::async_trait]
impl pb::writer_service_server::WriterService for GatewayTransport {
    async fn get_workspace_writer_status(
        &self,
        request: Request<pb::GetWorkspaceWriterStatusRequest>,
    ) -> Result<Response<pb::GetWorkspaceWriterStatusResponse>, Status> {
        let deadline = rpc_deadline(&request, "WriterService.GetWorkspaceWriterStatus")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "WriterService.GetWorkspaceWriterStatus"))?;
        self.execute(
            "WriterService.GetWorkspaceWriterStatus",
            deadline,
            move |backend| backend.get_workspace_writer_status(request),
        )
        .await
        .map(Response::new)
    }
    async fn acquire_writer(
        &self,
        request: Request<pb::AcquireWriterRequest>,
    ) -> Result<Response<pb::WriterState>, Status> {
        let deadline = rpc_deadline(&request, "WriterService.AcquireWriter")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "WriterService.AcquireWriter"))?;
        self.execute("WriterService.AcquireWriter", deadline, move |backend| {
            backend.acquire_writer(request)
        })
        .await
        .map(Response::new)
    }
    async fn release_writer(
        &self,
        request: Request<pb::ReleaseWriterRequest>,
    ) -> Result<Response<pb::WriterState>, Status> {
        let deadline = rpc_deadline(&request, "WriterService.ReleaseWriter")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "WriterService.ReleaseWriter"))?;
        self.execute("WriterService.ReleaseWriter", deadline, move |backend| {
            backend.release_writer(request)
        })
        .await
        .map(Response::new)
    }
    async fn prepare_writer_handoff(
        &self,
        request: Request<pb::PrepareWriterHandoffRequest>,
    ) -> Result<Response<pb::WriterHandoffResponse>, Status> {
        let _deadline = rpc_deadline(&request, "WriterService.PrepareWriterHandoff")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "WriterService.PrepareWriterHandoff"))?;
        Err(error_status(
            &unavailable("WriterService.PrepareWriterHandoff"),
            "WriterService.PrepareWriterHandoff",
        ))
    }
    async fn commit_writer_handoff(
        &self,
        request: Request<pb::CommitWriterHandoffRequest>,
    ) -> Result<Response<pb::WriterHandoffResponse>, Status> {
        let _deadline = rpc_deadline(&request, "WriterService.CommitWriterHandoff")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "WriterService.CommitWriterHandoff"))?;
        Err(error_status(
            &unavailable("WriterService.CommitWriterHandoff"),
            "WriterService.CommitWriterHandoff",
        ))
    }
    async fn cancel_writer_handoff(
        &self,
        request: Request<pb::CancelWriterHandoffRequest>,
    ) -> Result<Response<pb::WriterHandoffResponse>, Status> {
        let _deadline = rpc_deadline(&request, "WriterService.CancelWriterHandoff")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "WriterService.CancelWriterHandoff"))?;
        Err(error_status(
            &unavailable("WriterService.CancelWriterHandoff"),
            "WriterService.CancelWriterHandoff",
        ))
    }
}

#[tonic::async_trait]
impl pb::controller_service_server::ControllerService for GatewayTransport {
    async fn verify_controller(
        &self,
        request: Request<pb::VerifyControllerRequest>,
    ) -> Result<Response<pb::VerifyControllerResponse>, Status> {
        let deadline = rpc_deadline(&request, "ControllerService.VerifyController")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "ControllerService.VerifyController"))?;
        self.execute(
            "ControllerService.VerifyController",
            deadline,
            move |backend| backend.verify_controller(request),
        )
        .await
        .map(Response::new)
    }
}

#[tonic::async_trait]
impl pb::artifact_service_server::ArtifactService for GatewayTransport {
    async fn get_artifact(
        &self,
        request: Request<pb::GetArtifactRequest>,
    ) -> Result<Response<pb::GetArtifactResponse>, Status> {
        let deadline = rpc_deadline(&request, "ArtifactService.GetArtifact")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "ArtifactService.GetArtifact"))?;
        self.execute("ArtifactService.GetArtifact", deadline, move |backend| {
            backend.get_artifact(request)
        })
        .await
        .map(Response::new)
    }
    async fn read_artifact_chunk(
        &self,
        request: Request<pb::ReadArtifactChunkRequest>,
    ) -> Result<Response<pb::ReadArtifactChunkResponse>, Status> {
        let deadline = rpc_deadline(&request, "ArtifactService.ReadArtifactChunk")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "ArtifactService.ReadArtifactChunk"))?;
        self.execute(
            "ArtifactService.ReadArtifactChunk",
            deadline,
            move |backend| backend.read_artifact_chunk(request),
        )
        .await
        .map(Response::new)
    }
}

#[tonic::async_trait]
impl pb::orchestration_service_server::OrchestrationService for GatewayTransport {
    async fn get_orchestrated_session(
        &self,
        request: Request<pb::GetOrchestratedSessionRequest>,
    ) -> Result<Response<pb::GetOrchestratedSessionResponse>, Status> {
        let deadline = rpc_deadline(&request, "OrchestrationService.GetOrchestratedSession")?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false)
            .map_err(|error| error_status(&error, "OrchestrationService.GetOrchestratedSession"))?;
        self.execute(
            "OrchestrationService.GetOrchestratedSession",
            deadline,
            move |backend| backend.get_orchestrated_session(request),
        )
        .await
        .map(Response::new)
    }

    async fn list_orchestrated_session_results(
        &self,
        request: Request<pb::ListOrchestratedSessionResultsRequest>,
    ) -> Result<Response<pb::ListOrchestratedSessionResultsResponse>, Status> {
        let deadline = rpc_deadline(
            &request,
            "OrchestrationService.ListOrchestratedSessionResults",
        )?;
        let request = request.into_inner();
        validate_context(request.context.as_ref(), false).map_err(|error| {
            error_status(
                &error,
                "OrchestrationService.ListOrchestratedSessionResults",
            )
        })?;
        self.execute(
            "OrchestrationService.ListOrchestratedSessionResults",
            deadline,
            move |backend| backend.list_orchestrated_session_results(request),
        )
        .await
        .map(Response::new)
    }
}

fn rpc_deadline<T>(request: &Request<T>, method: &str) -> Result<tokio::time::Instant, Status> {
    rpc_deadline_with(request, method, error_status)
}

pub(crate) fn rpc_deadline_with<T>(
    request: &Request<T>,
    method: &str,
    map_error: fn(&MachineError, &str) -> Status,
) -> Result<tokio::time::Instant, Status> {
    let invalid_timeout = || {
        map_error(
            &MachineError::invalid_argument("grpc-timeout", "invalid RPC timeout"),
            method,
        )
    };
    let budget = match request.metadata().get("grpc-timeout") {
        None => RPC_BUDGET,
        Some(value) => {
            let text = value.to_str().map_err(|_| invalid_timeout())?;
            if !(2..=9).contains(&text.len()) {
                return Err(invalid_timeout());
            }
            let (digits, unit) = text.split_at(text.len() - 1);
            if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid_timeout());
            }
            let amount = digits.parse::<u64>().map_err(|_| invalid_timeout())?;
            let duration = match unit {
                "H" => Duration::from_secs(amount * 3600),
                "M" => Duration::from_secs(amount * 60),
                "S" => Duration::from_secs(amount),
                "m" => Duration::from_millis(amount),
                "u" => Duration::from_micros(amount),
                "n" => Duration::from_nanos(amount),
                _ => return Err(invalid_timeout()),
            };
            duration.min(RPC_BUDGET)
        }
    };
    Ok(tokio::time::Instant::now() + budget)
}

fn validate_context(
    context: Option<&pb::RequestContext>,
    handshake: bool,
) -> Result<(), MachineError> {
    let context = context
        .ok_or_else(|| MachineError::invalid_argument("context", "request context is required"))?;
    if context.protocol_version != 1 && !(handshake && context.protocol_version == 0) {
        return Err(transport_error("PROTOCOL_VERSION_UNSUPPORTED"));
    }
    // The wire contract requires a request UUID after negotiation, without
    // restricting its version. Client instance identity is opaque metadata.
    if context.protocol_version != 0 && Uuid::parse_str(&context.client_request_id).is_err() {
        return Err(MachineError::invalid_argument(
            "client_request_id",
            "a request UUID is required",
        ));
    }
    Ok(())
}

struct StreamQueue {
    map_error: fn(&MachineError, &str) -> Status,
    items: VecDeque<pb::RunEventEnvelope>,
    bytes: usize,
    last_delivery: Option<Instant>,
    error: Option<Status>,
    finished: bool,
    closed: bool,
    waker: Option<Waker>,
}

impl Default for StreamQueue {
    fn default() -> Self {
        Self {
            map_error: error_status,
            items: VecDeque::new(),
            bytes: 0,
            last_delivery: None,
            error: None,
            finished: false,
            closed: false,
            waker: None,
        }
    }
}

impl StreamQueue {
    fn push(&mut self, item: pb::RunEventEnvelope) -> bool {
        if self.closed || self.finished {
            return false;
        }
        let bytes = item.encoded_len();
        if self.items.len() >= STREAM_COUNT
            || self.bytes.saturating_add(bytes) > STREAM_BYTES
            || self.stalled()
        {
            self.fail((self.map_error)(
                &transport_error("SLOW_CONSUMER"),
                "ObservationService.WatchRunEvents",
            ));
            return false;
        }
        if self.items.is_empty() {
            self.last_delivery = Some(Instant::now());
        }
        self.bytes += bytes;
        self.items.push_back(item);
        self.wake();
        true
    }

    fn stalled(&self) -> bool {
        !self.items.is_empty()
            && self
                .last_delivery
                .is_some_and(|delivered| delivered.elapsed() >= STREAM_STALL)
    }

    fn fail(&mut self, error: Status) {
        self.items.clear();
        self.bytes = 0;
        self.error = Some(error);
        self.finished = true;
        self.wake();
    }

    fn wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

pub struct EventStream {
    queue: Arc<Mutex<StreamQueue>>,
}
impl Stream for EventStream {
    type Item = Result<pb::RunEventEnvelope, Status>;
    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut queue = self
            .queue
            .lock()
            .expect("stream queue mutex is not poisoned");
        if queue.stalled() {
            let map_error = queue.map_error;
            queue.fail(map_error(
                &transport_error("SLOW_CONSUMER"),
                "ObservationService.WatchRunEvents",
            ));
        }
        if let Some(item) = queue.items.pop_front() {
            queue.bytes -= item.encoded_len();
            queue.last_delivery = if queue.items.is_empty() {
                None
            } else {
                Some(Instant::now())
            };
            return Poll::Ready(Some(Ok(item)));
        }
        if let Some(error) = queue.error.take() {
            return Poll::Ready(Some(Err(error)));
        }
        if queue.finished {
            return Poll::Ready(None);
        }
        queue.waker = Some(context.waker().clone());
        Poll::Pending
    }
}
impl Drop for EventStream {
    fn drop(&mut self) {
        self.queue
            .lock()
            .expect("stream queue mutex is not poisoned")
            .closed = true;
    }
}

async fn produce_events(
    transport: GatewayTransport,
    request: pb::WatchRunEventsRequest,
    page: EventPage,
    queue: Arc<Mutex<StreamQueue>>,
) {
    produce_event_pages(transport, EventRequest::V1(request), page, queue).await;
}

enum EventRequest {
    V1(pb::WatchRunEventsRequest),
    V2(crate::protocol::public_v2::WatchRunEventsRequest),
}
impl EventRequest {
    fn run_id(&self) -> String {
        match self {
            Self::V1(v) => v.run.as_ref().map(|v| v.run_id.clone()).unwrap_or_default(),
            Self::V2(v) => v.run.as_ref().map(|v| v.run_id.clone()).unwrap_or_default(),
        }
    }
    fn after(&mut self, after: String) {
        match self {
            Self::V1(v) => v.after_cursor = after,
            Self::V2(v) => v.after_cursor = after,
        }
    }
}
async fn produce_event_pages(
    transport: GatewayTransport,
    mut request: EventRequest,
    mut page: EventPage,
    queue: Arc<Mutex<StreamQueue>>,
) {
    let method = "ObservationService.WatchRunEvents";
    let run_id = request.run_id();
    let map_error = queue
        .lock()
        .expect("stream queue mutex is not poisoned")
        .map_error;
    let initial_head = match page.durable_head_cursor.parse::<u64>() {
        Ok(head) => head,
        Err(_) => {
            queue
                .lock()
                .expect("stream queue mutex is not poisoned")
                .fail(map_error(&transport_error("INTERNAL_ERROR"), method));
            return;
        }
    };
    let mut shutdown = transport.shutdown.clone();
    let mut last_heartbeat = Instant::now();
    let mut sample_delay = Duration::from_millis(100);
    loop {
        {
            let mut queue = queue.lock().expect("stream queue mutex is not poisoned");
            if queue.closed {
                return;
            }
            if page.events.len() > STREAM_COUNT
                || page
                    .events
                    .iter()
                    .map(prost::Message::encoded_len)
                    .sum::<usize>()
                    > STREAM_BYTES
            {
                queue.fail(map_error(&transport_error("SLOW_CONSUMER"), method));
                return;
            }
            for mut event in page.events.drain(..) {
                if let Some(pb::run_event_envelope::Item::DurableEvent(durable)) = &mut event.item {
                    let Ok(cursor) = durable.cursor.parse::<u64>() else {
                        queue.fail(map_error(&transport_error("INTERNAL_ERROR"), method));
                        return;
                    };
                    durable.replay = cursor <= initial_head;
                }
                if !queue.push(event) {
                    return;
                }
            }
            if page.terminal
                && page.next_cursor == page.durable_head_cursor
                && queue.items.is_empty()
            {
                if !queue.push(stream_end(
                    &run_id,
                    &page.durable_head_cursor,
                    pb::StreamEndReason::RunTerminal,
                )) {
                    return;
                }
                queue.finished = true;
                queue.wake();
                return;
            }
            if !page.terminal
                && queue.items.is_empty()
                && last_heartbeat.elapsed() >= Duration::from_secs(30)
            {
                let elapsed = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default();
                if !queue.push(pb::RunEventEnvelope {
                    item: Some(pb::run_event_envelope::Item::Heartbeat(
                        pb::RunEventHeartbeat {
                            run_id: run_id.clone(),
                            durable_head_cursor: page.durable_head_cursor.clone(),
                            lifecycle: page.lifecycle,
                            emitted_at: Some(prost_types::Timestamp {
                                seconds: elapsed.as_secs() as i64,
                                nanos: elapsed.subsec_nanos() as i32,
                            }),
                        },
                    )),
                }) {
                    return;
                }
                last_heartbeat = Instant::now();
            }
        }
        request.after(page.next_cursor.clone());
        tokio::select! {
            _ = shutdown.changed() => {
                let mut queue = queue.lock().expect("stream queue mutex is not poisoned");
                queue.items.clear(); queue.bytes = 0;
                queue.push(stream_end(&run_id, &page.durable_head_cursor, pb::StreamEndReason::ServerShutdown));
                queue.error = Some(map_error(&transport_error("SERVER_SHUTDOWN"), method));
                queue.finished = true; queue.wake(); return;
            }
            _ = tokio::time::sleep(sample_delay) => {}
        }
        {
            let mut queue = queue.lock().expect("stream queue mutex is not poisoned");
            if queue.closed {
                return;
            }
            if queue.stalled() {
                queue.fail(map_error(&transport_error("SLOW_CONSUMER"), method));
                return;
            }
            // Terminal lifecycle does not prove that bounded replay is exhausted.
            // Drain this page before sampling its successor, including an empty
            // sample needed to prove a trailing filtered gap.
            if page.terminal
                && (!queue.items.is_empty() || page.next_cursor == page.durable_head_cursor)
            {
                continue;
            }
        }
        let fetched = match &request {
            EventRequest::V1(request) => {
                let request = request.clone();
                transport
                    .execute_stream(
                        method,
                        tokio::time::Instant::now() + RPC_BUDGET,
                        move |backend| backend.watch_run_events(request),
                    )
                    .await
            }
            EventRequest::V2(request) => {
                let request = request.clone();
                transport
                    .execute_in(
                        &transport.stream_workers,
                        method,
                        tokio::time::Instant::now() + RPC_BUDGET,
                        move |backend| {
                            crate::gateway_observation_v2::watch_page(backend.modern()?, request)
                        },
                        map_error,
                    )
                    .await
            }
        };
        match fetched {
            Ok(next) => {
                sample_delay = if next.durable_head_cursor == page.durable_head_cursor {
                    (sample_delay * 2).min(Duration::from_millis(500))
                } else {
                    Duration::from_millis(100)
                };
                page = next;
            }
            Err(error) => {
                queue
                    .lock()
                    .expect("stream queue mutex is not poisoned")
                    .fail(error);
                return;
            }
        }
    }
}

fn stream_end(run_id: &str, cursor: &str, reason: pb::StreamEndReason) -> pb::RunEventEnvelope {
    pb::RunEventEnvelope {
        item: Some(pb::run_event_envelope::Item::StreamEnd(
            pb::RunEventStreamEnd {
                run_id: run_id.to_owned(),
                durable_head_cursor: cursor.to_owned(),
                reason: reason as i32,
            },
        )),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct GoogleRpcStatus {
    #[prost(int32, tag = "1")]
    pub(crate) code: i32,
    #[prost(string, tag = "2")]
    pub(crate) message: String,
    #[prost(message, repeated, tag = "3")]
    pub(crate) details: Vec<prost_types::Any>,
}

/// Checked mapping is the sole source of status, retry and recovery policy.
/// Free-form Machine messages/details never cross the public boundary.
pub fn error_status(error: &MachineError, method: &str) -> Status {
    static MAPPING: OnceLock<Value> = OnceLock::new();
    let mapping = MAPPING.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-grpc-error-mapping-v1.json"
        ))
        .expect("checked gRPC mapping is valid")
    });
    let legacy_error = crate::machine::legacy_transport_error(error);
    let private = mapping["private_machine_error_codes"]
        .as_array()
        .expect("checked private Machine error list")
        .iter()
        .any(|code| code.as_str() == Some(legacy_error.code.as_str()));
    let code = if crate::machine::registered_errors().contains_key(&legacy_error.code) && !private {
        legacy_error.code.as_str()
    } else {
        "INTERNAL_ERROR"
    };
    let status_name = mapped_name(mapping, "status_overrides", code, "default_status");
    let status = match status_name {
        "INVALID_ARGUMENT" => tonic::Code::InvalidArgument,
        "NOT_FOUND" => tonic::Code::NotFound,
        "PERMISSION_DENIED" => tonic::Code::PermissionDenied,
        "ABORTED" => tonic::Code::Aborted,
        "RESOURCE_EXHAUSTED" => tonic::Code::ResourceExhausted,
        "UNAVAILABLE" => tonic::Code::Unavailable,
        "DATA_LOSS" => tonic::Code::DataLoss,
        "INTERNAL" => tonic::Code::Internal,
        _ => tonic::Code::FailedPrecondition,
    };
    let class = method_class(method);
    let override_value = |field: &str, global: &'static str, default: &'static str| {
        mapping["method_overrides"][method][code][field]
            .as_str()
            .or_else(|| mapping["method_class_overrides"][class][code][field].as_str())
            .unwrap_or_else(|| mapped_name(mapping, global, code, default))
            .to_owned()
    };
    let action = override_value(
        "required_action",
        "required_action_overrides",
        "default_required_action",
    );
    let retry = override_value(
        "retry_classification",
        "retry_classification_overrides",
        "default_retry_classification",
    );
    let recovery = override_value(
        "recovery_classification",
        "recovery_classification_overrides",
        "default_recovery_classification",
    );
    let uuid_field = |name: &str| {
        error.details[name]
            .as_str()
            .filter(|value| Uuid::parse_str(value).is_ok())
            .map(str::to_owned)
    };
    let detail = pb::DolgoraeErrorDetail {
        detail_version: 1,
        dolgorae_error_code: code.to_owned(),
        action: pb::RequiredClientAction::from_str_name(&action).expect("checked action") as i32,
        run_id: uuid_field("run_id"),
        turn_id: uuid_field("turn_id"),
        interaction_id: uuid_field("interaction_id"),
        operation_id: uuid_field("operation_id"),
        idempotency_key: None,
        retry_classification: pb::RetryClassification::from_str_name(&retry).expect("checked retry")
            as i32,
        recovery_classification: pb::RecoveryClassification::from_str_name(&recovery)
            .expect("checked recovery") as i32,
        safe_resume_cursor: None,
    };
    let rich = GoogleRpcStatus {
        code: status as i32,
        message: code.to_owned(),
        details: vec![prost_types::Any {
            type_url: "type.googleapis.com/dolgorae.public.v1.DolgoraeErrorDetail".into(),
            value: detail.encode_to_vec(),
        }],
    };
    Status::with_details(status, code, rich.encode_to_vec().into())
}

fn mapped_name<'a>(mapping: &'a Value, field: &str, code: &str, default: &str) -> &'a str {
    mapping[field]
        .as_object()
        .expect("checked override map")
        .iter()
        .find(|(_, codes)| {
            codes
                .as_array()
                .expect("checked code list")
                .iter()
                .any(|item| item.as_str() == Some(code))
        })
        .map(|(name, _)| name.as_str())
        .unwrap_or_else(|| mapping[default].as_str().expect("checked default"))
}

fn method_class(method: &str) -> &'static str {
    match method {
        "ObservationService.WatchRunEvents" => "server_stream",
        "RunService.StartRun"
        | "RunService.SubmitTurn"
        | "RunService.ForkRun"
        | "RunService.CreateWriteContinuation"
        | "InteractionService.ResolveInteraction" => "idempotent_mutation",
        "RunService.InterruptTurn"
        | "RunService.SetDefaultEffort"
        | "RunService.PauseRun"
        | "RunService.ResumeRun"
        | "RunService.CloseRun"
        | "RunService.DeleteRun"
        | "RunService.RecoverRun"
        | "RunService.ReconcileRun"
        | "WriterService.AcquireWriter"
        | "WriterService.ReleaseWriter"
        | "WriterService.PrepareWriterHandoff"
        | "WriterService.CommitWriterHandoff"
        | "WriterService.CancelWriterHandoff" => "tokenless_mutation",
        _ => "unary_read",
    }
}

fn unavailable(method: &str) -> MachineError {
    MachineError::new(
        "CAPABILITY_UNSUPPORTED",
        "RPC is unavailable in this delivery milestone",
        false,
        json!({"method": method}),
    )
}
fn transport_error(code: &str) -> MachineError {
    let details = match code {
        "INTERNAL_ERROR" => json!({"invariant": "gateway transport runtime failed"}),
        "TRANSPORT_FAILURE" => {
            json!({"stage": "shutdown", "acceptance": "uncertain", "request_id": null})
        }
        "SERVER_SHUTDOWN" => {
            json!({"action": "refresh_snapshot", "operation_id": null, "run_id": null})
        }
        _ => json!({}),
    };
    MachineError::new(code, "gateway transport operation failed", false, details)
}

/// Writes the single readiness envelope itself. The caller maps this result
/// to an exit code and must not print a second Machine envelope.
pub fn serve(
    home: &DolgoraeHome,
    socket_path: &Path,
    ready_fd: Option<i32>,
    make_backend: impl FnOnce(Uuid) -> Arc<dyn GatewayBackend>,
) -> Result<(), MachineError> {
    let mut output: Box<dyn std::io::Write> = match ready_fd {
        Some(fd) => Box::new(DarwinSystem.take_ready_file(fd).map_err(|_| {
            MachineError::invalid_argument("ready_fd", "readiness descriptor is unavailable")
        })?),
        None => Box::new(std::io::stdout()),
    };
    let startup = (|| {
        let socket = GatewaySocket::bind(home, socket_path)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(SEMANTIC_WORKERS + STREAM_WORKERS)
            .enable_all()
            .build()
            .map_err(|_| transport_error("INTERNAL_ERROR"))?;
        Ok::<_, MachineError>((socket, runtime))
    })();
    let (mut socket, runtime) = match startup {
        Ok(started) => started,
        Err(error) => {
            write_readiness(&mut output, &FailureEnvelope::new("serve", error.clone()))?;
            return Err(error);
        }
    };
    let backend = make_backend(socket.record().server_instance_id);
    // Register SIGTERM before publishing readiness so the supervisor may
    // immediately request shutdown without racing signal installation.
    let mut signal = match runtime.block_on(async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    }) {
        Ok(signal) => signal,
        Err(_) => {
            let error = transport_error("INTERNAL_ERROR");
            write_readiness(&mut output, &FailureEnvelope::new("serve", error.clone()))?;
            return Err(error);
        }
    };
    let control = match runtime.block_on(async {
        crate::scheduler::ControlPlaneRuntime::start(home, socket.record().server_instance_id)
    }) {
        Ok(control) => control,
        Err(error) => {
            write_readiness(&mut output, &FailureEnvelope::new("serve", error.clone()))?;
            return Err(error);
        }
    };
    let readiness = write_readiness(
        &mut output,
        &SuccessEnvelope::new("serve", socket.readiness_data()),
    );
    drop(output);
    let result = runtime.block_on(async {
        let mut drain_until = None;
        let result = match readiness {
            Ok(()) => {
                run_server_until(&mut socket, backend, async {
                    signal.recv().await;
                    let until = tokio::time::Instant::now() + DRAIN_BUDGET;
                    drain_until = Some(until);
                    control.request_stop();
                    until
                })
                .await
            }
            Err(error) => Err(error),
        };
        control
            .shutdown(drain_until.unwrap_or_else(|| tokio::time::Instant::now() + DRAIN_BUDGET))
            .await;
        result
    });
    // Blocking semantic calls are bounded in count but are not cancellation-safe;
    // do not wait beyond the foreground drain budget for their worker threads.
    runtime.shutdown_timeout(Duration::ZERO);
    let cleanup = socket.cleanup();
    result.and(cleanup)
}

fn write_readiness(
    output: &mut dyn std::io::Write,
    envelope: &impl serde::Serialize,
) -> Result<(), MachineError> {
    serde_json::to_writer(&mut *output, envelope)
        .map_err(|_| transport_error("TRANSPORT_FAILURE"))?;
    output
        .write_all(b"\n")
        .and_then(|()| output.flush())
        .map_err(|_| transport_error("TRANSPORT_FAILURE"))
}

struct GatewayConnection {
    stream: tokio::net::UnixStream,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
impl tonic::transport::server::Connected for GatewayConnection {
    type ConnectInfo = tonic::transport::server::UdsConnectInfo;
    fn connect_info(&self) -> Self::ConnectInfo {
        tonic::transport::server::Connected::connect_info(&self.stream)
    }
}
impl tokio::io::AsyncRead for GatewayConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buffer)
    }
}
impl tokio::io::AsyncWrite for GatewayConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

trait AcceptSource: Send + 'static {
    fn accept(&mut self) -> impl Future<Output = std::io::Result<tokio::net::UnixStream>> + Send;
}

impl AcceptSource for tokio::net::UnixListener {
    async fn accept(&mut self) -> std::io::Result<tokio::net::UnixStream> {
        tokio::net::UnixListener::accept(self)
            .await
            .map(|(stream, _)| stream)
    }
}

pub async fn run_server(
    socket: &mut GatewaySocket,
    backend: Arc<dyn GatewayBackend>,
    shutdown: impl Future<Output = ()>,
) -> Result<(), MachineError> {
    run_server_until(socket, backend, async {
        shutdown.await;
        tokio::time::Instant::now() + DRAIN_BUDGET
    })
    .await
}

async fn run_server_until(
    socket: &mut GatewaySocket,
    backend: Arc<dyn GatewayBackend>,
    shutdown: impl Future<Output = tokio::time::Instant>,
) -> Result<(), MachineError> {
    let listener = socket
        .take_listener()
        .ok_or_else(|| transport_error("INTERNAL_ERROR"))?;
    listener
        .set_nonblocking(true)
        .map_err(|_| transport_error("TRANSPORT_FAILURE"))?;
    let listener = tokio::net::UnixListener::from_std(listener)
        .map_err(|_| transport_error("TRANSPORT_FAILURE"))?;
    run_server_with_accept_until(listener, backend, shutdown).await
}

#[cfg(test)]
async fn run_server_with_accept(
    listener: impl AcceptSource,
    backend: Arc<dyn GatewayBackend>,
    shutdown: impl Future<Output = ()>,
) -> Result<(), MachineError> {
    run_server_with_accept_until(listener, backend, async {
        shutdown.await;
        tokio::time::Instant::now() + DRAIN_BUDGET
    })
    .await
}

async fn run_server_with_accept_until(
    mut listener: impl AcceptSource,
    backend: Arc<dyn GatewayBackend>,
    shutdown: impl Future<Output = tokio::time::Instant>,
) -> Result<(), MachineError> {
    let (stop, stopped) = watch::channel(false);
    let service = GatewayTransport::new(backend, stopped.clone());
    let (incoming, receiver) =
        tokio::sync::mpsc::channel::<std::io::Result<GatewayConnection>>(MAX_CONNECTIONS);
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let (accept_failed, mut accept_failure) = tokio::sync::oneshot::channel();
    let mut accept_stop = stopped.clone();
    let accept_task = tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                _ = accept_stop.changed() => break,
                accepted = listener.accept() => accepted,
            };
            let accepted = match accepted {
                Ok(socket) => socket.into_std().and_then(|socket| {
                    GatewaySocket::verify_peer(&socket)
                        .map_err(|_| std::io::Error::from(std::io::ErrorKind::PermissionDenied))?;
                    tokio::net::UnixStream::from_std(socket)
                }),
                Err(error)
                    if matches!(
                        error.raw_os_error(),
                        Some(libc::ECONNABORTED | libc::EINTR | libc::EMFILE | libc::ENFILE)
                    ) =>
                {
                    // Temporary connection/resource pressure must not end the
                    // incoming stream and tear down healthy HTTP/2 connections.
                    tokio::select! {
                        _ = accept_stop.changed() => break,
                        () = tokio::time::sleep(Duration::from_millis(50)) => continue,
                    }
                }
                Err(_) => {
                    // Tonic treats an exhausted incoming stream as success, so
                    // report fatal listener failure independently of that stream.
                    let _ = accept_failed.send(());
                    break;
                }
            };
            if let Ok(stream) = accepted {
                let Ok(permit) = connections.clone().try_acquire_owned() else {
                    continue;
                };
                let accepted = GatewayConnection {
                    stream,
                    _permit: permit,
                };
                tokio::select! {
                    _ = accept_stop.changed() => break,
                    sent = incoming.send(Ok(accepted)) => if sent.is_err() { break; },
                }
            }
        }
    });
    let mut server_stop = stopped.clone();
    let server = tonic::transport::Server::builder()
        .concurrency_limit_per_connection(16)
        .max_concurrent_streams(Some(16))
        .initial_stream_window_size(Some(65_535))
        .initial_connection_window_size(Some(1_048_576))
        .add_service(
            pb::runtime_service_server::RuntimeServiceServer::new(service.clone())
                .max_decoding_message_size(12 * 1024 * 1024)
                .max_encoding_message_size(12 * 1024 * 1024),
        )
        .add_service(
            pb::run_service_server::RunServiceServer::new(service.clone())
                .max_decoding_message_size(12 * 1024 * 1024)
                .max_encoding_message_size(12 * 1024 * 1024),
        )
        .add_service(
            pb::observation_service_server::ObservationServiceServer::new(service.clone())
                .max_decoding_message_size(12 * 1024 * 1024)
                .max_encoding_message_size(12 * 1024 * 1024),
        )
        .add_service(
            pb::interaction_service_server::InteractionServiceServer::new(service.clone())
                .max_decoding_message_size(12 * 1024 * 1024)
                .max_encoding_message_size(12 * 1024 * 1024),
        )
        .add_service(
            pb::writer_service_server::WriterServiceServer::new(service.clone())
                .max_decoding_message_size(12 * 1024 * 1024)
                .max_encoding_message_size(12 * 1024 * 1024),
        )
        .add_service(
            pb::controller_service_server::ControllerServiceServer::new(service.clone())
                .max_decoding_message_size(12 * 1024 * 1024)
                .max_encoding_message_size(12 * 1024 * 1024),
        )
        .add_service(
            pb::artifact_service_server::ArtifactServiceServer::new(service.clone())
                .max_decoding_message_size(12 * 1024 * 1024)
                .max_encoding_message_size(12 * 1024 * 1024),
        )
        .add_service(
            pb::orchestration_service_server::OrchestrationServiceServer::new(service.clone())
                .max_decoding_message_size(12 * 1024 * 1024)
                .max_encoding_message_size(12 * 1024 * 1024),
        )
        .add_service(crate::protocol::public_v2::observation_service_server::ObservationServiceServer::new(service.clone()).max_decoding_message_size(12 * 1024 * 1024).max_encoding_message_size(12 * 1024 * 1024))
        .add_service(crate::protocol::public_v2::operation_service_server::OperationServiceServer::new(service.clone()))
        .add_service(crate::protocol::public_v2::runtime_service_server::RuntimeServiceServer::new(service.clone()))
        .add_service(crate::protocol::public_v2::run_service_server::RunServiceServer::new(service.clone()))
        .add_service(crate::protocol::public_v2::writer_service_server::WriterServiceServer::new(service.clone()))
        .add_service(crate::protocol::public_v2::provisioning_service_server::ProvisioningServiceServer::new(service.clone()))
        .add_service(crate::protocol::public_v2::orchestration_service_server::OrchestrationServiceServer::new(service.clone()))
        .add_service(crate::protocol::public_v2::group_service_server::GroupServiceServer::new(service.clone()))
        .add_service(crate::protocol::public_v2::broker_service_server::BrokerServiceServer::new(service.clone()))
        .add_service(crate::protocol::public_v2::controller_service_server::ControllerServiceServer::new(service.clone()))
        .add_service(crate::protocol::public_v2::artifact_service_server::ArtifactServiceServer::new(service.clone()))
        .add_service(crate::protocol::public_v2::interaction_service_server::InteractionServiceServer::new(service.clone()).max_decoding_message_size(12 * 1024 * 1024).max_encoding_message_size(12 * 1024 * 1024))
        .serve_with_incoming_shutdown(
            tokio_stream::wrappers::ReceiverStream::new(receiver),
            async move {
                let _ = server_stop.changed().await;
            },
        );
    tokio::pin!(server);
    tokio::pin!(shutdown);
    let result = tokio::select! {
        Ok(()) = &mut accept_failure => Err(transport_error("TRANSPORT_FAILURE")),
        result = &mut server => result.map_err(|_| transport_error("TRANSPORT_FAILURE")),
        until = &mut shutdown => {
            let _ = stop.send(true);
            match tokio::time::timeout_at(until, &mut server).await {
                Ok(result) => result.map_err(|_| transport_error("TRANSPORT_FAILURE")),
                Err(_) => Ok(()),
            }
        }
    };
    let _ = stop.send(true);
    accept_task.abort();
    let _ = accept_task.await;
    // The server or shutdown branch may win the select after the accept task
    // reports failure. Do not let that race turn a fatal error into success.
    if accept_failure.try_recv().is_ok() {
        Err(transport_error("TRANSPORT_FAILURE"))
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb::run_service_server::RunService as _;
    use pb::runtime_service_server::RuntimeService as _;
    use tokio_stream::StreamExt as _;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_drain_uses_supplied_deadline_without_cancelling_accepted_work() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt as _;
        struct BlockedBackend {
            entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            release: Mutex<std::sync::mpsc::Receiver<()>>,
            finished: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        }
        impl GatewayBackend for BlockedBackend {
            fn get_capabilities(
                &self,
                _: pb::GetCapabilitiesRequest,
            ) -> Result<pb::GetCapabilitiesResponse, MachineError> {
                self.entered
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .send(())
                    .unwrap();
                self.release.lock().unwrap().recv().unwrap();
                self.finished
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .send(())
                    .unwrap();
                Ok(pb::GetCapabilitiesResponse::default())
            }
        }
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("gd-{}", Uuid::now_v7().simple()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("g.sock");
        let home = DolgoraeHome::from_canonical_home(root.clone()).unwrap();
        let mut socket = GatewaySocket::bind(&home, &path).unwrap();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let (finished, mut done) = tokio::sync::oneshot::channel();
        let backend = Arc::new(BlockedBackend {
            entered: Mutex::new(Some(entered)),
            release: Mutex::new(wait),
            finished: Mutex::new(Some(finished)),
        });
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let result =
                run_server_until(&mut socket, backend, async { stopped.await.unwrap() }).await;
            socket.cleanup().unwrap();
            result
        });
        let connect_path = path.clone();
        let channel = tonic::transport::Endpoint::try_from("http://[::]:50051")
            .unwrap()
            .connect_with_connector(tower::service_fn(move |_| {
                let path = connect_path.clone();
                async move {
                    tokio::net::UnixStream::connect(path)
                        .await
                        .map(hyper_util::rt::TokioIo::new)
                }
            }))
            .await
            .unwrap();
        let request = tokio::spawn(async move {
            pb::runtime_service_client::RuntimeServiceClient::new(channel)
                .get_capabilities(pb::GetCapabilitiesRequest {
                    context: context(0),
                    minimum_protocol_version: 1,
                    maximum_protocol_version: 1,
                })
                .await
        });
        started.await.unwrap();
        stop.send(tokio::time::Instant::now()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("server reset the supplied drain deadline")
            .unwrap()
            .unwrap();
        assert!(!path.exists());
        assert!(matches!(
            done.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        release.send(()).unwrap();
        done.await.unwrap();
        let _ = request.await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stream_work_saturation_preserves_unary_admission() {
        let (_stop, shutdown) = watch::channel(false);
        let service = GatewayTransport::new(Arc::new(Backend), shutdown);
        let mut jobs = Vec::new();
        let mut releases = Vec::new();
        for _ in 0..STREAM_WORKERS {
            let (release, wait) = std::sync::mpsc::channel();
            let (started, entered) = tokio::sync::oneshot::channel();
            releases.push(release);
            let copy = service.clone();
            jobs.push(tokio::spawn(async move {
                copy.execute_stream(
                    "ObservationService.WatchRunEvents",
                    tokio::time::Instant::now() + RPC_BUDGET,
                    move |_| {
                        let _ = started.send(());
                        wait.recv_timeout(Duration::from_secs(2)).unwrap();
                        Ok(())
                    },
                )
                .await
            }));
            entered.await.unwrap();
        }
        assert_eq!(service.stream_workers.available_permits(), 0);
        service
            .execute(
                "RuntimeService.GetCapabilities",
                tokio::time::Instant::now() + Duration::from_millis(100),
                |_| Ok(()),
            )
            .await
            .unwrap();
        for release in releases {
            release.send(()).unwrap();
        }
        for job in jobs {
            job.await.unwrap().unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn response_deadline_keeps_admitted_effect_and_permit_until_completion() {
        let (_stop, shutdown) = watch::channel(false);
        let service = GatewayTransport::new(Arc::new(Backend), shutdown);
        let (release, wait) = std::sync::mpsc::channel();
        let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let effect = completed.clone();
        let status = service
            .execute(
                "RunService.SubmitTurn",
                tokio::time::Instant::now() + Duration::from_millis(50),
                move |_| {
                    wait.recv_timeout(Duration::from_secs(2)).unwrap();
                    effect.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                },
            )
            .await
            .unwrap_err();
        assert_eq!(status.message(), "TRANSPORT_FAILURE");
        let recovery = detail(&status);
        assert_eq!(
            recovery.recovery_classification,
            pb::RecoveryClassification::SnapshotRequired as i32
        );
        assert_eq!(
            recovery.action,
            pb::RequiredClientAction::RefreshSnapshot as i32
        );
        assert_eq!(
            recovery.retry_classification,
            pb::RetryClassification::ExactIdempotencyKey as i32
        );
        assert_eq!(service.workers.available_permits(), SEMANTIC_WORKERS - 1);
        assert_eq!(completed.load(std::sync::atomic::Ordering::SeqCst), 0);
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while service.workers.available_permits() != SEMANTIC_WORKERS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(completed.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rpc_timeout_is_bounded_and_rejects_private_malformed_metadata() {
        let mut request = Request::new(());
        let before = tokio::time::Instant::now();
        request
            .metadata_mut()
            .insert("grpc-timeout", "20m".parse().unwrap());
        assert!(
            rpc_deadline(&request, "RunService.GetRun").unwrap()
                <= before + Duration::from_millis(25)
        );
        request
            .metadata_mut()
            .insert("grpc-timeout", "99999999H".parse().unwrap());
        assert!(
            rpc_deadline(&request, "RunService.GetRun").unwrap()
                <= before + RPC_BUDGET + Duration::from_millis(5)
        );
        for value in ["secret-canary", "123456789S", "-1S", "1x", "S"] {
            request
                .metadata_mut()
                .insert("grpc-timeout", value.parse().unwrap());
            let error = rpc_deadline(&request, "RunService.GetRun").unwrap_err();
            assert_eq!(error.code(), tonic::Code::InvalidArgument);
            assert!(!error.message().contains(value));
            assert!(
                !error.details().is_empty(),
                "checked error details are required"
            );
            assert!(!String::from_utf8_lossy(error.details()).contains(value));
        }
        let (_stop, shutdown) = watch::channel(false);
        let status = GatewayTransport::new(Arc::new(Backend), shutdown)
            .execute::<()>("RunService.SubmitTurn", tokio::time::Instant::now(), |_| {
                panic!("expired admission must not dispatch")
            })
            .await
            .unwrap_err();
        assert_eq!(status.message(), "TRANSPORT_FAILURE");
    }

    struct Backend;
    impl GatewayBackend for Backend {
        fn get_capabilities(
            &self,
            _: pb::GetCapabilitiesRequest,
        ) -> Result<pb::GetCapabilitiesResponse, MachineError> {
            Ok(pb::GetCapabilitiesResponse::default())
        }
    }
    fn context(version: u32) -> Option<pb::RequestContext> {
        Some(pb::RequestContext {
            protocol_version: version,
            client_request_id: Uuid::now_v7().to_string(),
            client_instance_id: "opaque-client".into(),
        })
    }
    fn detail(status: &Status) -> pb::DolgoraeErrorDetail {
        let rich = GoogleRpcStatus::decode(status.details()).unwrap();
        assert_eq!(rich.code, status.code() as i32);
        assert_eq!(rich.details.len(), 1);
        assert_eq!(
            rich.details[0].type_url,
            "type.googleapis.com/dolgorae.public.v1.DolgoraeErrorDetail"
        );
        pb::DolgoraeErrorDetail::decode(rich.details[0].value.as_slice()).unwrap()
    }
    fn heartbeat() -> pb::RunEventEnvelope {
        pb::RunEventEnvelope {
            item: Some(pb::run_event_envelope::Item::Heartbeat(
                pb::RunEventHeartbeat::default(),
            )),
        }
    }

    #[tokio::test]
    async fn protocol_handshake_and_unadvertised_methods_fail_closed() {
        let (_stop, stopped) = watch::channel(false);
        let gateway = GatewayTransport::new(Arc::new(Backend), stopped);
        gateway
            .get_capabilities(Request::new(pb::GetCapabilitiesRequest {
                context: context(0),
                minimum_protocol_version: 1,
                maximum_protocol_version: 1,
            }))
            .await
            .unwrap();
        let invalid = gateway
            .get_capabilities(Request::new(pb::GetCapabilitiesRequest {
                context: context(0),
                minimum_protocol_version: 2,
                maximum_protocol_version: 3,
            }))
            .await
            .unwrap_err();
        assert_eq!(
            detail(&invalid).dolgorae_error_code,
            "PROTOCOL_VERSION_UNSUPPORTED"
        );
        let invalid = gateway
            .get_run(Request::new(pb::GetRunRequest {
                context: context(0),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(
            detail(&invalid).dolgorae_error_code,
            "PROTOCOL_VERSION_UNSUPPORTED"
        );
        let unavailable = gateway
            .delete_run(Request::new(pb::DeleteRunRequest {
                context: context(1),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(
            detail(&unavailable).dolgorae_error_code,
            "CAPABILITY_UNSUPPORTED"
        );
        assert_eq!(
            detail(&unavailable).action,
            pb::RequiredClientAction::RefreshCapabilities as i32
        );
    }

    #[test]
    fn public_v1_capacity_uses_the_frozen_reporting_boundary() {
        let run_id = Uuid::now_v7();
        let capacity = MachineError::new(
            "STORAGE_CAPACITY_EXCEEDED",
            "capacity",
            false,
            serde_json::json!({"run_id":run_id,"required_action":"operator_checkpoint"}),
        );
        let status = error_status(&capacity, "RunService.SubmitTurn");
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        let rich = <GoogleRpcStatus as prost::Message>::decode(status.details()).unwrap();
        let detail =
            <pb::DolgoraeErrorDetail as prost::Message>::decode(rich.details[0].value.as_slice())
                .unwrap();
        assert_eq!(detail.dolgorae_error_code, "CAPABILITY_UNSUPPORTED");
        assert_eq!(
            detail.action,
            pb::RequiredClientAction::from_str_name("REQUIRED_CLIENT_ACTION_REFRESH_CAPABILITIES")
                .unwrap() as i32
        );
        assert_eq!(detail.run_id, Some(run_id.to_string()));
        assert_eq!(capacity.code, "STORAGE_CAPACITY_EXCEEDED");
    }

    #[test]
    fn checked_error_tables_cover_every_public_method() {
        let descriptor =
            prost_types::FileDescriptorSet::decode(crate::protocol::PUBLIC_V1_DESCRIPTOR).unwrap();
        let mapping: Value = serde_json::from_str(include_str!(
            "../docs/protocol/dolgorae-grpc-error-mapping-v1.json"
        ))
        .unwrap();
        let private_codes = mapping["private_machine_error_codes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|code| code.as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        let mut methods = 0;
        for service in descriptor.file.iter().flat_map(|file| &file.service) {
            for method in &service.method {
                methods += 1;
                let name = format!(
                    "{}.{}",
                    service.name.as_deref().unwrap(),
                    method.name.as_deref().unwrap(),
                );
                for code in crate::machine::registered_errors().keys() {
                    let status =
                        error_status(&MachineError::new(code, "", false, json!({})), &name);
                    let detail = detail(&status);
                    let expected = if private_codes.contains(code.as_str()) {
                        "INTERNAL_ERROR"
                    } else if code == "RESULT_CONTENT_UNAVAILABLE" {
                        "CAPABILITY_UNSUPPORTED"
                    } else {
                        code
                    };
                    assert_eq!(detail.dolgorae_error_code, expected);
                    assert!(pb::RequiredClientAction::try_from(detail.action).is_ok());
                    assert!(pb::RetryClassification::try_from(detail.retry_classification).is_ok());
                    assert!(
                        pb::RecoveryClassification::try_from(detail.recovery_classification)
                            .is_ok()
                    );
                }
            }
        }
        assert_eq!(methods, 36);
    }

    #[test]
    fn rich_error_mapping_preserves_method_policy_without_raw_secrets() {
        let error = MachineError::new(
            "SERVER_SHUTDOWN",
            "secret response body",
            false,
            json!({"response": "secret response body", "operation_id": "not-a-uuid"}),
        );
        let status = error_status(&error, "InteractionService.ResolveInteraction");
        let detail = detail(&status);
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(
            detail.action,
            pb::RequiredClientAction::RefetchInteraction as i32
        );
        assert_eq!(
            detail.recovery_classification,
            pb::RecoveryClassification::OutcomeUnknown as i32
        );
        assert!(detail.operation_id.is_none());
        assert!(!String::from_utf8_lossy(status.details()).contains("secret"));
        assert!(!status.message().contains("secret"));
        let unknown = error_status(
            &MachineError::new("secret unknown code", "", false, json!({})),
            "RunService.GetRun",
        );
        assert_eq!(unknown.message(), "INTERNAL_ERROR");
    }

    #[test]
    fn close_in_progress_error_preserves_checked_policy_and_operation_identity() {
        let run_id = Uuid::now_v7().to_string();
        let operation_id = Uuid::now_v7().to_string();
        let error = MachineError::new(
            "SESSION_CLOSE_IN_PROGRESS",
            "whole-session close was accepted and is still settling",
            false,
            json!({
                "run_id": run_id,
                "operation_id": operation_id,
                "required_action": "refresh_snapshot"
            }),
        );
        assert_eq!(error.exit_status(), 4);
        let status = error_status(&error, "RunService.CloseRun");
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        assert_eq!(status.message(), "SESSION_CLOSE_IN_PROGRESS");
        let detail = detail(&status);
        assert_eq!(detail.dolgorae_error_code, "SESSION_CLOSE_IN_PROGRESS");
        assert_eq!(
            detail.action,
            pb::RequiredClientAction::RefreshSnapshot as i32
        );
        assert_eq!(
            detail.retry_classification,
            pb::RetryClassification::Forbidden as i32
        );
        assert_eq!(
            detail.recovery_classification,
            pb::RecoveryClassification::SnapshotRequired as i32
        );
        assert_eq!(detail.run_id.as_deref(), Some(run_id.as_str()));
        assert_eq!(detail.operation_id.as_deref(), Some(operation_id.as_str()));
        let submitting = error_status(&error, "RunService.SubmitTurn");
        assert_eq!(submitting.code(), tonic::Code::FailedPrecondition);
        assert_eq!(self::detail(&submitting), detail);
    }

    #[tokio::test]
    async fn queue_count_bytes_and_stall_bounds_close_only_their_stream() {
        let mut full = StreamQueue::default();
        for _ in 0..STREAM_COUNT {
            assert!(full.push(heartbeat()));
        }
        assert!(!full.push(heartbeat()));
        assert_eq!(
            detail(full.error.as_ref().unwrap()).dolgorae_error_code,
            "SLOW_CONSUMER"
        );
        let mut large = StreamQueue::default();
        let oversized = pb::RunEventEnvelope {
            item: Some(pb::run_event_envelope::Item::Heartbeat(
                pb::RunEventHeartbeat {
                    run_id: "x".repeat(STREAM_BYTES),
                    ..Default::default()
                },
            )),
        };
        assert!(!large.push(oversized));
        assert_eq!(
            large.error.as_ref().unwrap().code(),
            tonic::Code::ResourceExhausted
        );
        let queue = Arc::new(Mutex::new(StreamQueue::default()));
        {
            let mut queue = queue.lock().unwrap();
            queue.push(heartbeat());
            queue.last_delivery = Some(Instant::now() - STREAM_STALL);
        }
        let mut stalled = EventStream { queue };
        assert_eq!(
            detail(&stalled.next().await.unwrap().unwrap_err()).dolgorae_error_code,
            "SLOW_CONSUMER"
        );
        let healthy = Arc::new(Mutex::new(StreamQueue::default()));
        healthy.lock().unwrap().push(heartbeat());
        let mut healthy = EventStream { queue: healthy };
        assert!(healthy.next().await.unwrap().is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn public_v2_watch_admission_and_response_deadlines_have_no_effect() {
        use crate::gateway_v2::ModernGatewayBackend;
        use crate::protocol::public_v2 as modern;
        use modern::observation_service_server::ObservationService;
        fn request() -> Request<modern::WatchRunEventsRequest> {
            let mut request = Request::new(modern::WatchRunEventsRequest {
                context: Some(modern::RequestContext {
                    protocol_version: 2,
                    client_request_id: Uuid::now_v7().to_string(),
                    client_instance_id: "read-timeout".into(),
                }),
                run: Some(modern::RunRef {
                    workspace: Some(modern::WorkspaceRef {
                        absolute_path: "/unused-test-workspace".into(),
                        expected_workspace_id: "a".repeat(64),
                    }),
                    run_id: Uuid::now_v7().to_string(),
                }),
                projection: modern::ProjectionProfile::Minimal as i32,
                projection_version: 1,
                ..Default::default()
            });
            request
                .metadata_mut()
                .insert("grpc-timeout", "50m".parse().unwrap());
            request
        }
        fn assert_timeout(status: Status) {
            assert_eq!(status.code(), tonic::Code::Unavailable);
            let rich = GoogleRpcStatus::decode(status.details()).unwrap();
            assert_eq!(
                rich.details[0].type_url,
                "type.googleapis.com/dolgorae.public.v2.DolgoraeErrorDetail"
            );
            let detail =
                modern::DolgoraeErrorDetail::decode(rich.details[0].value.as_slice()).unwrap();
            assert_eq!(detail.dolgorae_error_code, "TRANSPORT_FAILURE");
            assert_eq!(detail.effect, modern::EffectState::None as i32);
            assert_eq!(
                detail.classification,
                modern::FailureClassification::Rejected as i32
            );
            assert_eq!(
                detail.action,
                modern::RequiredClientAction::ReconnectFromCommittedCursor as i32
            );
            assert_eq!(
                detail.recovery_classification,
                modern::RecoveryClassification::None as i32
            );
            assert_eq!(
                detail.retry_classification,
                modern::RetryClassification::Forbidden as i32
            );
            assert!(detail.operation.is_none() && detail.idempotency_key.is_none());
            assert!(detail.safe_resume_cursor.is_none());
        }
        let (_stop, shutdown) = watch::channel(false);
        let service = GatewayTransport::new(Arc::new(Backend), shutdown);
        let permits = service
            .stream_workers
            .clone()
            .acquire_many_owned(STREAM_WORKERS as u32)
            .await
            .unwrap();
        let status = match ObservationService::watch_run_events(&service, request()).await {
            Err(status) => status,
            Ok(_) => panic!("saturated Watch admission did not expire"),
        };
        assert_timeout(status);
        drop(permits);

        struct BlockedObservation {
            entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            release: Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl GatewayBackend for BlockedObservation {
            fn modern(&self) -> Result<&dyn ModernGatewayBackend, MachineError> {
                Ok(self)
            }
        }
        impl ModernGatewayBackend for BlockedObservation {
            fn home(&self) -> &DolgoraeHome {
                panic!("failed observation has no home access")
            }
            fn instance_id(&self) -> Uuid {
                Uuid::nil()
            }
            fn workspace(
                &self,
                _: &pb::WorkspaceRef,
            ) -> Result<crate::workspace::WorkspaceView, MachineError> {
                panic!("Watch must use workspace observation")
            }
            fn workspace_observed(
                &self,
                _: &pb::WorkspaceRef,
            ) -> Result<crate::workspace::WorkspaceView, MachineError> {
                self.entered
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .send(())
                    .unwrap();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
                Err(transport_error("INTERNAL_ERROR"))
            }
            fn controller(
                &self,
                _: &pb::ControllerCarrierRef,
            ) -> Result<crate::controller::CredentialCarrier, MachineError> {
                panic!("event observation has no Controller read")
            }
            fn context(&self) -> pb::ResponseContext {
                pb::ResponseContext::default()
            }
        }
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let (_stop, shutdown) = watch::channel(false);
        let service = GatewayTransport::new(
            Arc::new(BlockedObservation {
                entered: Mutex::new(Some(entered)),
                release: Mutex::new(wait),
            }),
            shutdown,
        );
        let copy = service.clone();
        let job = tokio::spawn(async move {
            match ObservationService::watch_run_events(&copy, request()).await {
                Err(status) => status,
                Ok(_) => panic!("blocked Watch response did not expire"),
            }
        });
        started.await.unwrap();
        let status = job.await.unwrap();
        // A timed-out read still owns its worker until its physical work returns.
        assert_eq!(
            service.stream_workers.available_permits(),
            STREAM_WORKERS - 1
        );
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while service.stream_workers.available_permits() != STREAM_WORKERS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_timeout(status);
    }

    #[tokio::test]
    async fn public_v2_stream_bounds_and_shutdown_use_v2_details() {
        use crate::protocol::public_v2 as modern;
        let mapped = |status: &Status, code: &str| {
            let rich = GoogleRpcStatus::decode(status.details()).unwrap();
            assert_eq!(
                rich.details[0].type_url,
                "type.googleapis.com/dolgorae.public.v2.DolgoraeErrorDetail"
            );
            assert_eq!(
                modern::DolgoraeErrorDetail::decode(rich.details[0].value.as_slice())
                    .unwrap()
                    .dolgorae_error_code,
                code
            );
        };
        let queue = || StreamQueue {
            map_error: crate::gateway_v2::error_status,
            ..Default::default()
        };
        let mut full = queue();
        for _ in 0..STREAM_COUNT {
            assert!(full.push(heartbeat()));
        }
        assert!(!full.push(heartbeat()));
        mapped(full.error.as_ref().unwrap(), "SLOW_CONSUMER");
        let mut bytes = queue();
        assert!(!bytes.push(pb::RunEventEnvelope {
            item: Some(pb::run_event_envelope::Item::Heartbeat(
                pb::RunEventHeartbeat {
                    run_id: "x".repeat(STREAM_BYTES),
                    ..Default::default()
                }
            ))
        }));
        mapped(bytes.error.as_ref().unwrap(), "SLOW_CONSUMER");
        let stalled = Arc::new(Mutex::new(queue()));
        {
            let mut q = stalled.lock().unwrap();
            q.push(heartbeat());
            q.last_delivery = Some(Instant::now() - STREAM_STALL);
        }
        let mut stream = EventStream { queue: stalled };
        mapped(&stream.next().await.unwrap().unwrap_err(), "SLOW_CONSUMER");
        let healthy = Arc::new(Mutex::new(queue()));
        healthy.lock().unwrap().push(heartbeat());
        let mut stream = EventStream {
            queue: healthy.clone(),
        };
        assert!(stream.next().await.unwrap().is_ok());
        drop(stream);
        assert!(healthy.lock().unwrap().closed);
        let (stop, stopped) = watch::channel(false);
        let queue = Arc::new(Mutex::new(queue()));
        let mut stream = EventStream {
            queue: queue.clone(),
        };
        let task = tokio::spawn(produce_event_pages(
            GatewayTransport::new(Arc::new(Backend), stopped),
            EventRequest::V2(modern::WatchRunEventsRequest::default()),
            EventPage {
                events: vec![],
                next_cursor: "0".into(),
                durable_head_cursor: "0".into(),
                lifecycle: 0,
                terminal: false,
            },
            queue,
        ));
        stop.send(true).unwrap();
        assert!(matches!(
            stream.next().await.unwrap().unwrap().item,
            Some(pb::run_event_envelope::Item::StreamEnd(_))
        ));
        mapped(
            &stream.next().await.unwrap().unwrap_err(),
            "SERVER_SHUTDOWN",
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn a_full_terminal_page_drains_before_stream_end() {
        let (_stop, stopped) = watch::channel(false);
        let transport = GatewayTransport::new(Arc::new(Backend), stopped);
        let queue = Arc::new(Mutex::new(StreamQueue::default()));
        let mut stream = EventStream {
            queue: queue.clone(),
        };
        let request = pb::WatchRunEventsRequest {
            context: context(1),
            ..Default::default()
        };
        let task = tokio::spawn(produce_events(
            transport,
            request,
            EventPage {
                events: (0..STREAM_COUNT).map(|_| heartbeat()).collect(),
                next_cursor: "32".into(),
                durable_head_cursor: "32".into(),
                lifecycle: 0,
                terminal: true,
            },
            queue,
        ));
        for _ in 0..STREAM_COUNT {
            assert!(stream.next().await.unwrap().is_ok());
        }
        let end = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            end.item,
            Some(pb::run_event_envelope::Item::StreamEnd(_))
        ));
        assert!(stream.next().await.is_none());
        task.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_ends_stream_with_typed_server_shutdown() {
        let (stop, stopped) = watch::channel(false);
        let transport = GatewayTransport::new(Arc::new(Backend), stopped);
        let queue = Arc::new(Mutex::new(StreamQueue::default()));
        let mut stream = EventStream {
            queue: queue.clone(),
        };
        let task = tokio::spawn(produce_events(
            transport,
            pb::WatchRunEventsRequest::default(),
            EventPage {
                events: vec![],
                next_cursor: "0".into(),
                durable_head_cursor: "0".into(),
                lifecycle: 0,
                terminal: false,
            },
            queue,
        ));
        stop.send(true).unwrap();
        assert!(matches!(
            stream.next().await.unwrap().unwrap().item,
            Some(pb::run_event_envelope::Item::StreamEnd(_))
        ));
        assert_eq!(
            detail(&stream.next().await.unwrap().unwrap_err()).dolgorae_error_code,
            "SERVER_SHUTDOWN"
        );
        task.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn persistent_accept_error_still_fails_the_server_closed() {
        struct BrokenListener;
        impl AcceptSource for BrokenListener {
            async fn accept(&mut self) -> std::io::Result<tokio::net::UnixStream> {
                Err(std::io::Error::from_raw_os_error(libc::EINVAL))
            }
        }
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            run_server_with_accept(BrokenListener, Arc::new(Backend), std::future::pending()),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.code, "TRANSPORT_FAILURE");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn transient_accept_errors_preserve_existing_http2_connections() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt as _;

        struct InjectedAccept {
            listener: tokio::net::UnixListener,
            errors: tokio::sync::mpsc::Receiver<(i32, tokio::sync::oneshot::Sender<()>)>,
        }
        impl AcceptSource for InjectedAccept {
            async fn accept(&mut self) -> std::io::Result<tokio::net::UnixStream> {
                tokio::select! {
                    Some((code, observed)) = self.errors.recv() => {
                        let _ = observed.send(());
                        Err(std::io::Error::from_raw_os_error(code))
                    },
                    result = self.listener.accept() => result.map(|(stream, _)| stream),
                }
            }
        }
        async fn connect(path: std::path::PathBuf) -> tonic::transport::Channel {
            tonic::transport::Endpoint::try_from("http://[::]:50051")
                .unwrap()
                .connect_with_connector(tower::service_fn(move |_| {
                    let path = path.clone();
                    async move {
                        tokio::net::UnixStream::connect(path)
                            .await
                            .map(hyper_util::rt::TokioIo::new)
                    }
                }))
                .await
                .unwrap()
        }
        async fn capabilities(
            client: &mut pb::runtime_service_client::RuntimeServiceClient<
                tonic::transport::Channel,
            >,
        ) {
            tokio::time::timeout(
                Duration::from_secs(2),
                client.get_capabilities(pb::GetCapabilitiesRequest {
                    context: context(0),
                    minimum_protocol_version: 1,
                    maximum_protocol_version: 1,
                }),
            )
            .await
            .unwrap()
            .unwrap();
        }
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("ga-{}", Uuid::now_v7().simple()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("g.sock");
        let home = DolgoraeHome::from_canonical_home(root.clone()).unwrap();
        let mut socket = GatewaySocket::bind(&home, &path).unwrap();
        let listener = socket.take_listener().unwrap();
        listener.set_nonblocking(true).unwrap();
        let (errors, injected) = tokio::sync::mpsc::channel(1);
        let source = InjectedAccept {
            listener: tokio::net::UnixListener::from_std(listener).unwrap(),
            errors: injected,
        };
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(run_server_with_accept(source, Arc::new(Backend), async {
            let _ = stopped.await;
        }));
        let mut client =
            pb::runtime_service_client::RuntimeServiceClient::new(connect(path.clone()).await);
        capabilities(&mut client).await;
        for code in [libc::ECONNABORTED, libc::EINTR, libc::EMFILE, libc::ENFILE] {
            let (observed, ack) = tokio::sync::oneshot::channel();
            errors.send((code, observed)).await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), ack)
                .await
                .unwrap()
                .unwrap();
            capabilities(&mut client).await;
            assert!(!server.is_finished());
        }
        // Acceptance resumes after the pressure clears, while the original
        // connection continues to serve requests throughout the retries.
        let mut fresh = pb::runtime_service_client::RuntimeServiceClient::new(connect(path).await);
        capabilities(&mut fresh).await;
        capabilities(&mut client).await;
        let (observed, ack) = tokio::sync::oneshot::channel();
        errors.send((libc::EMFILE, observed)).await.unwrap();
        ack.await.unwrap();
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        socket.cleanup().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connection_admission_rejects_the_seventeenth_without_disrupting_existing_clients() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt as _;

        async fn connect(
            path: std::path::PathBuf,
        ) -> Result<tonic::transport::Channel, tonic::transport::Error> {
            tonic::transport::Endpoint::try_from("http://[::]:50051")?
                .connect_with_connector(tower::service_fn(move |_| {
                    let path = path.clone();
                    async move {
                        tokio::net::UnixStream::connect(path)
                            .await
                            .map(hyper_util::rt::TokioIo::new)
                    }
                }))
                .await
        }

        async fn capabilities(
            client: &mut pb::runtime_service_client::RuntimeServiceClient<
                tonic::transport::Channel,
            >,
        ) -> Result<(), tonic::Status> {
            client
                .get_capabilities(pb::GetCapabilitiesRequest {
                    context: context(0),
                    minimum_protocol_version: 1,
                    maximum_protocol_version: 1,
                })
                .await
                .map(|_| ())
        }

        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("ga-cap-{}", Uuid::now_v7().simple()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("g.sock");
        let home = DolgoraeHome::from_canonical_home(root.clone()).unwrap();
        let mut socket = GatewaySocket::bind(&home, &path).unwrap();
        let listener = socket.take_listener().unwrap();
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::UnixListener::from_std(listener).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(run_server_with_accept(listener, Arc::new(Backend), async {
            let _ = stopped.await;
        }));

        let mut clients = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            let channel = connect(path.clone()).await.unwrap();
            let mut client = pb::runtime_service_client::RuntimeServiceClient::new(channel);
            tokio::time::timeout(Duration::from_secs(2), capabilities(&mut client))
                .await
                .unwrap()
                .unwrap();
            clients.push(client);
        }

        let rejected = tokio::time::timeout(Duration::from_secs(2), connect(path.clone())).await;
        let over_cap_failed = match rejected {
            Ok(Ok(channel)) => {
                let mut client = pb::runtime_service_client::RuntimeServiceClient::new(channel);
                !matches!(
                    tokio::time::timeout(Duration::from_millis(500), capabilities(&mut client))
                        .await,
                    Ok(Ok(()))
                )
            }
            Ok(Err(_)) | Err(_) => true,
        };
        assert!(over_cap_failed, "the seventeenth connection was admitted");
        capabilities(&mut clients[0]).await.unwrap();

        drop(clients);
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        socket.cleanup().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn generated_client_uses_real_private_uds_and_receives_shutdown_detail() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt as _;
        struct StreamingBackend;
        impl GatewayBackend for StreamingBackend {
            fn get_capabilities(
                &self,
                _: pb::GetCapabilitiesRequest,
            ) -> Result<pb::GetCapabilitiesResponse, MachineError> {
                Ok(pb::GetCapabilitiesResponse::default())
            }
            fn watch_run_events(
                &self,
                request: pb::WatchRunEventsRequest,
            ) -> Result<EventPage, MachineError> {
                Ok(EventPage {
                    events: vec![],
                    next_cursor: request.after_cursor,
                    durable_head_cursor: "0".into(),
                    lifecycle: 0,
                    terminal: false,
                })
            }
        }
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("gt-{}", Uuid::now_v7().simple()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("g.sock");
        let home = DolgoraeHome::from_canonical_home(root.clone()).unwrap();
        let mut socket = GatewaySocket::bind(&home, &path).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            run_server(&mut socket, Arc::new(StreamingBackend), async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
            socket.cleanup().unwrap();
        });
        let connect_path = path.clone();
        let channel = tonic::transport::Endpoint::try_from("http://[::]:50051")
            .unwrap()
            .connect_with_connector(tower::service_fn(move |_| {
                let path = connect_path.clone();
                async move {
                    tokio::net::UnixStream::connect(path)
                        .await
                        .map(hyper_util::rt::TokioIo::new)
                }
            }))
            .await
            .unwrap();
        let mut runtime = pb::runtime_service_client::RuntimeServiceClient::new(channel.clone());
        runtime
            .get_capabilities(pb::GetCapabilitiesRequest {
                context: context(0),
                minimum_protocol_version: 1,
                maximum_protocol_version: 1,
            })
            .await
            .unwrap();
        let mut runs = pb::run_service_client::RunServiceClient::new(channel.clone());
        let status = runs
            .delete_run(pb::DeleteRunRequest {
                context: context(1),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(
            detail(&status).dolgorae_error_code,
            "CAPABILITY_UNSUPPORTED"
        );
        let mut observations =
            pb::observation_service_client::ObservationServiceClient::new(channel);
        let mut stream = observations
            .watch_run_events(pb::WatchRunEventsRequest {
                context: context(1),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_inner();
        stop.send(()).unwrap();
        let end = stream.message().await.unwrap().unwrap();
        assert!(matches!(
            end.item,
            Some(pb::run_event_envelope::Item::StreamEnd(_))
        ));
        let status = stream.message().await.unwrap_err();
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert_eq!(detail(&status).dolgorae_error_code, "SERVER_SHUTDOWN");
        tokio::time::timeout(Duration::from_secs(6), server)
            .await
            .unwrap()
            .unwrap();
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_http2_stream_does_not_block_unary_calls_on_same_channel() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt as _;
        struct FloodBackend;
        impl GatewayBackend for FloodBackend {
            fn get_capabilities(
                &self,
                _: pb::GetCapabilitiesRequest,
            ) -> Result<pb::GetCapabilitiesResponse, MachineError> {
                Ok(pb::GetCapabilitiesResponse::default())
            }
            fn watch_run_events(
                &self,
                request: pb::WatchRunEventsRequest,
            ) -> Result<EventPage, MachineError> {
                let after = request.after_cursor.parse::<u64>().unwrap_or(0);
                let events = (1..=16)
                    .map(|offset| pb::RunEventEnvelope {
                        item: Some(pb::run_event_envelope::Item::DurableEvent(
                            pb::DurableRunEvent {
                                cursor: (after + offset).to_string(),
                                event: Some(pb::durable_run_event::Event::DiagnosticReported(
                                    pb::DiagnosticReported {
                                        safe_message: "x".repeat(200_000),
                                    },
                                )),
                                ..Default::default()
                            },
                        )),
                    })
                    .collect();
                Ok(EventPage {
                    events,
                    next_cursor: (after + 16).to_string(),
                    durable_head_cursor: "1000000".into(),
                    lifecycle: 0,
                    terminal: false,
                })
            }
        }
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("gs-{}", Uuid::now_v7().simple()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("g.sock");
        let home = DolgoraeHome::from_canonical_home(root.clone()).unwrap();
        let mut socket = GatewaySocket::bind(&home, &path).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            run_server(&mut socket, Arc::new(FloodBackend), async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
            socket.cleanup().unwrap();
        });
        let channel = tonic::transport::Endpoint::try_from("http://[::]:50051")
            .unwrap()
            .connect_with_connector(tower::service_fn(move |_| {
                let path = path.clone();
                async move {
                    tokio::net::UnixStream::connect(path)
                        .await
                        .map(hyper_util::rt::TokioIo::new)
                }
            }))
            .await
            .unwrap();
        let mut observations =
            pb::observation_service_client::ObservationServiceClient::new(channel.clone());
        let mut stream = observations
            .watch_run_events(pb::WatchRunEventsRequest {
                context: context(1),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_inner();
        // Do not consume the data stream while the producer exceeds its queue.
        tokio::time::sleep(Duration::from_millis(700)).await;
        let mut runtime = pb::runtime_service_client::RuntimeServiceClient::new(channel);
        tokio::time::timeout(
            Duration::from_secs(2),
            runtime.get_capabilities(pb::GetCapabilitiesRequest {
                context: context(0),
                minimum_protocol_version: 1,
                maximum_protocol_version: 1,
            }),
        )
        .await
        .unwrap()
        .unwrap();
        let status = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match stream.message().await {
                    Ok(Some(_)) => {}
                    Ok(None) => panic!("slow stream ended without typed error"),
                    Err(status) => break status,
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
        assert_eq!(detail(&status).dolgorae_error_code, "SLOW_CONSUMER");
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(6), server)
            .await
            .unwrap()
            .unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn readiness_write_failure_retires_started_control_endpoint() {
        use std::os::fd::IntoRawFd as _;
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("gr-{}", Uuid::now_v7().simple()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = DolgoraeHome::from_canonical_home(root.clone()).unwrap();
        let path = root.join("g.sock");
        let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(reader);
        let instance = std::cell::Cell::new(None);
        let error = serve(&home, &path, Some(writer.into_raw_fd()), |id| {
            instance.set(Some(id));
            Arc::new(Backend)
        })
        .unwrap_err();
        let instance = instance
            .get()
            .expect("gateway did not reach backend startup before readiness write failure");
        assert_eq!(error.code, "TRANSPORT_FAILURE");
        assert!(!path.exists());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let control = runtime
            .block_on(async { crate::scheduler::ControlPlaneRuntime::start(&home, instance) })
            .expect("readiness failure retained the original control endpoint");
        runtime.block_on(control.shutdown(tokio::time::Instant::now() + DRAIN_BUDGET));
        runtime.shutdown_timeout(Duration::ZERO);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_failure_writes_one_readiness_envelope_and_closes_inherited_fd() {
        use std::io::Read as _;
        use std::os::fd::IntoRawFd as _;
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let home = DolgoraeHome::from_canonical_home(
            std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("gr-{}", Uuid::now_v7().simple())),
        )
        .unwrap();
        let error = serve(
            &home,
            Path::new("relative.sock"),
            Some(writer.into_raw_fd()),
            |_| Arc::new(Backend),
        )
        .unwrap_err();
        assert_eq!(error.code, "RPC_SOCKET_UNSAFE");
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
        let envelope: FailureEnvelope = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(envelope.command, "serve");
        assert_eq!(envelope.error.code, "RPC_SOCKET_UNSAFE");
        assert!(!home.root().exists());
    }

    #[tokio::test]
    async fn replay_flag_uses_the_subscription_captured_head() {
        fn event(cursor: &str) -> pb::RunEventEnvelope {
            pb::RunEventEnvelope {
                item: Some(pb::run_event_envelope::Item::DurableEvent(
                    pb::DurableRunEvent {
                        cursor: cursor.into(),
                        event_id: format!("event-{cursor}"),
                        replay: true,
                        stamp: Some(pb::ProjectionStamp {
                            run_state_revision: cursor.parse().unwrap(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )),
            }
        }
        struct LaterPage;
        impl GatewayBackend for LaterPage {
            fn watch_run_events(
                &self,
                _: pb::WatchRunEventsRequest,
            ) -> Result<EventPage, MachineError> {
                Ok(EventPage {
                    events: vec![event("2")],
                    next_cursor: "2".into(),
                    durable_head_cursor: "2".into(),
                    lifecycle: 0,
                    terminal: true,
                })
            }
        }
        let (_stop, stopped) = watch::channel(false);
        let queue = Arc::new(Mutex::new(StreamQueue::default()));
        let mut stream = EventStream {
            queue: queue.clone(),
        };
        let task = tokio::spawn(produce_events(
            GatewayTransport::new(Arc::new(LaterPage), stopped),
            pb::WatchRunEventsRequest::default(),
            EventPage {
                events: vec![event("1")],
                next_cursor: "1".into(),
                durable_head_cursor: "1".into(),
                lifecycle: 0,
                terminal: false,
            },
            queue,
        ));
        assert_eq!(stream.next().await.unwrap().unwrap(), event("1"));
        let mut live = event("2");
        let Some(pb::run_event_envelope::Item::DurableEvent(ref mut durable)) = live.item else {
            unreachable!()
        };
        durable.replay = false;
        assert_eq!(stream.next().await.unwrap().unwrap(), live);
        assert!(stream.next().await.unwrap().is_ok());
        task.await.unwrap();
    }
}
