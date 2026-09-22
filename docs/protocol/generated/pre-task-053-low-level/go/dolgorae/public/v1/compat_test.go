package dolgoraev1_test

import (
	"context"
	"fmt"
	"net"
	"os"
	"sort"
	"sync/atomic"
	"testing"
	"time"

	pb "github.com/rootkernel/dolgorae/frozen/pre-task-053-low-level/dolgorae/public/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

var requestSequence atomic.Uint64

func requestContext() *pb.RequestContext {
	return &pb.RequestContext{ProtocolVersion: 1, ClientRequestId: fmt.Sprintf("00000000-0000-7000-9000-%012x", requestSequence.Add(1)), ClientInstanceId: "frozen-pre-task-053-low-level"}
}

func TestPreExtensionClientAgainstCandidate(t *testing.T) {
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
	connection, err := grpc.NewClient("passthrough:///dolgorae", grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithContextDialer(func(ctx context.Context, _ string) (net.Conn, error) {
		return (&net.Dialer{}).DialContext(ctx, "unix", socket)
	}))
	if err != nil {
		t.Fatal(err)
	}
	defer connection.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	run := &pb.RunRef{Workspace: &pb.WorkspaceRef{AbsolutePath: workspace, ExpectedWorkspaceId: workspaceID}, RunId: runID}
	carrier := &pb.ControllerCarrierRef{AbsoluteFilePath: controller, ExpectedControllerId: controllerID, ExpectedControllerGeneration: 1}

	runtimeClient := pb.NewRuntimeServiceClient(connection)
	capabilities, err := runtimeClient.GetCapabilities(ctx, &pb.GetCapabilitiesRequest{Context: &pb.RequestContext{ProtocolVersion: 0, ClientRequestId: requestContext().ClientRequestId, ClientInstanceId: "frozen-pre-task-053-low-level"}, MinimumProtocolVersion: 1, MaximumProtocolVersion: 1})
	if err != nil {
		t.Fatal(err)
	}
	if capabilities.GetDescriptorSha256() != descriptorSHA256 || capabilities.GetContext().GetProtocolVersion() != 1 {
		t.Fatalf("pre-extension client did not negotiate the additive candidate: protocol=%d descriptor=%q", capabilities.GetContext().GetProtocolVersion(), capabilities.GetDescriptorSha256())
	}
	methods := append([]string(nil), capabilities.GetSupportedMethods()...)
	sort.Strings(methods)
	for _, method := range []string{"RunService.GetRun", "RunService.ListRuns", "ObservationService.ListRunTimelineItems", "ObservationService.WatchRunEvents"} {
		index := sort.SearchStrings(methods, method)
		if index == len(methods) || methods[index] != method {
			t.Fatalf("historical method is not advertised: %s", method)
		}
	}

	runs := pb.NewRunServiceClient(connection)
	listed, err := runs.ListRuns(ctx, &pb.ListRunsRequest{Context: requestContext(), Workspace: run.Workspace})
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, item := range listed.GetItems() {
		found = found || item.GetRunId() == runID
	}
	if !found {
		t.Fatalf("candidate run %s is absent from the old list projection", runID)
	}
	snapshot, err := runs.GetRun(ctx, &pb.GetRunRequest{Context: requestContext(), Run: run})
	if err != nil {
		t.Fatal(err)
	}
	if phase == "recovered" && snapshot.GetRun().GetLifecycle() != pb.RunLifecycle_RUN_LIFECYCLE_CLOSED {
		t.Fatalf("old client did not retain closed state across restart: %v", snapshot.GetRun().GetLifecycle())
	}

	observations := pb.NewObservationServiceClient(connection)
	timeline, err := observations.ListRunTimelineItems(ctx, &pb.ListRunTimelineItemsRequest{Context: requestContext(), Run: run, Controller: carrier, Limit: 100, TimelineVersion: 1})
	if err != nil {
		t.Fatal(err)
	}
	if phase != "recovered" && (len(timeline.GetItems()) == 0 || timeline.GetCapturedHeadCursor() == "") {
		t.Fatalf("old client cannot read the new candidate history: items=%d head=%q", len(timeline.GetItems()), timeline.GetCapturedHeadCursor())
	}
	stream, err := observations.WatchRunEvents(ctx, &pb.WatchRunEventsRequest{Context: requestContext(), Run: run, Projection: pb.ProjectionProfile_PROJECTION_PROFILE_OPERATIONAL, ProjectionVersion: 1})
	if err != nil {
		t.Fatal(err)
	}
	envelope, err := stream.Recv()
	if err != nil {
		t.Fatal(err)
	}
	if envelope.GetDurableEvent() == nil || envelope.GetDurableEvent().GetCursor() == "" || envelope.GetDurableEvent().GetStamp().GetRunStateRevision() == 0 {
		t.Fatalf("old client event projection is incomplete: %#v", envelope)
	}
}
