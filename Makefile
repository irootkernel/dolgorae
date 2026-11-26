PYTHON_BIN ?= python3

.PHONY: format-check test-prepare test-unit test-int test-e2e test

format-check:
	cargo fmt --all -- --check

test-prepare:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	$(PYTHON_BIN) tools/check_v2_guards.py

test-unit:
	cargo test --workspace --lib
	$(PYTHON_BIN) -m unittest discover -s tools -p 'test_*.py' -v

test-int:
	cargo test --workspace --tests

test-e2e:
	cargo test --workspace --bins

test: test-prepare test-unit test-int test-e2e
