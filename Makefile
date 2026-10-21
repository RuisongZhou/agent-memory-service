PYTHON ?= .venv/bin/python
CARGO ?= cargo
# Empty = cargo default (all cores). CI limits parallelism through CARGO_BUILD_JOBS.
JOBS ?=
JFLAG = $(if $(JOBS),-j $(JOBS))

.PHONY: setup build test test-rust test-service selftest smoke install-check fmt check-public check-arch
setup:
	python3 eval/setup_eval.py
build:
	$(CARGO) build --locked -p memory-service --bin agent-memoryd --profile fast $(JFLAG)
test:
	$(PYTHON) -m pytest -q eval/tests
test-rust:
	$(CARGO) test --locked -p memory-rules -p memory-fulltext -p memory-config --profile fast $(JFLAG)
test-service:
	$(CARGO) test --locked -p memory-domain -p memory-store -p memory-engine -p memory-algo-mem0 -p memory-service --lib --profile fast $(JFLAG)
selftest:
	MEMORY_MOCK_MODELS=1 target/fast/agent-memoryd selftest
smoke:
	$(PYTHON) scripts/http_smoke.py --bin target/fast/agent-memoryd --output results/http-smoke.json
	$(PYTHON) scripts/mem0_http_smoke.py --bin target/fast/agent-memoryd --output results/mem0-http-smoke.json
	$(PYTHON) scripts/mem0_history_http_smoke.py --bin target/fast/agent-memoryd --output results/mem0-history-http-smoke.json
	$(PYTHON) scripts/mem0_hybrid_http_smoke.py --bin target/fast/agent-memoryd --output results/mem0-hybrid-http-smoke.json
# One-command install for every platform (with capture and recall where it has hooks) into a temporary home, verified against a mock daemon.
install-check:
	tmp=$$(mktemp -d) && mkdir "$$tmp/project" && \
	  opts="--apply --non-interactive --binary target/fast/agent-memoryd" && \
	  bash scripts/install.sh --host codex --home "$$tmp" $$opts --project install-check --recall && \
	  bash scripts/install.sh --host workbuddy --home "$$tmp" $$opts --project install-check --recall && \
	  bash scripts/install.sh --host claude-code --home "$$tmp" --workspace "$$tmp/project" $$opts --project install-check --recall && \
	  bash scripts/install.sh --host deepseek-harness --home "$$tmp" $$opts; \
	  status=$$?; rm -rf "$$tmp"; exit $$status
fmt:
	$(CARGO) fmt --all -- --check
check-public:
	$(PYTHON) scripts/check_public.py
check-arch:
	$(PYTHON) scripts/check_architecture.py
