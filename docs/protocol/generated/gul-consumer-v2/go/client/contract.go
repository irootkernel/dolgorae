// Package client provides conservative H1 consumer admission and loss recovery.
// It does not claim that a production public-v2 provider is available.
package client

import (
	"context"
	"fmt"
	"regexp"

	pb "github.com/rootkernel/dolgorae/contracts/gul-consumer-v2/dolgorae/public/v2"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protoreflect"
)

// Decode rejects unknown fields and unknown enum values, including numeric
// values that standard protobuf decoding preserves for forward compatibility.
// Unknown decision variants require a compatible contract, never a default.
func Decode(data []byte, destination proto.Message) error {
	if err := (protojson.UnmarshalOptions{DiscardUnknown: false}).Unmarshal(data, destination); err != nil {
		return err
	}
	return validateEnums(destination.ProtoReflect())
}

func validateEnums(message protoreflect.Message) error {
	var failure error
	message.Range(func(field protoreflect.FieldDescriptor, value protoreflect.Value) bool {
		check := func(item protoreflect.Value) error {
			switch field.Kind() {
			case protoreflect.EnumKind:
				if field.Enum().Values().ByNumber(item.Enum()) == nil {
					return fmt.Errorf("unknown enum %s=%d", field.FullName(), item.Enum())
				}
			case protoreflect.MessageKind, protoreflect.GroupKind:
				return validateEnums(item.Message())
			}
			return nil
		}
		if field.IsList() {
			for i := 0; i < value.List().Len(); i++ {
				if failure = check(value.List().Get(i)); failure != nil {
					return false
				}
			}
		} else {
			failure = check(value)
		}
		return failure == nil
	})
	return failure
}

// RequireMethods checks runtime advertisement separately from descriptor
// existence. Call again after a server-instance change. This grants no account,
// Controller or writer authorization.
func RequireMethods(capabilities *pb.GetCapabilitiesResponse, descriptorSHA256 string, methods ...string) error {
	if capabilities.GetContext().GetProtocolVersion() != 2 || capabilities.GetContext().GetServerInstanceId() == "" ||
		capabilities.GetDescriptorSha256() != descriptorSHA256 {
		return fmt.Errorf("public-v2 capability identity mismatch")
	}
	supported := make(map[string]bool)
	for _, method := range capabilities.GetSupportedMethods() {
		supported[method] = true
	}
	for _, method := range methods {
		if !supported[method] {
			return fmt.Errorf("method unavailable: %s", method)
		}
	}
	return nil
}

// ResolveStartRunLoss performs only the authorized original-key lookup. It
// never reconstructs inputs or resubmits StartRun. The caller retains the exact
// operation scope and generation before the initial request.
func ResolveStartRunLoss(ctx context.Context, operations pb.OperationServiceClient, requestContext *pb.RequestContext, controller *pb.ControllerCarrierRef, original *pb.OperationRef) (*pb.LookupOperationResponse, error) {
	if original == nil || original.GetOperationKind() != pb.OperationKind_OPERATION_KIND_START_RUN ||
		original.GetIdempotencyKey() == "" || original.GetCallerScope() == nil || original.GetNamespaceGeneration() == 0 {
		return nil, fmt.Errorf("exact original StartRun operation identity required")
	}
	if requestContext.GetProtocolVersion() != 2 || requestContext.GetClientRequestId() == "" || requestContext.GetClientInstanceId() == "" {
		return nil, fmt.Errorf("compatible lookup request context required")
	}
	result, err := operations.LookupOperation(ctx, &pb.LookupOperationRequest{
		Context: requestContext, Controller: controller, Operation: original,
	})
	if err != nil {
		return nil, err
	}
	if err := ValidateLookup(result, original); err != nil {
		return nil, err
	}
	return result, nil
}

// ValidateLookup rejects an unset/future union, mismatched original identity or
// unproved absence. Any such response leaves the original operation unresolved.
func ValidateLookup(response *pb.LookupOperationResponse, original *pb.OperationRef) error {
	if response == nil || original == nil {
		return fmt.Errorf("lookup remains unresolved: missing response or original identity")
	}
	if err := validateEnums(response.ProtoReflect()); err != nil {
		return err
	}
	var returned *pb.OperationRef
	switch value := response.GetResult().(type) {
	case *pb.LookupOperationResponse_Accepted:
		receipt := value.Accepted.GetReceipt()
		if receipt.GetReceiptId() == "" || !digestPattern.MatchString(receipt.GetAcceptedInputDigest()) || !acceptedSubject(original.GetOperationKind(), receipt.GetSubjects()) || receipt.GetEffect() != pb.EffectState_EFFECT_STATE_ACCEPTED || value.Accepted.GetCurrentOutcome() == pb.OperationOutcome_OPERATION_OUTCOME_UNSPECIFIED {
			return fmt.Errorf("lookup remains unresolved: incomplete acceptance")
		}
		returned = receipt.GetOperation()
	case *pb.LookupOperationResponse_NotAccepted:
		if !knownErrorCode(value.NotAccepted.GetDefinitiveRefusalCode()) || !digestPattern.MatchString(value.NotAccepted.GetInputDigest()) {
			return fmt.Errorf("lookup remains unresolved: no durable definitive refusal")
		}
		returned = value.NotAccepted.GetOperation()
	case *pb.LookupOperationResponse_Unresolved:
		if value.Unresolved.GetEffect() != pb.EffectState_EFFECT_STATE_UNRESOLVED || !lookupRecoveryAction(value.Unresolved.GetRequiredAction()) {
			return fmt.Errorf("lookup remains unresolved: incomplete recovery condition")
		}
		returned = value.Unresolved.GetOperation()
	case *pb.LookupOperationResponse_Absent:
		if !value.Absent.GetCompleteAuthoritativeLookup() || value.Absent.GetRetainedNamespaceFence() == "" {
			return fmt.Errorf("lookup remains unresolved: absence is not authoritative")
		}
		returned = value.Absent.GetOperation()
	default:
		return fmt.Errorf("lookup remains unresolved: unset or unsupported outcome")
	}
	if !proto.Equal(returned, original) {
		return fmt.Errorf("lookup remains unresolved: original scope/kind/key/generation mismatch")
	}
	return nil
}

var digestPattern = regexp.MustCompile(`^sha256:[0-9a-f]{64}$`)

func lookupRecoveryAction(action pb.RequiredClientAction) bool {
	switch action {
	case pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_RECONCILE_OPERATION,
		pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_RECOVER_RUN,
		pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_RESTORE_STATE,
		pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_RESUME_MAINTENANCE:
		return true
	default:
		return false
	}
}

func acceptedSubject(kind pb.OperationKind, subjects *pb.SubjectIdentities) bool {
	if subjects == nil {
		return false
	}
	switch kind {
	case pb.OperationKind_OPERATION_KIND_START_RUN,
		pb.OperationKind_OPERATION_KIND_FORK_RUN, pb.OperationKind_OPERATION_KIND_CREATE_WRITE_CONTINUATION:
		return subjects.GetRunId() != ""
	case pb.OperationKind_OPERATION_KIND_SUBMIT_TURN:
		return subjects.GetRunId() != "" && subjects.GetTurnId() != ""
	case pb.OperationKind_OPERATION_KIND_RESOLVE_INTERACTION:
		return subjects.GetRunId() != "" && subjects.GetTurnId() != "" && subjects.GetInteractionId() != ""
	case pb.OperationKind_OPERATION_KIND_INSTALL_PROVISIONING:
		return subjects.GetProvisioningRef() != ""
	case pb.OperationKind_OPERATION_KIND_OPEN_EXTERNAL_ENGAGEMENT:
		return subjects.GetEngagementId() != ""
	case pb.OperationKind_OPERATION_KIND_HIRE_EXTERNAL_SPECIALIST, pb.OperationKind_OPERATION_KIND_SPAWN_SPECIALIST:
		return subjects.GetMemberId() != ""
	case pb.OperationKind_OPERATION_KIND_ASSIGN_EXTERNAL_SPECIALIST_TASK, pb.OperationKind_OPERATION_KIND_ASSIGN_SPECIALIST_TASK, pb.OperationKind_OPERATION_KIND_ASSIGN_SYNTHESIS:
		return subjects.GetTaskId() != ""
	case pb.OperationKind_OPERATION_KIND_ASSIGN_GROUP:
		return subjects.GetGroupId() != ""
	case pb.OperationKind_OPERATION_KIND_MERGE_GROUP_RESULTS:
		return subjects.GetMergeId() != ""
	default:
		return false
	}
}

// MutationClassification refuses to infer an effect from status text or an
// incomplete typed detail. A malformed detail remains Unknown.
func MutationClassification(detail *pb.DolgoraeErrorDetail) pb.FailureClassification {
	if detail == nil || detail.GetDetailVersion() != 2 || !knownErrorCode(detail.GetDolgoraeErrorCode()) ||
		detail.GetOperationKind() == pb.OperationKind_OPERATION_KIND_UNSPECIFIED ||
		pb.OperationKind_name[int32(detail.GetOperationKind())] == "" ||
		detail.GetAction() == pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_UNSPECIFIED || validateEnums(detail.ProtoReflect()) != nil {
		return pb.FailureClassification_FAILURE_CLASSIFICATION_UNKNOWN
	}
	if operation := detail.GetOperation(); operation != nil &&
		(operation.GetOperationKind() != detail.GetOperationKind() || operation.GetIdempotencyKey() == "" ||
			operation.GetCallerScope() == nil || operation.GetNamespaceGeneration() == 0 ||
			(detail.IdempotencyKey != nil && detail.GetIdempotencyKey() != operation.GetIdempotencyKey())) {
		return pb.FailureClassification_FAILURE_CLASSIFICATION_UNKNOWN
	}
	switch detail.GetClassification() {
	case pb.FailureClassification_FAILURE_CLASSIFICATION_REJECTED:
		if detail.GetEffect() == pb.EffectState_EFFECT_STATE_NONE {
			return detail.GetClassification()
		}
	case pb.FailureClassification_FAILURE_CLASSIFICATION_RETRY_SAME_KEY:
		if detail.GetAction() != pb.RequiredClientAction_REQUIRED_CLIENT_ACTION_RETRY_EXACT_IDEMPOTENCY_KEY {
			return pb.FailureClassification_FAILURE_CLASSIFICATION_UNKNOWN
		}
		if detail.GetEffect() == pb.EffectState_EFFECT_STATE_NONE || detail.GetEffect() == pb.EffectState_EFFECT_STATE_ACCEPTED ||
			detail.GetEffect() == pb.EffectState_EFFECT_STATE_UNRESOLVED {
			return detail.GetClassification()
		}
	case pb.FailureClassification_FAILURE_CLASSIFICATION_RECOVERY_NEEDED:
		if !lookupRecoveryAction(detail.GetAction()) {
			return pb.FailureClassification_FAILURE_CLASSIFICATION_UNKNOWN
		}
		if detail.GetEffect() == pb.EffectState_EFFECT_STATE_NONE || detail.GetEffect() == pb.EffectState_EFFECT_STATE_ACCEPTED ||
			detail.GetEffect() == pb.EffectState_EFFECT_STATE_UNRESOLVED {
			return detail.GetClassification()
		}
	}
	return pb.FailureClassification_FAILURE_CLASSIFICATION_UNKNOWN
}
