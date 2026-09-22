package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"net"
	"os"
	"strings"
	"time"

	pb "github.com/rootkernel/dolgorae/frozen/gul-consumer-v1/dolgorae/public/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
)

var requiredMethods = []string{
	"ArtifactService.GetArtifact", "ArtifactService.ReadArtifactChunk",
	"ControllerService.VerifyController",
	"InteractionService.GetControllerInteraction", "InteractionService.ListPendingInteractions", "InteractionService.ResolveInteraction",
	"ObservationService.ListRunTimelineItems", "ObservationService.WatchRunEvents",
	"OrchestrationService.GetOrchestratedSession", "OrchestrationService.ListOrchestratedSessionResults",
	"RunService.CloseRun", "RunService.GetRun", "RunService.InterruptTurn", "RunService.ListRuns", "RunService.PauseRun",
	"RunService.ReconcileRun", "RunService.RecoverRun", "RunService.ResumeRun", "RunService.StartRun", "RunService.SubmitTurn",
	"RuntimeService.GetCapabilities", "RuntimeService.GetProfile", "RuntimeService.InspectWorkspace", "RuntimeService.ListProfiles",
	"WriterService.AcquireWriter", "WriterService.GetWorkspaceWriterStatus", "WriterService.ReleaseWriter",
}

const (
	expectedDescriptorSHA256       = "28b132842bbeb48123c2b7cc529de689e6e6286b0783cbde8551d17ba4921ed5"
	expectedInteractionResponse    = uint32(1_048_576)
	expectedInteractionSafePayload = uint32(8_388_608)
	expectedArtifactMaximum        = uint64(33_554_432)
	expectedArtifactChunk          = uint32(1_048_576)
	expectedArtifactInline         = uint32(1_048_576)
)

type commonFlags struct {
	socket       string
	workspace    string
	workspaceID  string
	controller   string
	controllerID string
	runID        string
}

func addCommon(flags *flag.FlagSet, values *commonFlags, includeRun bool) {
	flags.StringVar(&values.socket, "socket", "", "absolute gateway socket")
	flags.StringVar(&values.workspace, "workspace", "", "absolute workspace")
	flags.StringVar(&values.workspaceID, "workspace-id", "", "expected workspace id")
	flags.StringVar(&values.controller, "controller", "", "absolute Controller carrier")
	flags.StringVar(&values.controllerID, "controller-id", "", "expected Controller id")
	if includeRun {
		flags.StringVar(&values.runID, "run-id", "", "root Run id")
	}
}

func (values commonFlags) validate(includeRun bool) {
	required := map[string]string{
		"socket": values.socket, "workspace": values.workspace,
		"workspace-id": values.workspaceID, "controller": values.controller,
		"controller-id": values.controllerID,
	}
	if includeRun {
		required["run-id"] = values.runID
	}
	for name, value := range required {
		if value == "" {
			fatalf("--%s is required", name)
		}
	}
}

func requestContext() *pb.RequestContext {
	return &pb.RequestContext{
		ProtocolVersion:  1,
		ClientRequestId:  fmt.Sprintf("00000000-0000-7000-8000-%012x", time.Now().UnixNano()&0xffffffffffff),
		ClientInstanceId: "task026-provider-acceptance",
	}
}

func (values commonFlags) workspaceRef() *pb.WorkspaceRef {
	return &pb.WorkspaceRef{AbsolutePath: values.workspace, ExpectedWorkspaceId: values.workspaceID}
}

func (values commonFlags) runRef() *pb.RunRef {
	return &pb.RunRef{Workspace: values.workspaceRef(), RunId: values.runID}
}

func (values commonFlags) carrier() *pb.ControllerCarrierRef {
	return &pb.ControllerCarrierRef{
		AbsoluteFilePath: values.controller, ExpectedControllerId: values.controllerID,
		ExpectedControllerGeneration: 1,
	}
}

func dial(socket string) *grpc.ClientConn {
	connection, err := grpc.NewClient(
		"passthrough:///dolgorae",
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithContextDialer(func(ctx context.Context, _ string) (net.Conn, error) {
			return (&net.Dialer{}).DialContext(ctx, "unix", socket)
		}),
	)
	if err != nil {
		fatalf("dial gateway: %v", err)
	}
	return connection
}

func rpcContext() (context.Context, context.CancelFunc) {
	return context.WithTimeout(context.Background(), 90*time.Second)
}

func negotiate(connection *grpc.ClientConn) int {
	ctx, cancel := rpcContext()
	defer cancel()
	capabilities, err := pb.NewRuntimeServiceClient(connection).GetCapabilities(ctx, &pb.GetCapabilitiesRequest{
		Context: &pb.RequestContext{
			ProtocolVersion:  0,
			ClientRequestId:  requestContext().ClientRequestId,
			ClientInstanceId: "task026-provider-acceptance",
		},
		MinimumProtocolVersion: 1,
		MaximumProtocolVersion: 1,
	})
	if err != nil {
		fatalf("GetCapabilities: %v", err)
	}
	protocol := capabilities.GetProtocol()
	if capabilities.GetContext().GetProtocolVersion() != 1 ||
		protocol.GetRpcProtocolVersion() != 1 ||
		protocol.GetMinimumClientProtocolVersion() != 1 ||
		protocol.GetMaximumClientProtocolVersion() != 1 {
		fatalf("provider protocol range is incompatible")
	}
	if capabilities.GetDescriptorSha256() != expectedDescriptorSHA256 {
		fatalf("provider descriptor digest is incompatible")
	}
	advertised := make(map[string]bool, len(capabilities.GetSupportedMethods()))
	for _, method := range capabilities.GetSupportedMethods() {
		advertised[method] = true
	}
	for _, method := range requiredMethods {
		if !advertised[method] {
			fatalf("provider omitted a required consumer method")
		}
	}
	interactions := capabilities.GetInteractions()
	artifacts := capabilities.GetArtifacts()
	if interactions.GetMaximumResponseBytes() != expectedInteractionResponse ||
		interactions.GetMaximumSafePayloadBytes() != expectedInteractionSafePayload ||
		artifacts.GetMaximumArtifactSize() != expectedArtifactMaximum ||
		artifacts.GetMaximumChunkSize() != expectedArtifactChunk ||
		artifacts.GetMaximumInlineResponseBytes() != expectedArtifactInline {
		fatalf("provider safety limits are incompatible")
	}
	return len(requiredMethods)
}

func printJSON(value any) {
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(value); err != nil {
		fatalf("encode result: %v", err)
	}
}

func stringPointer(value string) *string { return &value }

func start(args []string) {
	flags := flag.NewFlagSet("start", flag.ExitOnError)
	var values commonFlags
	addCommon(flags, &values, false)
	profile := flags.String("profile", "", "Profile name")
	key := flags.String("idempotency-key", "", "start key")
	instructions := flags.String("instructions", "", "Primary instructions")
	flags.Parse(args)
	values.validate(false)
	if *profile == "" || *key == "" || *instructions == "" {
		fatalf("--profile, --idempotency-key, and --instructions are required")
	}
	connection := dial(values.socket)
	defer connection.Close()
	negotiate(connection)
	ctx, cancel := rpcContext()
	defer cancel()
	response, err := pb.NewRunServiceClient(connection).StartRun(ctx, &pb.StartRunRequest{
		Context: requestContext(), Workspace: values.workspaceRef(), Controller: values.carrier(),
		IdempotencyKey: *key, ProfileName: *profile,
		ControlMode:       pb.ControlMode_CONTROL_MODE_DIRECT_INTERACTIVE,
		ExecutionLane:     pb.ExecutionLane_EXECUTION_LANE_SHARED_READONLY,
		Purpose:           pb.PurposeKind_PURPOSE_KIND_INTERACTIVE,
		RequiredAssurance: pb.AssuranceLevel_ASSURANCE_LEVEL_BEST_EFFORT_PERSONAL_ALPHA,
		Instructions:      stringPointer(*instructions),
	})
	if err != nil {
		fatalf("StartRun: %v", err)
	}
	printJSON(map[string]any{"run_id": response.GetRun().GetRunId(), "state_revision": response.GetRun().GetStateRevision(), "exact_replay": response.GetExactReplay()})
}

func submit(args []string) {
	flags := flag.NewFlagSet("submit", flag.ExitOnError)
	var values commonFlags
	addCommon(flags, &values, true)
	key := flags.String("idempotency-key", "", "turn key")
	message := flags.String("message", "", "turn message")
	flags.Parse(args)
	values.validate(true)
	if *key == "" || *message == "" {
		fatalf("--idempotency-key and --message are required")
	}
	connection := dial(values.socket)
	defer connection.Close()
	negotiate(connection)
	ctx, cancel := rpcContext()
	defer cancel()
	client := pb.NewRunServiceClient(connection)
	snapshot, err := client.GetRun(ctx, &pb.GetRunRequest{Context: requestContext(), Run: values.runRef()})
	if err != nil {
		fatalf("GetRun before SubmitTurn: %v", err)
	}
	response, err := client.SubmitTurn(ctx, &pb.SubmitTurnRequest{
		Context: requestContext(), Run: values.runRef(), Controller: values.carrier(),
		IdempotencyKey: *key, WriteIntent: pb.WriteIntent_WRITE_INTENT_READ,
		Message: *message, ExpectedStateRevision: snapshot.GetRun().GetStateRevision(),
	})
	if err != nil {
		fatalf("SubmitTurn: %v", err)
	}
	printJSON(map[string]any{"turn_id": response.GetAcceptedTurn().GetTurnId(), "state_revision": response.GetRun().GetStateRevision()})
}

func waitRun(args []string) {
	flags := flag.NewFlagSet("wait", flag.ExitOnError)
	var values commonFlags
	addCommon(flags, &values, true)
	targets := flags.String("states", "idle", "comma-separated lifecycle names")
	timeout := flags.Duration("timeout", 15*time.Minute, "wait timeout")
	flags.Parse(args)
	values.validate(true)
	wanted := map[string]bool{}
	for _, target := range strings.Split(*targets, ",") {
		wanted[strings.TrimSpace(target)] = true
	}
	connection := dial(values.socket)
	defer connection.Close()
	negotiate(connection)
	client := pb.NewRunServiceClient(connection)
	deadline := time.Now().Add(*timeout)
	for {
		ctx, cancel := rpcContext()
		response, err := client.GetRun(ctx, &pb.GetRunRequest{Context: requestContext(), Run: values.runRef()})
		cancel()
		if err != nil {
			fatalf("GetRun while waiting: %v", err)
		}
		name := strings.TrimPrefix(response.GetRun().GetLifecycle().String(), "RUN_LIFECYCLE_")
		name = strings.ToLower(name)
		if wanted[name] {
			printJSON(map[string]any{"lifecycle": name, "state_revision": response.GetRun().GetStateRevision()})
			return
		}
		if time.Now().After(deadline) {
			fatalf("Run remained %s beyond %s", name, timeout.String())
		}
		time.Sleep(250 * time.Millisecond)
	}
}

func approve(args []string) {
	flags := flag.NewFlagSet("approve", flag.ExitOnError)
	var values commonFlags
	addCommon(flags, &values, true)
	key := flags.String("idempotency-key", "", "resolution key")
	flags.Parse(args)
	values.validate(true)
	if *key == "" {
		fatalf("--idempotency-key is required")
	}
	connection := dial(values.socket)
	defer connection.Close()
	negotiate(connection)
	ctx, cancel := rpcContext()
	defer cancel()
	client := pb.NewInteractionServiceClient(connection)
	pending, err := client.ListPendingInteractions(ctx, &pb.ListPendingInteractionsRequest{Context: requestContext(), Run: values.runRef()})
	if err != nil {
		fatalf("ListPendingInteractions: %v", err)
	}
	if len(pending.GetItems()) != 1 {
		fatalf("expected one pending Specialist approval, observed %d", len(pending.GetItems()))
	}
	interactionID := pending.GetItems()[0].GetInteractionId()
	full, err := client.GetControllerInteraction(ctx, &pb.GetControllerInteractionRequest{
		Context: requestContext(), Run: values.runRef(), Controller: values.carrier(), InteractionId: interactionID,
	})
	if err != nil || full.GetInteraction().GetPayload() == nil {
		fatalf("GetControllerInteraction: response=%v error=%v", full, err)
	}
	response := []byte(`{"answers":{"specialist_approval":{"answers":["approve"]}}}`)
	resolved, err := client.ResolveInteraction(ctx, &pb.ResolveInteractionRequest{
		Context: requestContext(), Run: values.runRef(), Controller: values.carrier(),
		InteractionId: interactionID, IdempotencyKey: *key, ResponseJson: response,
	})
	if err != nil {
		fatalf("ResolveInteraction: %v", err)
	}
	printJSON(map[string]any{"status": resolved.GetStatus().String(), "resolution_receipt": resolved.GetResolutionReceipt() != ""})
}

func observe(args []string) {
	flags := flag.NewFlagSet("observe", flag.ExitOnError)
	var values commonFlags
	addCommon(flags, &values, true)
	minimumBytes := flags.Uint64("minimum-result-bytes", 1, "minimum result size")
	flags.Parse(args)
	values.validate(true)
	connection := dial(values.socket)
	defer connection.Close()
	methodCount := negotiate(connection)
	ctx, cancel := rpcContext()
	defer cancel()
	orchestration := pb.NewOrchestrationServiceClient(connection)
	session, err := orchestration.GetOrchestratedSession(ctx, &pb.GetOrchestratedSessionRequest{
		Context: requestContext(), RootRun: values.runRef(), Controller: values.carrier(),
	})
	if err != nil {
		fatalf("GetOrchestratedSession: %v", err)
	}
	results, err := orchestration.ListOrchestratedSessionResults(ctx, &pb.ListOrchestratedSessionResultsRequest{
		Context: requestContext(), RootRun: values.runRef(), Controller: values.carrier(), Limit: 64, ProjectionVersion: 1,
	})
	if err != nil {
		fatalf("ListOrchestratedSessionResults: %v", err)
	}
	if len(results.GetItems()) == 0 {
		fatalf("public result discovery returned no items")
	}
	result := results.GetItems()[len(results.GetItems())-1]
	if result.GetByteLength() < *minimumBytes ||
		result.GetByteLength() > expectedArtifactMaximum ||
		result.GetArtifactOwner().GetRunId() != values.runID {
		fatalf("public result projection is incomplete or too small")
	}
	artifact := result.GetArtifact()
	if artifact == nil || artifact.GetArtifactId() == "" {
		fatalf("public result has no artifact reference")
	}
	artifacts := pb.NewArtifactServiceClient(connection)
	if _, err := artifacts.GetArtifact(ctx, &pb.GetArtifactRequest{Context: requestContext(), Run: values.runRef(), ArtifactId: artifact.GetArtifactId()}); status.Code(err) != codes.PermissionDenied {
		fatalf("controller-free artifact read was not denied: %v", err)
	}
	metadata, err := artifacts.GetArtifact(ctx, &pb.GetArtifactRequest{
		Context: requestContext(), Run: values.runRef(), ArtifactId: artifact.GetArtifactId(), Controller: values.carrier(),
	})
	if err != nil {
		fatalf("GetArtifact: %v", err)
	}
	if metadata.GetArtifact().GetByteLength() != result.GetByteLength() ||
		metadata.GetArtifact().GetByteLength() > expectedArtifactMaximum {
		fatalf("public artifact metadata length is incompatible")
	}
	var downloaded []byte
	for uint64(len(downloaded)) < metadata.GetArtifact().GetByteLength() {
		chunk, err := artifacts.ReadArtifactChunk(ctx, &pb.ReadArtifactChunkRequest{
			Context: requestContext(), Run: values.runRef(), ArtifactId: artifact.GetArtifactId(),
			Offset: uint64(len(downloaded)), Length: 65536, Controller: values.carrier(),
		})
		if err != nil {
			fatalf("ReadArtifactChunk: %v", err)
		}
		data := chunk.GetData()
		nextLength := uint64(len(downloaded)) + uint64(len(data))
		if len(data) == 0 && !chunk.GetEof() ||
			uint64(len(data)) > uint64(expectedArtifactChunk) ||
			nextLength > metadata.GetArtifact().GetByteLength() ||
			nextLength > expectedArtifactMaximum {
			fatalf("public artifact chunk violated negotiated bounds")
		}
		downloaded = append(downloaded, data...)
		if chunk.GetEof() {
			break
		}
	}
	digest := sha256.Sum256(downloaded)
	computed := hex.EncodeToString(digest[:])
	if uint64(len(downloaded)) != result.GetByteLength() || computed != result.GetSha256() || computed != metadata.GetArtifact().GetSha256() {
		fatalf("public artifact length or digest mismatch")
	}
	printJSON(map[string]any{
		"method_count": methodCount, "session_lifecycle": session.GetSession().GetLifecycle().String(),
		"published_result_count": session.GetSession().GetPublishedResultCount(),
		"result_bytes":           len(downloaded), "sha256": computed,
	})
}

func closeRun(args []string) {
	flags := flag.NewFlagSet("close", flag.ExitOnError)
	var values commonFlags
	addCommon(flags, &values, true)
	interrupt := flags.Bool("interrupt", false, "interrupt active work")
	flags.Parse(args)
	values.validate(true)
	connection := dial(values.socket)
	defer connection.Close()
	negotiate(connection)
	ctx, cancel := rpcContext()
	defer cancel()
	client := pb.NewRunServiceClient(connection)
	snapshot, err := client.GetRun(ctx, &pb.GetRunRequest{Context: requestContext(), Run: values.runRef()})
	if err != nil {
		fatalf("GetRun before CloseRun: %v", err)
	}
	closed, err := client.CloseRun(ctx, &pb.CloseRunRequest{
		Context: requestContext(), Run: values.runRef(), Controller: values.carrier(),
		Interrupt: *interrupt, ExpectedStateRevision: snapshot.GetRun().GetStateRevision(),
	})
	if err != nil {
		fatalf("CloseRun: %v", err)
	}
	printJSON(map[string]any{"lifecycle": closed.GetRun().GetLifecycle().String(), "operation_id": closed.GetContext().GetOperationId() != ""})
}

func fatalf(format string, args ...any) {
	fmt.Fprintf(os.Stderr, format+"\n", args...)
	os.Exit(1)
}

func main() {
	if len(os.Args) < 2 {
		fatalf("usage: private-boundary-client <start|submit|wait|approve|observe|close> ...")
	}
	switch os.Args[1] {
	case "start":
		start(os.Args[2:])
	case "submit":
		submit(os.Args[2:])
	case "wait":
		waitRun(os.Args[2:])
	case "approve":
		approve(os.Args[2:])
	case "observe":
		observe(os.Args[2:])
	case "close":
		closeRun(os.Args[2:])
	default:
		fatalf("unknown subcommand %q", os.Args[1])
	}
}
