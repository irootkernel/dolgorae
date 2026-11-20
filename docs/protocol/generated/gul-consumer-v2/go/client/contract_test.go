package client_test

import (
	"context"
	"encoding/json"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	client "github.com/rootkernel/dolgorae/contracts/gul-consumer-v2/client"
	pb "github.com/rootkernel/dolgorae/contracts/gul-consumer-v2/dolgorae/public/v2"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
	"google.golang.org/grpc/test/bufconn"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/reflect/protoregistry"
)

func TestStrictDecisionDecode(t *testing.T) {
	for _, payload := range []string{
		`{"effect":"EFFECT_STATE_FUTURE"}`,
		`{"effect":99}`,
		`{"classification":99}`,
		`{"operationKind":99}`,
		`{"extra":true}`,
		`{"operation":{"callerScope":{"controllerKind":99}}}`,
	} {
		if err := client.Decode([]byte(payload), &pb.DolgoraeErrorDetail{}); err == nil {
			t.Fatalf("unsupported decision detail accepted: %s", payload)
		}
	}
	if err := client.Decode([]byte(`{"effect":"EFFECT_STATE_UNRESOLVED","classification":"FAILURE_CLASSIFICATION_UNKNOWN"}`), &pb.DolgoraeErrorDetail{}); err != nil {
		t.Fatal(err)
	}
}

func TestMutationUncertaintyCannotBecomeRefusal(t *testing.T) {
	for _, detail := range []*pb.DolgoraeErrorDetail{
		nil, {},
		{OperationKind: pb.OperationKind_OPERATION_KIND_START_RUN, Classification: pb.FailureClassification_FAILURE_CLASSIFICATION_REJECTED, Effect: pb.EffectState_EFFECT_STATE_ACCEPTED},
		{OperationKind: pb.OperationKind_OPERATION_KIND_START_RUN, Classification: pb.FailureClassification(99), Effect: pb.EffectState_EFFECT_STATE_NONE},
	} {
		if client.MutationClassification(detail) != pb.FailureClassification_FAILURE_CLASSIFICATION_UNKNOWN {
			t.Fatalf("incomplete or contradictory mutation detail became a known refusal: %v", detail)
		}
	}
	refused := &pb.DolgoraeErrorDetail{DetailVersion: 2, DolgoraeErrorCode: "INVALID_ARGUMENT", Action: pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_FIX_REQUEST, OperationKind: pb.OperationKind_OPERATION_KIND_START_RUN, Classification: pb.FailureClassification_FAILURE_CLASSIFICATION_REJECTED, Effect: pb.EffectState_EFFECT_STATE_NONE}
	if client.MutationClassification(refused) != pb.FailureClassification_FAILURE_CLASSIFICATION_REJECTED {
		t.Fatal("typed no-effect refusal lost")
	}
	for _, corrupt := range []func(*pb.DolgoraeErrorDetail){
		func(d *pb.DolgoraeErrorDetail) { d.DolgoraeErrorCode = "FUTURE_CODE" },
		func(d *pb.DolgoraeErrorDetail) { d.DolgoraeErrorCode = "" },
		func(d *pb.DolgoraeErrorDetail) { d.DetailVersion = 0 },
		func(d *pb.DolgoraeErrorDetail) { d.DetailVersion = 99 },
		func(d *pb.DolgoraeErrorDetail) { d.Action = pb.RequiredClientAction(99) },
		func(d *pb.DolgoraeErrorDetail) { d.Action = pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_UNSPECIFIED },
		func(d *pb.DolgoraeErrorDetail) {
			d.Operation = &pb.OperationRef{OperationKind: pb.OperationKind_OPERATION_KIND_SUBMIT_TURN}
		},
	} {
		detail := proto.Clone(refused).(*pb.DolgoraeErrorDetail)
		corrupt(detail)
		if client.MutationClassification(detail) != pb.FailureClassification_FAILURE_CLASSIFICATION_UNKNOWN {
			t.Fatalf("malformed detail resolved uncertainty: %v", detail)
		}
	}
}

func TestMutationClassificationRequiresCompatibleAction(t *testing.T) {
	for _, classification := range []pb.FailureClassification{
		pb.FailureClassification_FAILURE_CLASSIFICATION_RETRY_SAME_KEY,
		pb.FailureClassification_FAILURE_CLASSIFICATION_RECOVERY_NEEDED,
	} {
		for _, effect := range []pb.EffectState{
			pb.EffectState_EFFECT_STATE_NONE,
			pb.EffectState_EFFECT_STATE_ACCEPTED,
			pb.EffectState_EFFECT_STATE_UNRESOLVED,
		} {
			for value := range pb.RequiredClientAction_name {
				action := pb.RequiredClientAction(value)
				allowed := action == pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_RETRY_EXACT_IDEMPOTENCY_KEY
				if classification == pb.FailureClassification_FAILURE_CLASSIFICATION_RECOVERY_NEEDED {
					allowed = action == pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_RECONCILE_OPERATION ||
						action == pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_RECOVER_RUN ||
						action == pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_RESTORE_STATE ||
						action == pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_RESUME_MAINTENANCE
				}
				detail := &pb.DolgoraeErrorDetail{DetailVersion: 2, DolgoraeErrorCode: "RECOVERY_REQUIRED",
					OperationKind:  pb.OperationKind_OPERATION_KIND_START_RUN,
					Classification: classification, Effect: effect, Action: action}
				expected := pb.FailureClassification_FAILURE_CLASSIFICATION_UNKNOWN
				if allowed {
					expected = classification
				}
				if actual := client.MutationClassification(detail); actual != expected {
					t.Fatalf("classification=%v effect=%v action=%v: got %v, want %v", classification, effect, action, actual, expected)
				}
			}
		}
	}
}

func TestDescriptorDoesNotAdvertiseRuntimeMethods(t *testing.T) {
	capabilities := &pb.GetCapabilitiesResponse{Context: &pb.ResponseContext{ProtocolVersion: 2, ServerInstanceId: "fixture-instance"}, DescriptorSha256: "fixture-digest", SupportedMethods: []string{"RuntimeService.GetCapabilities"}}
	if err := client.RequireMethods(capabilities, "fixture-digest", "RuntimeService.GetCapabilities"); err != nil {
		t.Fatal(err)
	}
	if err := client.RequireMethods(capabilities, "fixture-digest", "RunService.StartRun"); err == nil {
		t.Fatal("published StartRun descriptor activated unavailable method")
	}
	if err := client.RequireMethods(capabilities, "different-descriptor"); err == nil {
		t.Fatal("descriptor skew accepted")
	}
	capabilities.Context.ProtocolVersion = 1
	if err := client.RequireMethods(capabilities, "fixture-digest"); err == nil {
		t.Fatal("v1 contract silently admitted as v2")
	}
}

// Generated service declarations supply no default implementation. This fixture
// checks that their default servers refuse execution; it does not certify the
// production provider's registration or future v2 capability advertisement.
func TestPublishedServicesRemainUnimplementedByDefault(t *testing.T) {
	listener := bufconn.Listen(1024 * 1024)
	server := grpc.NewServer()
	pb.RegisterRuntimeServiceServer(server, &pb.UnimplementedRuntimeServiceServer{})
	pb.RegisterRunServiceServer(server, &pb.UnimplementedRunServiceServer{})
	pb.RegisterOperationServiceServer(server, &pb.UnimplementedOperationServiceServer{})
	pb.RegisterProvisioningServiceServer(server, &pb.UnimplementedProvisioningServiceServer{})
	pb.RegisterSpecialistServiceServer(server, &pb.UnimplementedSpecialistServiceServer{})
	pb.RegisterBrokerServiceServer(server, &pb.UnimplementedBrokerServiceServer{})
	pb.RegisterGroupServiceServer(server, &pb.UnimplementedGroupServiceServer{})
	pb.RegisterMaintenanceServiceServer(server, &pb.UnimplementedMaintenanceServiceServer{})
	go func() { _ = server.Serve(listener) }()
	t.Cleanup(server.Stop)
	connection, err := grpc.NewClient("passthrough:///unimplemented", grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithContextDialer(func(context.Context, string) (net.Conn, error) { return listener.Dial() }), grpc.WithDisableRetry())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = connection.Close() })
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	methods := []struct {
		name     string
		request  proto.Message
		response proto.Message
	}{
		{"RuntimeService/GetCapabilities", &pb.GetCapabilitiesRequest{}, &pb.GetCapabilitiesResponse{}},
		{"RunService/StartRun", &pb.StartRunRequest{}, &pb.StartRunResponse{}},
		{"OperationService/LookupOperation", &pb.LookupOperationRequest{}, &pb.LookupOperationResponse{}},
		{"ProvisioningService/InstallProvisioning", &pb.InstallProvisioningRequest{}, &pb.InstallProvisioningResponse{}},
		{"SpecialistService/HireExternalSpecialist", &pb.HireExternalSpecialistRequest{}, &pb.HireExternalSpecialistResponse{}},
		{"BrokerService/SpawnSpecialist", &pb.SpawnSpecialistRequest{}, &pb.SpawnSpecialistResponse{}},
		{"GroupService/AssignGroup", &pb.AssignGroupRequest{}, &pb.AssignGroupResponse{}},
		{"MaintenanceService/ApplyMaintenance", &pb.ApplyMaintenanceRequest{}, &pb.ApplyMaintenanceResponse{}},
	}
	for _, method := range methods {
		err := connection.Invoke(ctx, "/dolgorae.public.v2."+method.name, method.request, method.response)
		if status.Code(err) != codes.Unimplemented {
			t.Fatalf("published default %s became executable: %v", method.name, err)
		}
	}
}

type responseLossServer struct {
	pb.UnimplementedRunServiceServer
	pb.UnimplementedOperationServiceServer
	starts   atomic.Int32
	submits  atomic.Int32
	lookups  atomic.Int32
	original *pb.OperationRef
}

func (server *responseLossServer) StartRun(_ context.Context, request *pb.StartRunRequest) (*pb.StartRunResponse, error) {
	server.starts.Add(1)
	if request.GetController().GetExpectedControllerGeneration() == 0 || request.GetWorkspace().GetExpectedWorkspaceId() == "" || request.IdempotencyKey == "" {
		return nil, status.Error(codes.InvalidArgument, "missing pre-call operation identity")
	}
	// The fixture credential's normalized principal is already known to both
	// peers. Derive acceptance from the actual request, without a seeded receipt
	// or a generation learned only from the response that is about to be lost.
	server.original = &pb.OperationRef{CallerScope: &pb.CallerScope{
		ControllerKind: pb.ControllerKind_CONTROLLER_KIND_INTERACTIVE_CLIENT,
		PrincipalRef:   "controller", ControllerGeneration: request.Controller.ExpectedControllerGeneration,
		WorkspaceId: request.Workspace.ExpectedWorkspaceId,
		Aggregate:   &pb.CallerScope_ProvisioningScope{ProvisioningScope: "run-start"},
	}, OperationKind: pb.OperationKind_OPERATION_KIND_START_RUN, IdempotencyKey: request.IdempotencyKey}
	return nil, status.Error(codes.Unavailable, "fixture drops reply after acceptance")
}

func (server *responseLossServer) LookupOperation(_ context.Context, request *pb.LookupOperationRequest) (*pb.LookupOperationResponse, error) {
	server.lookups.Add(1)
	if !proto.Equal(request.Operation, server.original) {
		return nil, status.Error(codes.PermissionDenied, "wrong original scope/key/generation")
	}
	subjects := &pb.SubjectIdentities{RunId: proto.String("original-run")}
	if server.original.GetOperationKind() == pb.OperationKind_OPERATION_KIND_SUBMIT_TURN {
		subjects.TurnId = proto.String("original-turn")
	}
	return &pb.LookupOperationResponse{Result: &pb.LookupOperationResponse_Accepted{Accepted: &pb.AcceptedOperation{
		CurrentOutcome: pb.OperationOutcome_OPERATION_OUTCOME_RUNNING,
		Receipt:        &pb.OperationReceipt{Operation: server.original, ReceiptId: "retained-receipt", AcceptedInputDigest: "sha256:" + strings.Repeat("0", 64), Effect: pb.EffectState_EFFECT_STATE_ACCEPTED, Subjects: subjects},
	}}}, nil
}

func (server *responseLossServer) SubmitTurn(_ context.Context, request *pb.SubmitTurnRequest) (*pb.SubmitTurnAccepted, error) {
	server.submits.Add(1)
	if request.GetController().GetExpectedControllerGeneration() == 0 || request.GetRun().GetWorkspace().GetExpectedWorkspaceId() == "" || request.GetRun().GetRunId() == "" || request.IdempotencyKey == "" {
		return nil, status.Error(codes.InvalidArgument, "missing pre-call operation identity")
	}
	server.original = &pb.OperationRef{CallerScope: &pb.CallerScope{
		ControllerKind: pb.ControllerKind_CONTROLLER_KIND_INTERACTIVE_CLIENT,
		PrincipalRef:   "controller", ControllerGeneration: request.Controller.ExpectedControllerGeneration,
		WorkspaceId: request.Run.Workspace.ExpectedWorkspaceId,
		Aggregate:   &pb.CallerScope_RunId{RunId: request.Run.RunId},
	}, OperationKind: pb.OperationKind_OPERATION_KIND_SUBMIT_TURN, IdempotencyKey: request.IdempotencyKey}
	return nil, status.Error(codes.Unavailable, "fixture drops reply after Turn acceptance")
}

func TestUnresolvedLookupPermitsOnlyRecoveryActions(t *testing.T) {
	original := &pb.OperationRef{OperationKind: pb.OperationKind_OPERATION_KIND_SUBMIT_TURN, IdempotencyKey: "original-key"}
	allowed := map[pb.RequiredClientAction]bool{
		pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_RECONCILE_OPERATION: true,
		pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_RECOVER_RUN:         true,
		pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_RESTORE_STATE:       true,
		pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_RESUME_MAINTENANCE:  true,
	}
	for number := range pb.RequiredClientAction_name {
		action := pb.RequiredClientAction(number)
		response := &pb.LookupOperationResponse{Result: &pb.LookupOperationResponse_Unresolved{Unresolved: &pb.UnresolvedOperation{Operation: original, Effect: pb.EffectState_EFFECT_STATE_UNRESOLVED, RequiredAction: action}}}
		if valid := client.ValidateLookup(response, original) == nil; valid != allowed[action] {
			t.Fatalf("unresolved lookup admitted unsafe action %v: valid=%v", action, valid)
		}
	}
}

func TestAcceptedLookupRequiresDigestSubjectAndCurrentOutcome(t *testing.T) {
	original := &pb.OperationRef{OperationKind: pb.OperationKind_OPERATION_KIND_START_RUN, IdempotencyKey: "original-key"}
	valid := &pb.LookupOperationResponse{Result: &pb.LookupOperationResponse_Accepted{Accepted: &pb.AcceptedOperation{
		CurrentOutcome: pb.OperationOutcome_OPERATION_OUTCOME_PENDING,
		Receipt:        &pb.OperationReceipt{Operation: original, ReceiptId: "retained-receipt", AcceptedInputDigest: "sha256:" + strings.Repeat("a", 64), Effect: pb.EffectState_EFFECT_STATE_ACCEPTED, Subjects: &pb.SubjectIdentities{RunId: proto.String("original-run")}},
	}}}
	for _, corrupt := range []func(*pb.AcceptedOperation){
		func(a *pb.AcceptedOperation) { a.Receipt.AcceptedInputDigest = "garbage" },
		func(a *pb.AcceptedOperation) { a.Receipt.AcceptedInputDigest = "sha256:" + strings.Repeat("A", 64) },
		func(a *pb.AcceptedOperation) { a.Receipt.Subjects = &pb.SubjectIdentities{} },
		func(a *pb.AcceptedOperation) {
			a.Receipt.Subjects = &pb.SubjectIdentities{TaskId: proto.String("foreign-kind")}
		},
		func(a *pb.AcceptedOperation) { a.CurrentOutcome = pb.OperationOutcome_OPERATION_OUTCOME_UNSPECIFIED },
		func(a *pb.AcceptedOperation) { a.CurrentOutcome = pb.OperationOutcome(99) },
	} {
		bad := proto.Clone(valid).(*pb.LookupOperationResponse)
		corrupt(bad.GetAccepted())
		if err := client.ValidateLookup(bad, original); err == nil {
			t.Fatalf("incomplete acceptance admitted: %v", bad)
		}
	}
	for _, outcome := range []pb.OperationOutcome{pb.OperationOutcome_OPERATION_OUTCOME_PENDING, pb.OperationOutcome_OPERATION_OUTCOME_RUNNING, pb.OperationOutcome_OPERATION_OUTCOME_COMPLETED, pb.OperationOutcome_OPERATION_OUTCOME_FAILED, pb.OperationOutcome_OPERATION_OUTCOME_CANCELLED, pb.OperationOutcome_OPERATION_OUTCOME_EXPIRED, pb.OperationOutcome_OPERATION_OUTCOME_UNKNOWN} {
		valid.GetAccepted().CurrentOutcome = outcome
		if err := client.ValidateLookup(valid, original); err != nil {
			t.Fatalf("closed current outcome %v refused: %v", outcome, err)
		}
	}
	refused := &pb.LookupOperationResponse{Result: &pb.LookupOperationResponse_NotAccepted{NotAccepted: &pb.NotAcceptedOperation{Operation: original, InputDigest: "garbage", DefinitiveRefusalCode: "INVALID_ARGUMENT"}}}
	if err := client.ValidateLookup(refused, original); err == nil {
		t.Fatal("malformed refusal input digest admitted")
	}
	refused.GetNotAccepted().InputDigest = "sha256:" + strings.Repeat("a", 64)
	if err := client.ValidateLookup(refused, original); err != nil {
		t.Fatal(err)
	}
}

func TestStartKeyClosureRequiresOriginalScopeAndPermanentReceipt(t *testing.T) {
	original := &pb.OperationRef{CallerScope: &pb.CallerScope{
		ControllerGeneration: 3, WorkspaceId: strings.Repeat("a", 64),
		Aggregate: &pb.CallerScope_ProvisioningScope{ProvisioningScope: "run-start"},
	}, OperationKind: pb.OperationKind_OPERATION_KIND_START_RUN, IdempotencyKey: "original-key"}
	valid := &pb.LookupOperationResponse{Result: &pb.LookupOperationResponse_ClosedWithoutAcceptance{ClosedWithoutAcceptance: &pb.ClosedStartOperation{
		Operation: proto.Clone(original).(*pb.OperationRef), ClosureReceiptDigest: "sha256:" + strings.Repeat("b", 64),
		DurableWatermark: 1, OriginalKeyPermanentlyClosed: true,
	}}}
	if err := client.ValidateLookup(valid, original); err != nil {
		t.Fatal(err)
	}
	for _, corrupt := range []func(*pb.ClosedStartOperation){
		func(c *pb.ClosedStartOperation) { c.OriginalKeyPermanentlyClosed = false },
		func(c *pb.ClosedStartOperation) { c.DurableWatermark = 0 },
		func(c *pb.ClosedStartOperation) { c.ClosureReceiptDigest = "garbage" },
		func(c *pb.ClosedStartOperation) { c.Operation.CallerScope.ControllerGeneration++ },
		func(c *pb.ClosedStartOperation) { c.Operation.IdempotencyKey = "different-key" },
	} {
		bad := proto.Clone(valid).(*pb.LookupOperationResponse)
		corrupt(bad.GetClosedWithoutAcceptance())
		if err := client.ValidateLookup(bad, original); err == nil {
			t.Fatal("invalid permanent key closure admitted")
		}
	}
	for _, corrupt := range []func(*pb.OperationRef){
		func(o *pb.OperationRef) { o.OperationKind = pb.OperationKind_OPERATION_KIND_SUBMIT_TURN },
		func(o *pb.OperationRef) { o.CallerScope.Aggregate = &pb.CallerScope_RunId{RunId: "run"} },
	} {
		wrong := proto.Clone(original).(*pb.OperationRef)
		corrupt(wrong)
		response := proto.Clone(valid).(*pb.LookupOperationResponse)
		response.GetClosedWithoutAcceptance().Operation = wrong
		if err := client.ValidateLookup(response, wrong); err == nil {
			t.Fatal("non-provisioning Start closure admitted")
		}
	}
}

func TestUnsetLookupAndUnprovedAbsenceFailClosed(t *testing.T) {
	original := &pb.OperationRef{CallerScope: &pb.CallerScope{ControllerGeneration: 3}, OperationKind: pb.OperationKind_OPERATION_KIND_START_RUN, IdempotencyKey: "original-key"}
	for _, response := range []*pb.LookupOperationResponse{
		nil, {},
		{Result: &pb.LookupOperationResponse_Absent{Absent: &pb.AbsentOperation{Operation: original}}},
		{Result: &pb.LookupOperationResponse_Accepted{Accepted: &pb.AcceptedOperation{}}},
		{Result: &pb.LookupOperationResponse_Unresolved{Unresolved: &pb.UnresolvedOperation{Operation: original, Effect: pb.EffectState(99)}}},
	} {
		if err := client.ValidateLookup(response, original); err == nil {
			t.Fatalf("unproved lookup resolved original uncertainty: %v", response)
		}
	}
	absent := &pb.LookupOperationResponse{Result: &pb.LookupOperationResponse_Absent{Absent: &pb.AbsentOperation{Operation: original, RetainedNamespaceFence: "retained-controller-generation-3", CompleteAuthoritativeLookup: true}}}
	if err := client.ValidateLookup(absent, original); err != nil {
		t.Fatal(err)
	}
	wrong := proto.Clone(original).(*pb.OperationRef)
	wrong.CallerScope.ControllerGeneration++
	if err := client.ValidateLookup(absent, wrong); err == nil {
		t.Fatal("cross-generation absence admitted")
	}
}

// This in-process fixture verifies the consumer's wire behavior only. Actual
// provider acceptance/recovery remains a separate later runtime gate.
func TestLostStartResponseUsesExactOriginalLookup(t *testing.T) {
	request := &pb.StartRunRequest{Context: &pb.RequestContext{ProtocolVersion: 2}, Workspace: &pb.WorkspaceRef{AbsolutePath: "/fixture/workspace", ExpectedWorkspaceId: "workspace"}, Controller: &pb.ControllerCarrierRef{ExpectedControllerId: "controller", ExpectedControllerGeneration: 3}, IdempotencyKey: "original-key"}
	// Retain the credential and request facts before any mutation response.
	original := &pb.OperationRef{CallerScope: &pb.CallerScope{ControllerKind: pb.ControllerKind_CONTROLLER_KIND_INTERACTIVE_CLIENT, PrincipalRef: "controller", ControllerGeneration: request.Controller.ExpectedControllerGeneration, WorkspaceId: request.Workspace.ExpectedWorkspaceId, Aggregate: &pb.CallerScope_ProvisioningScope{ProvisioningScope: "run-start"}}, OperationKind: pb.OperationKind_OPERATION_KIND_START_RUN, IdempotencyKey: request.IdempotencyKey}
	fixture := &responseLossServer{}
	listener := bufconn.Listen(1024 * 1024)
	server := grpc.NewServer()
	pb.RegisterRunServiceServer(server, fixture)
	pb.RegisterOperationServiceServer(server, fixture)
	go func() { _ = server.Serve(listener) }()
	t.Cleanup(server.Stop)
	connection, err := grpc.NewClient("passthrough:///fixture", grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithContextDialer(func(context.Context, string) (net.Conn, error) { return listener.Dial() }), grpc.WithDisableRetry())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = connection.Close() })
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_, err = pb.NewRunServiceClient(connection).StartRun(ctx, request)
	if status.Code(err) != codes.Unavailable {
		t.Fatalf("expected lost response: %v", err)
	}
	lookupContext := &pb.RequestContext{ProtocolVersion: 2, ClientRequestId: "019a0000-0000-7000-8000-000000000001", ClientInstanceId: "fixture-client"}
	result, err := client.ResolveStartRunLoss(ctx, pb.NewOperationServiceClient(connection), lookupContext, &pb.ControllerCarrierRef{ExpectedControllerId: "controller", ExpectedControllerGeneration: 3}, original)
	if err != nil {
		t.Fatal(err)
	}
	if result.GetAccepted().GetReceipt().GetSubjects().GetRunId() != "original-run" || fixture.starts.Load() != 1 || fixture.lookups.Load() != 1 {
		t.Fatalf("original acceptance not recovered without redispatch: %v starts=%d lookups=%d", result, fixture.starts.Load(), fixture.lookups.Load())
	}
	wrong := proto.Clone(original).(*pb.OperationRef)
	wrong.CallerScope.ControllerGeneration++
	if _, err := client.ResolveStartRunLoss(ctx, pb.NewOperationServiceClient(connection), lookupContext, nil, wrong); status.Code(err) != codes.PermissionDenied {
		t.Fatalf("wrong Controller generation accepted: %v", err)
	}
}

func TestLostSubmitResponseRecoversOriginalTurnWithoutRedispatch(t *testing.T) {
	request := &pb.SubmitTurnRequest{Context: &pb.RequestContext{ProtocolVersion: 2}, Controller: &pb.ControllerCarrierRef{ExpectedControllerId: "controller", ExpectedControllerGeneration: 3}, IdempotencyKey: "original-submit-key", Run: &pb.RunRef{Workspace: &pb.WorkspaceRef{AbsolutePath: "/fixture/workspace", ExpectedWorkspaceId: "workspace"}, RunId: "original-run"}, Message: "accepted once"}
	original := &pb.OperationRef{CallerScope: &pb.CallerScope{ControllerKind: pb.ControllerKind_CONTROLLER_KIND_INTERACTIVE_CLIENT, PrincipalRef: "controller", ControllerGeneration: request.Controller.ExpectedControllerGeneration, WorkspaceId: request.Run.Workspace.ExpectedWorkspaceId, Aggregate: &pb.CallerScope_RunId{RunId: request.Run.RunId}}, OperationKind: pb.OperationKind_OPERATION_KIND_SUBMIT_TURN, IdempotencyKey: request.IdempotencyKey}
	fixture := &responseLossServer{}
	listener := bufconn.Listen(1024 * 1024)
	server := grpc.NewServer()
	pb.RegisterRunServiceServer(server, fixture)
	pb.RegisterOperationServiceServer(server, fixture)
	go func() { _ = server.Serve(listener) }()
	t.Cleanup(server.Stop)
	connection, err := grpc.NewClient("passthrough:///submit-fixture", grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithContextDialer(func(context.Context, string) (net.Conn, error) { return listener.Dial() }), grpc.WithDisableRetry())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = connection.Close() })
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_, err = pb.NewRunServiceClient(connection).SubmitTurn(ctx, request)
	if status.Code(err) != codes.Unavailable {
		t.Fatalf("expected lost submit response: %v", err)
	}
	result, err := pb.NewOperationServiceClient(connection).LookupOperation(ctx, &pb.LookupOperationRequest{Operation: original})
	if err != nil {
		t.Fatal(err)
	}
	if err := client.ValidateLookup(result, original); err != nil {
		t.Fatal(err)
	}
	if result.GetAccepted().GetReceipt().GetSubjects().GetTurnId() != "original-turn" || fixture.submits.Load() != 1 || fixture.lookups.Load() != 1 {
		t.Fatal("original Turn not recovered without redispatch")
	}
	result.GetAccepted().GetReceipt().GetSubjects().TurnId = nil
	if err := client.ValidateLookup(result, original); err == nil {
		t.Fatal("accepted SubmitTurn without original Turn identity admitted")
	}
}

func TestPortableFixturesDecodeAgainstGeneratedTypes(t *testing.T) {
	directory, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	var path string
	for {
		path = filepath.Join(directory, "docs/protocol/dolgorae-gul-consumer-v2.fixtures.json")
		if _, err := os.Stat(path); err == nil {
			break
		}
		parent := filepath.Dir(directory)
		if parent == directory {
			t.Fatal("v2 consumer fixtures missing")
		}
		directory = parent
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	type wireFixture struct {
		ID      string          `json:"id"`
		Message string          `json:"grpc_message"`
		JSON    json.RawMessage `json:"grpc_json"`
	}
	var fixtures struct {
		Positive    []wireFixture `json:"positive"`
		Negative    []wireFixture `json:"negative"`
		MethodCases map[string]struct {
			Request     string `json:"request"`
			Response    string `json:"response"`
			Error       string `json:"error"`
			EffectClass string `json:"effect_class"`
		} `json:"method_cases"`
	}
	if err := json.Unmarshal(data, &fixtures); err != nil {
		t.Fatal(err)
	}
	if len(fixtures.Positive) == 0 {
		t.Fatal("no portable v2 fixture messages")
	}
	decoded := make(map[string]proto.Message)
	for _, fixture := range fixtures.Positive {
		if !strings.HasPrefix(fixture.Message, "dolgorae.public.v2.") {
			t.Fatalf("fixture %s claims a different contract", fixture.ID)
		}
		messageType, err := protoregistry.GlobalTypes.FindMessageByName(protoreflect.FullName(fixture.Message))
		if err != nil {
			t.Fatal(err)
		}
		message := messageType.New().Interface()
		if err := client.Decode(fixture.JSON, message); err != nil {
			t.Fatalf("fixture %s: %v", fixture.ID, err)
		}
		if _, exists := decoded[fixture.ID]; exists {
			t.Fatalf("duplicate fixture identity %s", fixture.ID)
		}
		decoded[fixture.ID] = message
		if lookup, ok := message.(*pb.LookupOperationResponse); ok {
			var original *pb.OperationRef
			switch outcome := lookup.Result.(type) {
			case *pb.LookupOperationResponse_Accepted:
				original = outcome.Accepted.GetReceipt().GetOperation()
			case *pb.LookupOperationResponse_NotAccepted:
				original = outcome.NotAccepted.GetOperation()
			case *pb.LookupOperationResponse_Unresolved:
				original = outcome.Unresolved.GetOperation()
			case *pb.LookupOperationResponse_Absent:
				original = outcome.Absent.GetOperation()
			case *pb.LookupOperationResponse_ClosedWithoutAcceptance:
				original = outcome.ClosedWithoutAcceptance.GetOperation()
			}
			if err := client.ValidateLookup(lookup, original); err != nil {
				t.Fatalf("lookup fixture %s: %v", fixture.ID, err)
			}
		}
	}
	install := decoded["install_provisioning_request"].(*pb.InstallProvisioningRequest)
	open := decoded["open_external_engagement_request"].(*pb.OpenExternalEngagementRequest)
	for _, before := range []struct {
		operation  *pb.OperationRef
		controller *pb.ControllerCarrierRef
		workspace  *pb.WorkspaceRef
		kind       pb.OperationKind
		key, scope string
	}{
		{decoded["install_provisioning_response"].(*pb.InstallProvisioningResponse).Receipt.Operation, install.Controller, install.Captured.Workspace, pb.OperationKind_OPERATION_KIND_INSTALL_PROVISIONING, install.IdempotencyKey, "provisioning-install"},
		{decoded["open_external_engagement_response"].(*pb.OpenExternalEngagementResponse).Receipt.Operation, open.Admission.Controller, open.Workspace, pb.OperationKind_OPERATION_KIND_OPEN_EXTERNAL_ENGAGEMENT, open.Admission.IdempotencyKey, "external-engagement-open"},
	} {
		// The allocated policy/engagement ID cannot be an input to recovering
		// the first lost response. Every varying identity fact exists pre-call.
		if before.operation.GetOperationKind() != before.kind || before.operation.GetIdempotencyKey() != before.key ||
			before.operation.GetCallerScope().GetControllerGeneration() != before.controller.GetExpectedControllerGeneration() ||
			before.operation.GetCallerScope().GetWorkspaceId() != before.workspace.GetExpectedWorkspaceId() ||
			before.operation.GetCallerScope().GetProvisioningScope() != before.scope {
			t.Fatalf("pre-allocation lookup requires response-only identity: %v", before.operation)
		}
	}
	imageJSON, err := os.ReadFile(filepath.Join(directory, "docs/protocol/examples/v014-identity-accepted-task.valid.json"))
	if err != nil {
		t.Fatal(err)
	}
	var identity struct {
		Images []struct {
			Content struct {
				ByteLength uint64 `json:"byte_length"`
				SHA256     string `json:"sha256"`
			} `json:"content"`
			MediaType string `json:"media_type"`
			Detail    string `json:"detail"`
		} `json:"images"`
	}
	if err := json.Unmarshal(imageJSON, &identity); err != nil || len(identity.Images) != 1 {
		t.Fatalf("nonempty JSON image identity missing: %v", err)
	}
	image := decoded["accepted_image_high"].(*pb.AcceptedImageRef)
	if identity.Images[0].Detail != "high" || image.Detail != pb.ImageDetail_IMAGE_DETAIL_HIGH || image.MediaType != identity.Images[0].MediaType || image.Content.ByteLength != identity.Images[0].Content.ByteLength || image.Content.Sha256 != identity.Images[0].Content.SHA256 {
		t.Fatal("JSON/protobuf accepted image identity differs")
	}
	plannedMaintenance := decoded["plan_maintenance_response"].(*pb.PlanMaintenanceResponse).Plan
	applyMaintenance := decoded["apply_maintenance_request"].(*pb.ApplyMaintenanceRequest)
	getMaintenance := decoded["get_maintenance_request"].(*pb.GetMaintenanceRequest)
	if !proto.Equal(applyMaintenance.Plan, plannedMaintenance) || !proto.Equal(getMaintenance.Scope, plannedMaintenance.Scope) || getMaintenance.MaintenanceId != plannedMaintenance.PlanId {
		t.Fatal("maintenance getter cannot be constructed from retained pre-call plan")
	}
	for _, owner := range []*pb.MaintenanceProjection{decoded["apply_maintenance_response"].(*pb.ApplyMaintenanceResponse).Maintenance, decoded["get_maintenance_response"].(*pb.GetMaintenanceResponse).Maintenance} {
		if owner.MaintenanceId != plannedMaintenance.PlanId || !proto.Equal(owner.Plan, plannedMaintenance) {
			t.Fatal("maintenance owner lost original plan identity")
		}
	}
	newServices := map[protoreflect.Name]bool{"OperationService": true, "ProvisioningService": true, "SpecialistService": true, "BrokerService": true, "GroupService": true, "MaintenanceService": true}
	seen := 0
	services := pb.File_dolgorae_public_v2_dolgorae_proto.Services()
	for index := 0; index < services.Len(); index++ {
		service := services.Get(index)
		if !newServices[service.Name()] {
			continue
		}
		for index := 0; index < service.Methods().Len(); index++ {
			method := service.Methods().Get(index)
			name := string(service.Name()) + "." + string(method.Name())
			entry, exists := fixtures.MethodCases[name]
			if !exists {
				t.Fatalf("missing request/response/error fixture case: %s", name)
			}
			request, response, failure := decoded[entry.Request], decoded[entry.Response], decoded[entry.Error]
			if request == nil || response == nil || failure == nil || request.ProtoReflect().Descriptor().FullName() != method.Input().FullName() || response.ProtoReflect().Descriptor().FullName() != method.Output().FullName() {
				t.Fatalf("method %s fixture carriers do not match the descriptor", name)
			}
			requestValue := request.ProtoReflect()
			requestContext := requestValue.Get(requestValue.Descriptor().Fields().ByName("context")).Message().Interface().(*pb.RequestContext)
			if requestContext.ProtocolVersion != 2 || requestContext.ClientRequestId == "" || requestContext.ClientInstanceId == "" {
				t.Fatalf("method %s fixture lacks request identity", name)
			}
			responseValue := response.ProtoReflect()
			responseContext := responseValue.Get(responseValue.Descriptor().Fields().ByName("context")).Message().Interface().(*pb.ResponseContext)
			if responseContext.ProtocolVersion != 2 || responseContext.ServerInstanceId == "" {
				t.Fatalf("method %s fixture lacks response identity", name)
			}
			detail, ok := failure.(*pb.DolgoraeErrorDetail)
			if !ok || detail.DetailVersion != 2 || detail.DolgoraeErrorCode == "" || detail.OperationKind == pb.OperationKind_OPERATION_KIND_UNSPECIFIED || detail.Effect == pb.EffectState_EFFECT_STATE_UNSPECIFIED || detail.Classification == pb.FailureClassification_FAILURE_CLASSIFICATION_UNSPECIFIED {
				t.Fatalf("method %s fixture lacks typed error/effect identity", name)
			}
			kindName := strings.ReplaceAll(strings.TrimPrefix(detail.OperationKind.String(), "OPERATION_KIND_"), "_", "")
			reconcilesStart := name == "OperationService.ReconcileStartOperation"
			if (!reconcilesStart && kindName != strings.ToUpper(string(method.Name()))) || (reconcilesStart && (kindName != "STARTRUN" || !proto.Equal(detail.Operation, request.(*pb.ReconcileStartOperationRequest).Operation))) {
				t.Fatalf("method %s error identifies a different operation kind", name)
			}
			if entry.EffectClass == "S" && !reconcilesStart && (detail.Subjects == nil || detail.Operation != nil || detail.IdempotencyKey != nil) {
				t.Fatalf("unkeyed %s error must name subjects without fabricating a key", name)
			}
			seen++
		}
	}
	if seen != 29 || len(fixtures.MethodCases) != seen {
		t.Fatalf("new-method fixture coverage differs: seen=%d cases=%d", seen, len(fixtures.MethodCases))
	}
	if len(fixtures.Negative) == 0 {
		t.Fatal("no portable negative v2 fixture messages")
	}
	for _, fixture := range fixtures.Negative {
		if !strings.HasPrefix(fixture.Message, "dolgorae.public.v2.") {
			t.Fatalf("negative fixture %s claims a different contract", fixture.ID)
		}
		messageType, err := protoregistry.GlobalTypes.FindMessageByName(protoreflect.FullName(fixture.Message))
		if err != nil {
			t.Fatal(err)
		}
		if err := client.Decode(fixture.JSON, messageType.New().Interface()); err == nil {
			t.Fatalf("negative fixture %s was admitted", fixture.ID)
		}
	}
}

// H1 wire fixture only; TASK-085 must implement the durable provider binding.
type maintenanceLossServer struct {
	pb.UnimplementedMaintenanceServiceServer
	applies atomic.Int32
	plan    *pb.MaintenancePlan
	owner   *pb.MaintenanceProjection
}

func (server *maintenanceLossServer) PlanMaintenance(_ context.Context, request *pb.PlanMaintenanceRequest) (*pb.PlanMaintenanceResponse, error) {
	server.plan = &pb.MaintenancePlan{PlanId: "019a0000-0000-7000-8000-000000000018", Scope: proto.Clone(request.Scope).(*pb.MaintenanceScope), Kind: request.Kind, DryRun: true, PlanDigest: "sha256:" + strings.Repeat("a", 64)}
	return &pb.PlanMaintenanceResponse{Plan: proto.Clone(server.plan).(*pb.MaintenancePlan)}, nil
}

func (server *maintenanceLossServer) ApplyMaintenance(_ context.Context, request *pb.ApplyMaintenanceRequest) (*pb.ApplyMaintenanceResponse, error) {
	server.applies.Add(1)
	if !proto.Equal(request.Plan, server.plan) || request.ExpectedGeneration != server.plan.Scope.SelectedGeneration {
		return nil, status.Error(codes.Aborted, "stale plan")
	}
	server.owner = &pb.MaintenanceProjection{MaintenanceId: request.Plan.PlanId, Plan: proto.Clone(request.Plan).(*pb.MaintenancePlan), State: pb.MaintenanceState_MAINTENANCE_STATE_COMPLETED, SelectedGeneration: request.ExpectedGeneration + 1}
	return nil, status.Error(codes.Unavailable, "reply lost after generation switch")
}

func (server *maintenanceLossServer) GetMaintenance(_ context.Context, request *pb.GetMaintenanceRequest) (*pb.GetMaintenanceResponse, error) {
	if request.GetOperator().GetExpectedOperatorId() != "operator" || request.GetOperator().GetExpectedGeneration() != 3 || request.MaintenanceId != server.plan.PlanId || !proto.Equal(request.Scope, server.plan.Scope) {
		return nil, status.Error(codes.PermissionDenied, "wrong original operator/scope/generation/plan")
	}
	if server.owner == nil {
		return nil, status.Error(codes.NotFound, "MAINTENANCE_NOT_APPLIED")
	}
	return &pb.GetMaintenanceResponse{Maintenance: proto.Clone(server.owner).(*pb.MaintenanceProjection)}, nil
}

func TestLostFirstMaintenanceReplyUsesPreCallPlanIdentity(t *testing.T) {
	fixture := &maintenanceLossServer{}
	listener := bufconn.Listen(1024 * 1024)
	server := grpc.NewServer()
	pb.RegisterMaintenanceServiceServer(server, fixture)
	go func() { _ = server.Serve(listener) }()
	t.Cleanup(server.Stop)
	connection, err := grpc.NewClient("passthrough:///maintenance-loss", grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithContextDialer(func(context.Context, string) (net.Conn, error) { return listener.Dial() }), grpc.WithDisableRetry())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = connection.Close() })
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	api := pb.NewMaintenanceServiceClient(connection)
	operator := &pb.OperatorCarrierRef{ExpectedOperatorId: "operator", ExpectedGeneration: 3}
	planned, err := api.PlanMaintenance(ctx, &pb.PlanMaintenanceRequest{Scope: &pb.MaintenanceScope{Workspace: &pb.WorkspaceRef{ExpectedWorkspaceId: "workspace"}, SelectedGeneration: 7}, Operator: operator, Kind: pb.MaintenanceKind_MAINTENANCE_KIND_LEDGER_CHECKPOINT})
	if err != nil {
		t.Fatal(err)
	}
	// Construct the entire getter before Apply; no returned maintenance ID exists.
	lookup := &pb.GetMaintenanceRequest{Scope: proto.Clone(planned.Plan.Scope).(*pb.MaintenanceScope), Operator: operator, MaintenanceId: planned.Plan.PlanId}
	if _, err = api.GetMaintenance(ctx, lookup); status.Code(err) != codes.NotFound {
		t.Fatalf("unapplied plan: %v", err)
	}
	_, err = api.ApplyMaintenance(ctx, &pb.ApplyMaintenanceRequest{Plan: planned.Plan, Operator: operator, ExpectedGeneration: 7})
	if status.Code(err) != codes.Unavailable {
		t.Fatalf("expected lost first reply: %v", err)
	}
	got, err := api.GetMaintenance(ctx, lookup)
	if err != nil || got.Maintenance.MaintenanceId != lookup.MaintenanceId || got.Maintenance.SelectedGeneration != 8 || fixture.applies.Load() != 1 {
		t.Fatalf("original maintenance not recovered: %v %v", got, err)
	}
	for _, change := range []func(*pb.GetMaintenanceRequest){func(r *pb.GetMaintenanceRequest) { r.MaintenanceId = "foreign" }, func(r *pb.GetMaintenanceRequest) { r.Scope.SelectedGeneration++ }, func(r *pb.GetMaintenanceRequest) { r.Operator.ExpectedGeneration++ }, func(r *pb.GetMaintenanceRequest) { r.Scope.Workspace.ExpectedWorkspaceId = "foreign" }} {
		wrong := proto.Clone(lookup).(*pb.GetMaintenanceRequest)
		change(wrong)
		if _, err := api.GetMaintenance(ctx, wrong); status.Code(err) != codes.PermissionDenied {
			t.Fatalf("foreign maintenance lookup admitted: %v", err)
		}
	}
}
