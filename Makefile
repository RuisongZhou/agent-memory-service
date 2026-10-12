PYTHON ?= .venv/bin/python
CARGO ?= cargo
# Empty = cargo default (all cores). CI limits parallelism through CARGO_BUILD_JOBS.
JOBS ?=
JFLAG = $(if $(JOBS),-j $(JOBS))

.PHONY: setup build test test-rust test-service selftest smoke fmt check-public check-arch
setup:
	python3 scripts/setup_eval.py
build:
	$(CARGO) build --locked -p memory-service --bin agent-memoryd --profile fast $(JFLAG)
test:
	$(PYTHON) -m pytest -q eval/tests
test-rust:
	$(CARGO) test --locked -p memory-rules -p memory-fulltext --profile fast $(JFLAG)
test-service:
	$(CARGO) test --locked -p memory-domain -p memory-store -p memory-service --lib --profile fast $(JFLAG)
selftest:
	MEMORY_MOCK_MODELS=1 target/fast/agent-memoryd selftest
smoke:
	$(PYTHON) scripts/http_smoke.py --bin target/fast/agent-memoryd --output results/http-smoke.json
fmt:
	$(CARGO) fmt --all -- --check
check-public:
	$(PYTHON) scripts/check_public.py
check-arch:
	$(PYTHON) scripts/check_architecture.py
