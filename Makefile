SHELL := /bin/bash

CARGO ?= cargo
GO ?= go
PYTHON_BIN ?= $(if $(wildcard .venv/bin/python),.venv/bin/python,python3)
DOLGORAE_BIN ?= $(CURDIR)/target/debug/dolgorae
TEST_THREADS ?= 4
BUF_VERSION := 1.69.0
GO_VERSION := go1.26.6

INT_TESTS := \
	--test conformance_contract \
	--test ledger_contract \
	--test run_record_contract \
	--test worker_controller_authority \
	--test worker_turn_drain \
	--test workspace_contract

.PHONY: test test-prepare test-unit test-int test-e2e validate-agent-skills \
	test-live-specialist-review test-live-scoped-specialist-review test-live-access-safety \
	test-live-codex-compatibility test-live-transport-probe test-live-primary-bridge \
	format format-check lint vet architecture \
	aquarium-dev-describe aquarium-dev-build test-aquarium-dev-producer

aquarium-dev-describe:
	@$(PYTHON_BIN) tools/dev_aquarium/producer.py describe

aquarium-dev-build:
	@$(PYTHON_BIN) tools/dev_aquarium/producer.py build

validate-agent-skills:
	$(PYTHON_BIN) tools/validators/validate_agent_skills.py

test-aquarium-dev-producer:
	@$(PYTHON_BIN) tests/e2e/test_dev_aquarium_producer.py

test:
	$(MAKE) test-prepare
	$(MAKE) test-unit
	$(MAKE) test-int
	$(MAKE) test-e2e
	@echo "[test] completed"

test-prepare:
	$(MAKE) format
	$(MAKE) lint
	$(MAKE) vet
	$(MAKE) architecture
	@$(PYTHON_BIN) -c 'import jsonschema, referencing' 2>/dev/null || { \
		echo "Missing Python dependencies. Create .venv and install tools/validation/requirements.txt." >&2; \
		exit 1; \
	}
	@command -v buf >/dev/null 2>&1 || { \
		echo "Missing validation dependency: buf $(BUF_VERSION) is required." >&2; \
		exit 1; \
	}
	@test "$$(buf --version)" = "$(BUF_VERSION)" || { \
		echo "Validation dependency mismatch: expected buf $(BUF_VERSION), got $$(buf --version)." >&2; \
		exit 1; \
	}
	@command -v $(GO) >/dev/null 2>&1 || { \
		echo "Missing validation dependency: Go $(GO_VERSION) is required." >&2; \
		exit 1; \
	}
	@test "$$(GOTOOLCHAIN=local $(GO) env GOVERSION)" = "$(GO_VERSION)" || { \
		echo "Validation dependency mismatch: expected Go $(GO_VERSION), got $$(GOTOOLCHAIN=local $(GO) env GOVERSION)." >&2; \
		exit 1; \
	}
	buf lint docs/protocol
	buf build docs/protocol >/dev/null
	$(PYTHON_BIN) tools/validators/validate_json_schemas.py
	$(PYTHON_BIN) tools/validators/validate_schema_examples.py
	$(PYTHON_BIN) tools/validators/validate_public_descriptor.py
	cd docs/protocol/generated/gul-consumer-v1/go && GOTOOLCHAIN=local $(GO) test ./...
	cd docs/protocol/generated/pre-task-053-low-level/go && GOTOOLCHAIN=local $(GO) test ./...
	$(PYTHON_BIN) tools/validators/validate_markdown.py
	$(MAKE) validate-agent-skills
	$(MAKE) test-aquarium-dev-producer
	git --no-pager diff --check
	@echo "[test-prepare] completed"

format:
	$(CARGO) fmt --all

format-check:
	$(CARGO) fmt --all --check

lint:
	$(CARGO) clippy --locked --all-targets --all-features -- -D warnings

vet:
	$(CARGO) check --locked --all-targets --all-features

architecture:
	$(CARGO) test --locked --test architecture_contract

test-unit:
	$(CARGO) test --locked --lib --bins -- --test-threads=$(TEST_THREADS)
	@echo "[test-unit] completed"

test-int:
	$(CARGO) test --locked $(INT_TESTS) -- --test-threads=$(TEST_THREADS)
	@echo "[test-int] completed"

test-e2e:
	$(CARGO) build --locked --bin dolgorae
	$(CARGO) test --locked --no-run --message-format=json --test gateway_native --test gateway_semantic_native --test gateway_configuration_native --test gateway_interaction_native > target/gateway-native-artifacts.json
	@command -v git >/dev/null 2>&1 || { \
		echo "Missing E2E dependency: Git 2.39 or later is required." >&2; \
		exit 1; \
	}
	@set -eu; \
		test_root="$$(mktemp -d "$${TMPDIR:-/tmp}/dolgorae-e2e.XXXXXX")"; \
		trap 'test_status=$$?; if [ -e "$$test_root.retired" ] || ! mv "$$test_root" "$$test_root.retired"; then echo "[test-e2e] could not retire $$test_root; preserving test data" >&2; test_status=1; elif $(PYTHON_BIN) tests/e2e/orphan_cleanup.py --binary "$(DOLGORAE_BIN)" --owner-root-under "$$test_root"; then rm -rf -- "$$test_root.retired" || test_status=1; else echo "[test-e2e] orphan cleanup failed; preserving $$test_root.retired" >&2; test_status=1; fi; exit "$$test_status"' EXIT; \
		trap 'exit 1' HUP INT TERM; \
		mkdir -p "$$test_root/home" "$$test_root/tmp" "$$test_root/config" "$$test_root/cache"; \
		chmod 700 "$$test_root" "$$test_root/home" "$$test_root/tmp" "$$test_root/config" "$$test_root/cache"; \
		export HOME="$$test_root/home"; \
		export TMPDIR="$$test_root/tmp"; \
		export XDG_CONFIG_HOME="$$test_root/config"; \
		export XDG_CACHE_HOME="$$test_root/cache"; \
		$(PYTHON_BIN) tests/e2e/test_orphan_cli.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_machine_cli.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_socket_ownership.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_gateway_restart.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_slow_consumer_isolation.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_event_revision_action_barrier.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_frozen_consumer_compatibility.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_start_run_allocation_replay.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_run_configuration_restart.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_threadless_first_write_runtime.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_protected_interaction_lost_response.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_interaction_size_and_secret_barrier.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_review_target_cli.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_scoped_specialist_review_cli.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_scoped_specialist_review_failures.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_workspace_cli.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_worker_cli.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_external_engagement_cli.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_profile_cli.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_specialist_review_acceptance.py; \
		$(PYTHON_BIN) tests/e2e/test_codex_compatibility.py; \
		$(PYTHON_BIN) tests/e2e/test_live_transport_probe.py; \
		$(PYTHON_BIN) tests/e2e/test_live_primary_bridge.py
	@echo "[test-e2e] completed"

test-live-transport-probe:
	@test "$${DOLGORAE_RUN_LIVE_TRANSPORT_PROBE:-}" = 1 || { \
		echo "DOLGORAE_RUN_LIVE_TRANSPORT_PROBE=1 is required" >&2; \
		exit 2; \
	}
	$(PYTHON_BIN) tests/e2e/run_live_transport_probe.py

test-live-primary-bridge:
	@test "$${DOLGORAE_RUN_LIVE_PRIMARY_BRIDGE:-}" = 1 || { \
		echo "DOLGORAE_RUN_LIVE_PRIMARY_BRIDGE=1 is required" >&2; \
		exit 2; \
	}
	$(PYTHON_BIN) tests/e2e/run_live_primary_bridge.py \
		--binary "$(DOLGORAE_BIN)" \
		--codex "$${DOLGORAE_CODEX_BIN:-$(HOME)/.local/bin/codex}" \
		--codex-home "$${DOLGORAE_LIVE_CODEX_HOME:-$(HOME)/.codex}"

test-live-specialist-review:
	@test "$${DOLGORAE_RUN_LIVE_SPECIALIST_REVIEW:-}" = 1 || { \
		echo "DOLGORAE_RUN_LIVE_SPECIALIST_REVIEW=1 is required" >&2; \
		exit 2; \
	}
	$(PYTHON_BIN) tests/e2e/run_specialist_review_acceptance.py \
		--binary "$(DOLGORAE_BIN)" \
		--workspace "$(CURDIR)" \
		--profile "$${DOLGORAE_REVIEW_PROFILE:-reviewer}" \
		--codex "$${DOLGORAE_CODEX_BIN:-$(HOME)/.local/bin/codex}" \
		--phase "$${DOLGORAE_ACCEPTANCE_PHASE:-review}"

test-live-scoped-specialist-review:
	@test "$${DOLGORAE_RUN_LIVE_SCOPED_SPECIALIST_REVIEW:-}" = 1 || { \
		echo "DOLGORAE_RUN_LIVE_SCOPED_SPECIALIST_REVIEW=1 is required" >&2; \
		exit 2; \
	}
	@test -n "$${DOLGORAE_LIVE_WORKSPACE:-}" || { \
		echo "DOLGORAE_LIVE_WORKSPACE is required" >&2; \
		exit 2; \
	}
	$(PYTHON_BIN) tests/e2e/run_scoped_specialist_review_acceptance.py \
		--binary "$(DOLGORAE_BIN)" \
		--workspace "$${DOLGORAE_LIVE_WORKSPACE}" \
		--profile "$${DOLGORAE_REVIEW_PROFILE:-reviewer}" \
		--codex "$${DOLGORAE_CODEX_BIN:-$(HOME)/.local/bin/codex}" \
		--target-kind "$${DOLGORAE_REVIEW_TARGET_KIND:-workspace}" \
		$${DOLGORAE_REVIEW_REVISION:+--revision "$${DOLGORAE_REVIEW_REVISION}"}

test-live-access-safety:
	@test "$${DOLGORAE_RUN_LIVE_ACCESS_SAFETY:-}" = 1 || { \
		echo "DOLGORAE_RUN_LIVE_ACCESS_SAFETY=1 is required" >&2; \
		exit 2; \
	}
	$(PYTHON_BIN) tests/e2e/run_access_safety_acceptance.py \
		--codex "$${DOLGORAE_CODEX_BIN:-$(HOME)/.local/bin/codex}"

test-live-codex-compatibility:
	@test "$${DOLGORAE_RUN_LIVE_CODEX_COMPATIBILITY:-}" = 1 || { \
		echo "DOLGORAE_RUN_LIVE_CODEX_COMPATIBILITY=1 is required" >&2; \
		exit 2; \
	}
	$(PYTHON_BIN) tests/e2e/run_codex_compatibility.py \
		--codex "$${DOLGORAE_CODEX_BIN:-$(HOME)/.local/bin/codex}"
