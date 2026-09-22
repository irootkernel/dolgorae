package dolgoraev1_test

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	pb "github.com/rootkernel/dolgorae/frozen/gul-consumer-v1/dolgorae/public/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
)

var requestSequence atomic.Uint64

func requestContext() *pb.RequestContext {
	sequence := requestSequence.Add(1)
	return &pb.RequestContext{
		ProtocolVersion:  1,
		ClientRequestId:  fmt.Sprintf("00000000-0000-7000-8000-%012x", sequence),
		ClientInstanceId: "frozen-gul-consumer-v1",
	}
}

func repositoryRoot(t *testing.T) string {
	t.Helper()
	if root := os.Getenv("DOLGORAE_REPOSITORY"); root != "" {
		return root
	}
	directory, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	for {
		candidate := filepath.Join(directory, "docs", "protocol", "dolgorae-gul-consumer-v1.fixtures.json")
		if _, err := os.Stat(candidate); err == nil {
			return directory
		}
		parent := filepath.Dir(directory)
		if parent == directory {
			t.Fatal("repository root is unavailable")
		}
		directory = parent
	}
}

func TestFrozenFixturesRemainReadable(t *testing.T) {
	data, err := os.ReadFile(filepath.Join(repositoryRoot(t), "docs", "protocol", "dolgorae-gul-consumer-v1.fixtures.json"))
	if err != nil {
		t.Fatal(err)
	}
	var fixtures struct {
		Positive []struct {
			ID          string          `json:"id"`
			GRPCMessage string          `json:"grpc_message"`
			GRPCJSON    json.RawMessage `json:"grpc_json"`
		} `json:"positive"`
	}
	if err := json.Unmarshal(data, &fixtures); err != nil {
		t.Fatal(err)
	}
	seen := map[string]bool{}
	for _, fixture := range fixtures.Positive {
		seen[fixture.ID] = true
		var message proto.Message
		switch fixture.GRPCMessage {
		case "dolgorae.public.v1.GetOrchestratedSessionResponse":
			message = &pb.GetOrchestratedSessionResponse{}
		case "dolgorae.public.v1.ListOrchestratedSessionResultsResponse":
			message = &pb.ListOrchestratedSessionResultsResponse{}
		default:
			continue
		}
		if err := protojson.Unmarshal(fixture.GRPCJSON, message); err != nil {
			t.Fatalf("fixture %s is unreadable: %v", fixture.ID, err)
		}
	}
	if !seen["session_active"] || !seen["results_page_one"] {
		t.Fatalf("frozen orchestration fixtures are incomplete: %v", seen)
	}
	unknown := []byte(`{"session":{"lifecycle":"ORCHESTRATED_SESSION_LIFECYCLE_FUTURE"}}`)
	if err := protojson.Unmarshal(unknown, &pb.GetOrchestratedSessionResponse{}); err == nil {
		t.Fatal("unknown decision-critical lifecycle did not fail closed")
	}
}

func candidateEnvironment(t *testing.T) (string, *pb.RunRef, *pb.ControllerCarrierRef, string, string) {
	t.Helper()
	socket := os.Getenv("DOLGORAE_FROZEN_SOCKET")
	workspace := os.Getenv("DOLGORAE_FROZEN_WORKSPACE")
	workspaceID := os.Getenv("DOLGORAE_FROZEN_WORKSPACE_ID")
	runID := os.Getenv("DOLGORAE_FROZEN_RUN_ID")
	controller := os.Getenv("DOLGORAE_FROZEN_CONTROLLER")
	controllerID := os.Getenv("DOLGORAE_FROZEN_CONTROLLER_ID")
	descriptorSHA256 := os.Getenv("DOLGORAE_FROZEN_DESCRIPTOR_SHA256")
	phase := os.Getenv("DOLGORAE_FROZEN_PHASE")
	if socket == "" || workspace == "" || workspaceID == "" || runID == "" || controller == "" || controllerID == "" || descriptorSHA256 == "" || phase == "" {
		t.Skip("candidate provider environment is not configured")
	}
	return socket, &pb.RunRef{Workspace: &pb.WorkspaceRef{AbsolutePath: workspace, ExpectedWorkspaceId: workspaceID}, RunId: runID}, &pb.ControllerCarrierRef{AbsoluteFilePath: controller, ExpectedControllerId: controllerID, ExpectedControllerGeneration: 1}, phase, descriptorSHA256
}

func dialCandidate(t *testing.T, socket string) *grpc.ClientConn {
	t.Helper()
	connection, err := grpc.NewClient("passthrough:///dolgorae", grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithContextDialer(func(ctx context.Context, _ string) (net.Conn, error) {
		return (&net.Dialer{}).DialContext(ctx, "unix", socket)
	}))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = connection.Close() })
	return connection
}

func TestFrozenConsumerAgainstCandidate(t *testing.T) {
	socket, run, controller, phase, descriptorSHA256 := candidateEnvironment(t)
	connection := dialCandidate(t, socket)
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	runtimeClient := pb.NewRuntimeServiceClient(connection)
	capabilities, err := runtimeClient.GetCapabilities(ctx, &pb.GetCapabilitiesRequest{Context: &pb.RequestContext{ProtocolVersion: 0, ClientRequestId: requestContext().ClientRequestId, ClientInstanceId: "frozen-gul-consumer-v1"}, MinimumProtocolVersion: 1, MaximumProtocolVersion: 1})
	if err != nil {
		t.Fatal(err)
	}
	if capabilities.GetContext().GetProtocolVersion() != 1 || capabilities.GetDescriptorSha256() != descriptorSHA256 {
		t.Fatalf("candidate identity mismatch: protocol=%d descriptor=%q", capabilities.GetContext().GetProtocolVersion(), capabilities.GetDescriptorSha256())
	}
	required := []string{
		"ArtifactService.GetArtifact", "ArtifactService.ReadArtifactChunk", "ControllerService.VerifyController",
		"InteractionService.GetControllerInteraction", "InteractionService.ListPendingInteractions", "InteractionService.ResolveInteraction",
		"ObservationService.ListRunTimelineItems", "ObservationService.WatchRunEvents",
		"OrchestrationService.GetOrchestratedSession", "OrchestrationService.ListOrchestratedSessionResults",
		"RunService.CloseRun", "RunService.GetRun", "RunService.InterruptTurn", "RunService.ListRuns", "RunService.PauseRun",
		"RunService.ReconcileRun", "RunService.RecoverRun", "RunService.ResumeRun", "RunService.StartRun", "RunService.SubmitTurn",
		"RuntimeService.GetCapabilities", "RuntimeService.GetProfile", "RuntimeService.InspectWorkspace", "RuntimeService.ListProfiles",
		"WriterService.AcquireWriter", "WriterService.GetWorkspaceWriterStatus", "WriterService.ReleaseWriter",
	}
	advertised := append([]string(nil), capabilities.GetSupportedMethods()...)
	sort.Strings(advertised)
	sort.Strings(required)
	for _, method := range required {
		index := sort.SearchStrings(advertised, method)
		if index == len(advertised) || advertised[index] != method {
			t.Fatalf("required method is not advertised: %s", method)
		}
	}

	runs := pb.NewRunServiceClient(connection)
	snapshot, err := runs.GetRun(ctx, &pb.GetRunRequest{Context: requestContext(), Run: run})
	if err != nil {
		t.Fatal(err)
	}
	if snapshot.GetRun().GetRunId() != run.GetRunId() {
		t.Fatalf("wrong run returned: %q", snapshot.GetRun().GetRunId())
	}

	orchestration := pb.NewOrchestrationServiceClient(connection)
	foreign := *controller
	foreign.ExpectedControllerId = "00000000-0000-7000-8000-000000000999"
	if _, err := orchestration.GetOrchestratedSession(ctx, &pb.GetOrchestratedSessionRequest{Context: requestContext(), RootRun: run, Controller: &foreign}); err == nil {
		t.Fatal("foreign controller crossed the protected session boundary")
	}
	session, err := orchestration.GetOrchestratedSession(ctx, &pb.GetOrchestratedSessionRequest{Context: requestContext(), RootRun: run, Controller: controller})
	if err != nil {
		t.Fatal(err)
	}
	results, err := orchestration.ListOrchestratedSessionResults(ctx, &pb.ListOrchestratedSessionResultsRequest{Context: requestContext(), RootRun: run, Controller: controller, Limit: 1, ProjectionVersion: 1})
	if err != nil {
		t.Fatal(err)
	}
	if len(results.GetItems()) != 0 || results.GetSourceRevision() == 0 {
		t.Fatalf("unexpected initial result page: items=%d revision=%d", len(results.GetItems()), results.GetSourceRevision())
	}

	_, err = runs.SetDefaultEffort(ctx, &pb.SetDefaultEffortRequest{Context: requestContext()})
	if err == nil || status.Code(err) != codes.FailedPrecondition {
		t.Fatalf("unadvertised optional method did not fail as a capability: %v", err)
	}

	switch phase {
	case "active":
		accepted, err := runs.SubmitTurn(ctx, &pb.SubmitTurnRequest{Context: requestContext(), Run: run, Controller: controller, IdempotencyKey: "00000000-0000-7000-8000-000000000100", WriteIntent: pb.WriteIntent_WRITE_INTENT_READ, Message: "Frozen consumer sequential input: 안녕하세요", ExpectedStateRevision: snapshot.GetRun().GetStateRevision()})
		if err != nil {
			t.Fatal(err)
		}
		deadline := time.Now().Add(10 * time.Second)
		snapshot, err = runs.GetRun(ctx, &pb.GetRunRequest{Context: requestContext(), Run: run})
		if err != nil {
			t.Fatal(err)
		}
		for snapshot.GetRun().GetLifecycle() != pb.RunLifecycle_RUN_LIFECYCLE_IDLE {
			if time.Now().After(deadline) {
				t.Fatalf("submitted turn did not settle: %v", snapshot.GetRun().GetLifecycle())
			}
			time.Sleep(20 * time.Millisecond)
			snapshot, err = runs.GetRun(ctx, &pb.GetRunRequest{Context: requestContext(), Run: run})
			if err != nil {
				t.Fatal(err)
			}
		}
		if accepted.GetAcceptedTurn().GetTurnId() == "" {
			t.Fatal("accepted turn identity is absent")
		}
		assertTimelineAndEvents(t, ctx, connection, run, controller)
	case "close":
		closed, err := runs.CloseRun(ctx, &pb.CloseRunRequest{Context: requestContext(), Run: run, Controller: controller, ExpectedStateRevision: snapshot.GetRun().GetStateRevision()})
		if err != nil {
			t.Fatal(err)
		}
		if closed.GetRun().GetLifecycle() != pb.RunLifecycle_RUN_LIFECYCLE_CLOSED || closed.GetContext().GetOperationId() == "" {
			t.Fatalf("whole-session close is incomplete: lifecycle=%v operation=%q", closed.GetRun().GetLifecycle(), closed.GetContext().GetOperationId())
		}
	case "recovered":
		if snapshot.GetRun().GetLifecycle() != pb.RunLifecycle_RUN_LIFECYCLE_CLOSED || session.GetSession().GetCloseProgress() != pb.SessionCloseProgress_SESSION_CLOSE_PROGRESS_COMPLETED || session.GetSession().GetCloseOperationId() == "" {
			t.Fatalf("closed session was not retained across restart: run=%v close=%v operation=%q", snapshot.GetRun().GetLifecycle(), session.GetSession().GetCloseProgress(), session.GetSession().GetCloseOperationId())
		}
	default:
		t.Fatalf("unsupported candidate phase %q", phase)
	}
}

func assertTimelineAndEvents(t *testing.T, ctx context.Context, connection *grpc.ClientConn, run *pb.RunRef, controller *pb.ControllerCarrierRef) {
	t.Helper()
	observations := pb.NewObservationServiceClient(connection)
	page, err := observations.ListRunTimelineItems(ctx, &pb.ListRunTimelineItemsRequest{Context: requestContext(), Run: run, Controller: controller, Limit: 1, TimelineVersion: 1})
	if err != nil {
		t.Fatal(err)
	}
	if len(page.GetItems()) != 1 || page.GetCapturedHeadCursor() == "" || page.GetStamp().GetRunStateRevision() == 0 {
		t.Fatalf("timeline page is incomplete: items=%d head=%q", len(page.GetItems()), page.GetCapturedHeadCursor())
	}
	seenInput := page.GetItems()[0].GetType() == pb.TimelineItemType_TIMELINE_ITEM_TYPE_USER_INPUT_ACCEPTED
	cursor := page.GetNextAfterCursor()
	for cursor != "" {
		page, err = observations.ListRunTimelineItems(ctx, &pb.ListRunTimelineItemsRequest{Context: requestContext(), Run: run, Controller: controller, AfterCursor: cursor, Limit: 1, TimelineVersion: 1})
		if err != nil {
			t.Fatal(err)
		}
		for _, item := range page.GetItems() {
			seenInput = seenInput || item.GetType() == pb.TimelineItemType_TIMELINE_ITEM_TYPE_USER_INPUT_ACCEPTED
		}
		cursor = page.GetNextAfterCursor()
	}
	if !seenInput {
		t.Fatal("sequential user input is absent from frozen-client history")
	}
	stream, err := observations.WatchRunEvents(ctx, &pb.WatchRunEventsRequest{Context: requestContext(), Run: run, Projection: pb.ProjectionProfile_PROJECTION_PROFILE_OPERATIONAL, ProjectionVersion: 1})
	if err != nil {
		t.Fatal(err)
	}
	envelope, err := stream.Recv()
	if err != nil {
		t.Fatal(err)
	}
	event := envelope.GetDurableEvent()
	if event == nil || strings.TrimSpace(event.GetCursor()) == "" || event.GetStamp().GetRunStateRevision() == 0 || event.GetEvent() == nil {
		t.Fatalf("event cursor/stamp/variant is incomplete: %#v", envelope)
	}
}
