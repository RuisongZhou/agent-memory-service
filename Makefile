PYTHON ?= .venv/bin/python
CARGO ?= cargo
JOBS ?= 2

.PHONY: setup build build-metal test test-rust selftest smoke fmt check-public
setup:
	python3 scripts/setup_eval.py
build:
	$(CARGO) build --locked -p memory-service --bin agent-memoryd --profile fast -j $(JOBS)
build-metal:
	$(CARGO) build --locked -p memory-service --bin agent-memoryd --profile fast --features metal -j $(JOBS)
test:
	$(PYTHON) -m pytest -q eval/tests
	$(PYTHON) -m unittest discover -s model_validation -p 'test_*.py'
test-rust:
	$(CARGO) test --locked -p memory-rules -p memory-fulltext -p memory-models --profile fast -j $(JOBS)
selftest:
	MEMORY_MOCK_MODELS=1 target/fast/agent-memoryd selftest
smoke:
	$(PYTHON) scripts/http_smoke.py --bin target/fast/agent-memoryd --output results/http-smoke.json
fmt:
	$(CARGO) fmt --all -- --check
check-public:
	$(PYTHON) scripts/check_public.py
