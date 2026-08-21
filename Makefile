SHELL := /bin/bash

CARGO ?= cargo
PYTHON_BIN ?= $(if $(wildcard .venv/bin/python),.venv/bin/python,python3)
DOLGORAE_BIN ?= $(CURDIR)/target/debug/dolgorae
TEST_THREADS ?= 4
BUF_VERSION := 1.66.1

INT_TESTS := \
	--test conformance_contract \
	--test ledger_contract \
	--test run_record_contract \
	--test workspace_contract

.PHONY: test test-prepare test-unit test-int test-e2e \
	format format-check lint vet architecture

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
	buf lint docs/protocol
	buf build docs/protocol >/dev/null
	$(PYTHON_BIN) tools/validators/validate_json_schemas.py
	$(PYTHON_BIN) tools/validators/validate_schema_examples.py
	$(PYTHON_BIN) tools/validators/validate_markdown.py
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
	@command -v git >/dev/null 2>&1 || { \
		echo "Missing E2E dependency: Git 2.39 or later is required." >&2; \
		exit 1; \
	}
	@set -eu; \
		test_root="$$(mktemp -d "$${TMPDIR:-/tmp}/dolgorae-e2e.XXXXXX")"; \
		trap 'rm -rf -- "$$test_root"' EXIT HUP INT TERM; \
		mkdir -p "$$test_root/home" "$$test_root/tmp" "$$test_root/config" "$$test_root/cache"; \
		chmod 700 "$$test_root" "$$test_root/home" "$$test_root/tmp" "$$test_root/config" "$$test_root/cache"; \
		export HOME="$$test_root/home"; \
		export TMPDIR="$$test_root/tmp"; \
		export XDG_CONFIG_HOME="$$test_root/config"; \
		export XDG_CACHE_HOME="$$test_root/cache"; \
		$(PYTHON_BIN) tests/e2e/test_machine_cli.py --binary "$(DOLGORAE_BIN)"; \
		$(PYTHON_BIN) tests/e2e/test_workspace_cli.py --binary "$(DOLGORAE_BIN)"
	@echo "[test-e2e] completed"
